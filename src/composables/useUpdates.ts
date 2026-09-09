import { reactive } from 'vue';

interface UpdateCheck {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
}

type Invoke = <T>(command: string) => Promise<T>;

export function useUpdates(invoke: Invoke) {
  const state = reactive({
    visible: false,
    checking: false,
    installing: false,
    error: null as string | null,
    result: null as UpdateCheck | null,
  });
  let dismissed = false;
  let pending: Promise<void> | null = null;

  function check(manual = true): Promise<void> {
    if (state.installing) return Promise.resolve();
    if (manual) state.visible = true;
    if (pending) return pending;
    dismissed = false;
    state.checking = true;
    state.error = null;
    state.result = null;
    pending = (async () => {
      try {
        state.result = await invoke<UpdateCheck>('check_for_updates');
        if (!manual && !dismissed && state.result.updateAvailable) state.visible = true;
      } catch (error) {
        state.error = String(error);
      } finally {
        state.checking = false;
        pending = null;
      }
    })();
    return pending;
  }

  function dismiss() {
    if (state.installing) return;
    dismissed = true;
    state.visible = false;
  }

  async function install() {
    if (state.installing || state.checking || !state.result?.updateAvailable) return;
    state.installing = true;
    state.error = null;
    try {
      await invoke('install_winget_update');
    } catch (error) {
      state.error = String(error);
      state.installing = false;
    }
  }

  return { state, check, dismiss, install };
}
