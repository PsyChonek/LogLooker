import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const script = fileURLToPath(
  new URL('../scripts/remove-winget-installer-locale.ps1', import.meta.url),
);
const manifest = new URL('../winget/PsyChonek.LogLooker.installer.yaml', import.meta.url);

test('removes installer locale while preserving installer identity and metadata locales', () => {
  const directory = mkdtempSync(join(tmpdir(), 'loglooker-winget-locale-'));
  const path = join(directory, 'PsyChonek.LogLooker.installer.yaml');
  const base = readFileSync(manifest, 'utf8');
  const run = () =>
    spawnSync('pwsh', ['-NoProfile', '-File', script, '-Path', path], { encoding: 'utf8' });
  try {
    for (const input of [
      base.replace('Platform:', 'InstallerLocale: en-US\nPlatform:'),
      base.replace(/(- Architecture: [^\r\n]+)(\r?\n)/g, '$1$2    InstallerLocale: en-US$2'),
      base,
    ]) {
      writeFileSync(path, input);
      const result = run();
      assert.equal(result.status, 0, result.stderr);
      assert.equal(readFileSync(path, 'utf8'), base);
    }

    writeFileSync(
      path,
      'Installers:\n- InstallerLocale: en-US\n  Architecture: x64\nManifestType: installer\n',
    );
    assert.equal(run().status, 0);
    assert.equal(
      readFileSync(path, 'utf8'),
      'Installers:\n-\n  Architecture: x64\nManifestType: installer\n',
    );

    for (const metadata of [
      'PackageLocale: en-US\nManifestType: defaultLocale\n',
      'DefaultLocale: en-US\nManifestType: version\n',
    ]) {
      writeFileSync(path, metadata);
      assert.notEqual(run().status, 0);
      assert.equal(readFileSync(path, 'utf8'), metadata);
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
