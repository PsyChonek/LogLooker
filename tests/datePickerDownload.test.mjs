import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { compileScript, parse, registerTS } from '@vue/compiler-sfc';
import ts from 'typescript';
import { createRenderer, h, nextTick } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
import { DATE_PRESETS } from '../src/utils/dateRange.ts';

registerTS(() => ts);

// Compile the real component/store without a browser or the Tauri runtime.
async function loadModule(path, component = false) {
  const url = new URL(path, import.meta.url);
  let source = await readFile(url, 'utf8');
  if (component) {
    source = compileScript(parse(source, { filename: fileURLToPath(url) }).descriptor, {
      id: 'date-picker-test',
      fs: { fileExists: ts.sys.fileExists, readFile: ts.sys.readFile },
    }).content;
  }
  let code = ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
  }).outputText;
  code = code.replace(/from ['"]([^'"]+)['"]/g, (_, specifier) => {
    const resolved = specifier.startsWith('@/')
      ? new URL(`../src/${specifier.slice(2)}.ts`, import.meta.url).href
      : import.meta.resolve(specifier);
    return `from '${resolved}'`;
  });
  return import(`data:text/javascript;base64,${btoa(unescape(encodeURIComponent(code)))}`);
}

const { useAppStore } = await loadModule('../src/stores/appStore.ts');
const { default: DateRangePicker } = await loadModule(
  '../src/components/DateRangePicker.vue',
  true,
);
DateRangePicker.render = () => null;

const renderer = createRenderer({
  createComment: () => ({}),
  insert() {},
  remove() {},
  parentNode: () => null,
  nextSibling: () => null,
});

globalThis.localStorage = undefined;
globalThis.window = undefined;
globalThis.document = undefined;

async function fixture(t) {
  t.mock.timers.enable({ apis: ['Date'], now: new Date(2026, 8, 7, 12).getTime() });
  t.mock.method(globalThis, 'setInterval', () => 0);
  const saved = { datePreset: null, dateFrom: '2020-01-01', dateTo: '2020-01-02' };
  t.mock.property(globalThis, 'localStorage', {
    getItem: () => JSON.stringify(saved),
    setItem() {},
  });
  t.mock.property(globalThis, 'window', new EventTarget());
  t.mock.property(globalThis, 'document', new EventTarget());
  setActivePinia(createPinia());
  const store = useAppStore();
  store.datePreset = 'last7';
  let picker;
  const app = renderer.createApp({
    render: () =>
      h(DateRangePicker, {
        ref: (instance) => {
          picker = instance?.$?.setupState;
        },
        from: store.dateFrom,
        to: store.dateTo,
        preset: store.datePreset,
        onChange: (selection) => store.setDateRange(selection),
      }),
  });
  app.mount({});
  t.after(() => app.unmount());
  return {
    store,
    picker,
    async click(day) {
      picker.pickDay(day);
      await nextTick();
    },
  };
}

test('custom range keeps a start equal to the active preset start', async (t) => {
  const { store, click } = await fixture(t);
  const from = store.dateFrom;
  const to = store.dateTo;
  await click(from);
  await click(to);
  assert.equal(store.datePreset, null);
  assert.equal(store.dateFrom, from);
  assert.equal(store.dateTo, to);
});

test('single-day selection at the preset end sends that day to downloads', async (t) => {
  const { store, click } = await fixture(t);
  const day = store.dateTo;
  await click(day);
  await click(day);
  const calls = [];
  window.__TAURI_INTERNALS__ = {
    invoke: async (command, args) => {
      calls.push({ command, args });
      return [];
    },
  };
  await store.downloadSelected('chosen-folder');
  assert.equal(calls[0].command, 'download_services');
  assert.equal(calls[0].args.dateFrom, day);
  assert.equal(calls[0].args.dateTo, day);
});

test('reverse selection commits an ordered custom range', async (t) => {
  const { store, click } = await fixture(t);
  await click('2026-07-20');
  await click('2026-07-10');
  assert.equal(store.dateFrom, '2026-07-10');
  assert.equal(store.dateTo, '2026-07-20');
});

test('preset selection replaces a custom range and remains relative', async (t) => {
  const { store, picker, click } = await fixture(t);
  await click('2020-01-01');
  await click('2020-01-02');
  picker.applyPreset(DATE_PRESETS.find((preset) => preset.id === 'today'));
  await nextTick();
  assert.equal(store.datePreset, 'today');
  assert.equal(store.dateFrom, '2026-09-07');
  assert.equal(store.dateTo, '2026-09-07');
});

for (const action of ['syncSelected', 'downloadSelected']) {
  test(`${action} refreshes a relative range before the rollover timer fires`, async (t) => {
    const { store } = await fixture(t);
    t.mock.timers.setTime(new Date(2026, 8, 8, 0, 0, 1).getTime());
    const calls = [];
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        calls.push({ command, args });
        return [];
      },
    };
    await store[action]('chosen-folder');
    assert.equal(calls[0].args.dateFrom, '2026-09-02');
    assert.equal(calls[0].args.dateTo, '2026-09-08');
  });
}
