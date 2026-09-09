import assert from 'node:assert/strict';
import test from 'node:test';
import { useUpdates } from '../src/composables/useUpdates.ts';

const available = { currentVersion: '1.1.9', latestVersion: '1.1.21', updateAvailable: true };

test('startup opens only for an available update', async () => {
  for (const updateAvailable of [false, true]) {
    const updates = useUpdates(async () => ({ ...available, updateAvailable }));
    await updates.check(false);
    assert.equal(updates.state.visible, updateAvailable);
    assert.equal(updates.state.checking, false);
  }
});

test('startup failures stay quiet, manual checks explain the failure', async () => {
  const updates = useUpdates(async () => {
    throw 'packageUnavailable';
  });
  await updates.check(false);
  assert.equal(updates.state.visible, false);
  await updates.check();
  assert.equal(updates.state.visible, true);
  assert.equal(updates.state.error, 'packageUnavailable');
});

test('manual check joins startup check and dismissal prevents reopening', async () => {
  let finish;
  let calls = 0;
  const updates = useUpdates(() => {
    calls++;
    return new Promise((resolve) => {
      finish = resolve;
    });
  });
  const background = updates.check(false);
  const manual = updates.check();
  assert.equal(updates.state.visible, true);
  assert.equal(calls, 1);
  updates.dismiss();
  finish(available);
  await Promise.all([background, manual]);
  assert.equal(updates.state.visible, false);
});

test('install requires an update and cannot be started twice', async () => {
  const calls = [];
  let finish;
  const updates = useUpdates((command) => {
    calls.push(command);
    if (command === 'check_for_updates') return Promise.resolve(available);
    return new Promise((resolve) => {
      finish = resolve;
    });
  });
  await updates.install();
  assert.deepEqual(calls, []);
  await updates.check();
  const installing = updates.install();
  await updates.install();
  updates.dismiss();
  assert.equal(updates.state.visible, true);
  assert.deepEqual(calls, ['check_for_updates', 'install_winget_update']);
  finish();
  await installing;
});

test('failed handoff keeps the popup usable and allows a fresh check', async () => {
  const updates = useUpdates(async (command) => {
    if (command === 'install_winget_update') throw 'launchFailed';
    return available;
  });
  await updates.check();
  await updates.install();
  assert.equal(updates.state.installing, false);
  assert.equal(updates.state.error, 'launchFailed');
  assert.equal(updates.state.visible, true);
  await updates.check();
  assert.equal(updates.state.error, null);
});
