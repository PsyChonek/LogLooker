import assert from 'node:assert/strict';
import test from 'node:test';
import { compileQuery } from '../src/utils/regexQuery.ts';
import { builderOpenAction } from '../src/utils/queryBuilderState.ts';

const storedRoot = {
  type: 'group',
  combinator: 'all',
  children: [{ type: 'criterion', mode: 'contains', text: 'previous', label: '' }],
};

test('opening the builder keeps the latest hand-edited query', () => {
  assert.deepEqual(builderOpenAction(storedRoot, 'latest user text'), { kind: 'keep' });
});

test('opening the builder keeps a query the user cleared', () => {
  assert.deepEqual(builderOpenAction(storedRoot, ''), { kind: 'keep' });
});

test('opening the builder restores a different builder-generated query', () => {
  const latestRoot = {
    type: 'group',
    combinator: 'any',
    children: [
      { type: 'criterion', mode: 'contains', text: 'error', label: '' },
      { type: 'criterion', mode: 'contains', text: 'warning', label: '' },
    ],
  };
  const latestQuery = compileQuery(latestRoot);
  const action = builderOpenAction(storedRoot, latestQuery);

  assert.equal(action.kind, 'replace');
  assert.equal(compileQuery(action.root), latestQuery);
});

test('opening the builder keeps its full stored tree when the query still matches', () => {
  assert.deepEqual(builderOpenAction(storedRoot, compileQuery(storedRoot)), { kind: 'keep' });
});
