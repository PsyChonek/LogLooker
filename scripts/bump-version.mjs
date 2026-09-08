import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

// Plan every write first so malformed metadata cannot leave a partial bump.
export function planVersionBump(root, bump) {
  if (!['major', 'minor', 'patch'].includes(bump)) {
    throw new Error('Version bump must be major, minor or patch');
  }
  const read = (path) => readFileSync(resolve(root, path), 'utf8');
  const pkg = JSON.parse(read('package.json'));
  if (!/^\d+\.\d+\.\d+$/.test(pkg.version)) {
    throw new Error(`Expected a stable package version, got ${pkg.version}`);
  }
  const parts = pkg.version.split('.').map(Number);
  const index = ['major', 'minor', 'patch'].indexOf(bump);
  parts[index]++;
  parts.fill(0, index + 1);
  const version = parts.join('.');
  const files = new Map();
  const json = (path, transform) => {
    const value = JSON.parse(read(path));
    transform(value);
    files.set(path, JSON.stringify(value, null, 2) + '\n');
  };
  json('package.json', (value) => (value.version = version));
  json('package-lock.json', (value) => {
    value.version = version;
    value.packages[''].version = version;
  });
  json('src-tauri/tauri.conf.json', (value) => (value.version = version));
  const cargo = read('src-tauri/Cargo.toml');
  const packageSection = cargo.match(/^\[package\]\r?\n[\s\S]*?(?=^\[|(?![\s\S]))/m)?.[0];
  if (!packageSection || !/^version = "[^"]+"/m.test(packageSection)) {
    throw new Error('Cargo.toml has no package version');
  }
  files.set(
    'src-tauri/Cargo.toml',
    cargo.replace(
      packageSection,
      packageSection.replace(/^version = "[^"]+"/m, `version = "${version}"`),
    ),
  );
  const cargoName = packageSection.match(/^name = "([^"]+)"/m)?.[1];
  const lock = read('src-tauri/Cargo.lock');
  let found = false;
  const nextLock = lock.replace(
    /^\[\[package\]\]\r?\n[\s\S]*?(?=^\[\[package\]\]|(?![\s\S]))/gm,
    (section) => {
      if (section.match(/^name = "([^"]+)"/m)?.[1] !== cargoName) return section;
      if (!/^version = "[^"]+"/m.test(section))
        throw new Error('Cargo.lock package has no version');
      found = true;
      return section.replace(/^version = "[^"]+"/m, `version = "${version}"`);
    },
  );
  if (!found) throw new Error('Cargo.lock does not contain the application package');
  files.set('src-tauri/Cargo.lock', nextLock);
  return { previous: pkg.version, version, files };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const [bump, ...flags] = process.argv.slice(2);
    if (flags.some((flag) => flag !== '--dry-run')) throw new Error('Unknown argument');
    const root = fileURLToPath(new URL('../', import.meta.url));
    const { version, files } = planVersionBump(root, bump);
    if (!flags.includes('--dry-run')) {
      for (const [path, contents] of files) writeFileSync(resolve(root, path), contents);
    }
    process.stdout.write(`${version}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
