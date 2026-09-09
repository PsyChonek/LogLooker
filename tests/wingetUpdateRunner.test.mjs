import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const runner = readFileSync(
  new URL('../src-tauri/src/commands/winget-update.ps1', import.meta.url),
  'utf8',
);

function runHelper({ exits = true, exitCode = 0 } = {}) {
  // Shadow both external operations. These tests never run winget or close apps.
  const mocks = `
    $script:waited = $false
    function Get-Process {
      $process = New-Object PSObject
      $process | Add-Member ScriptMethod WaitForExit {
        $script:waited = $true
        return $${exits}
      }
      return $process
    }
    function winget.exe {
      if (-not $script:waited) { throw 'Installer started before waiting for app exit' }
      Write-Host ('MOCK_WINGET ' + ($args -join ' '))
      $global:LASTEXITCODE = ${exitCode}
    }
  `;
  const result = spawnSync(
    'powershell.exe',
    [
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      mocks + runner.replace('__LOGLOOKER_PID__', '12345'),
    ],
    { encoding: 'utf8', timeout: 15000, windowsHide: true },
  );
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  return result.stdout;
}

test(
  'helper waits for exit, upgrades only LogLooker, and reports completion',
  { skip: process.platform !== 'win32' },
  () => {
    const output = runHelper();
    assert.match(
      output,
      /MOCK_WINGET upgrade --id PsyChonek\.LogLooker --exact --source winget --interactive/,
    );
    assert.match(output, /Update complete/);
  },
);

test(
  'helper does not invoke winget if the app cannot exit',
  { skip: process.platform !== 'win32' },
  () => {
    const output = runHelper({ exits: false });
    assert.doesNotMatch(output, /MOCK_WINGET/);
    assert.match(output, /Update failed: LogLooker is still running/);
  },
);

test(
  'installer errors remain visible without claiming success',
  { skip: process.platform !== 'win32' },
  () => {
    const output = runHelper({ exitCode: 42 });
    assert.match(output, /Update failed: Winget did not complete the update \(exit code 42\)/);
    assert.doesNotMatch(output, /Update complete/);
  },
);
