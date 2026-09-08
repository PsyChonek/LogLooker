import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { planVersionBump } from '../scripts/bump-version.mjs';

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'loglooker-version-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, 'src-tauri'));
  const files = {
    'package.json': JSON.stringify({ name: 'log-looker', version: '1.2.9' }),
    'package-lock.json': JSON.stringify({
      version: '1.0.0',
      packages: { '': { version: '1.0.0' }, dependency: { version: '9.9.9' } },
    }),
    'src-tauri/tauri.conf.json': JSON.stringify({ version: '1.2.8' }),
    'src-tauri/Cargo.toml':
      '[package]\nname = "log-looker"\nversion = "0.1.0"\n\n[dependencies]\nserde = "1"\n',
    'src-tauri/Cargo.lock':
      'version = 4\n\n[[package]]\nname = "dependency"\nversion = "9.9.9"\n\n[[package]]\nname = "log-looker"\nversion = "0.1.0"\ndependencies = ["dependency"]\n',
  };
  for (const [path, contents] of Object.entries(files)) writeFileSync(join(root, path), contents);
  return { root, files };
}

for (const [bump, expected] of [
  ['patch', '1.2.10'],
  ['minor', '1.3.0'],
  ['major', '2.0.0'],
]) {
  test(`${bump} release synchronizes all metadata without changing dependency versions`, (t) => {
    const { root, files: original } = fixture(t);
    const { version, files } = planVersionBump(root, bump);
    assert.equal(version, expected);
    assert.equal(files.size, 5);
    for (const path of ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json']) {
      assert.equal(JSON.parse(files.get(path)).version, expected);
    }
    const lock = JSON.parse(files.get('package-lock.json'));
    assert.equal(lock.packages[''].version, expected);
    assert.equal(lock.packages.dependency.version, '9.9.9');
    assert.ok(files.get('src-tauri/Cargo.toml').includes(`version = "${expected}"`));
    assert.ok(
      files.get('src-tauri/Cargo.lock').includes(`name = "log-looker"\nversion = "${expected}"`),
    );
    assert.ok(files.get('src-tauri/Cargo.lock').includes('name = "dependency"\nversion = "9.9.9"'));
    for (const [path, contents] of Object.entries(original)) {
      assert.equal(
        readFileSync(join(root, path), 'utf8'),
        contents,
        'planning must not write files',
      );
    }
  });
}

test('invalid input or missing Cargo package fails before any version is written', (t) => {
  const { root, files } = fixture(t);
  assert.throws(() => planVersionBump(root, 'oops'), /major, minor or patch/);
  writeFileSync(join(root, 'src-tauri/Cargo.lock'), 'version = 4\n');
  assert.throws(() => planVersionBump(root, 'patch'), /application package/);
  assert.equal(readFileSync(join(root, 'package.json'), 'utf8'), files['package.json']);
});
