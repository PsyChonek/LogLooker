import assert from 'node:assert/strict';
import test from 'node:test';
import { canResizeRightBoundary, widthFromPointerDelta } from '../src/utils/tableResize.ts';

test('the boundary before a fixed column cannot resize', () => {
  const columns = [{ resizable: true }, { resizable: false }];

  assert.equal(canResizeRightBoundary(columns, 0), false);
});

test('ordinary and trailing resizable boundaries stay enabled', () => {
  assert.equal(canResizeRightBoundary([{}, {}], 0), true);
  assert.equal(canResizeRightBoundary([{}], 0), true);
});

test('a fixed column has no resize boundary of its own', () => {
  assert.equal(canResizeRightBoundary([{ resizable: false }], 0), false);
});

test('drag width follows the pointer delta from its starting position', () => {
  assert.equal(widthFromPointerDelta(200, 540, 600, 48), 260);
  assert.equal(widthFromPointerDelta(200, 540, 230, 48), 48);
});
