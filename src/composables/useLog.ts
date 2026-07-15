import { ref } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import type { LogEntry } from '@/types';

const logs = ref<LogEntry[]>([]);

export function useLog() {
  async function refreshLogs() {
    logs.value = await invoke<LogEntry[]>('get_logs');
  }

  async function clearLogs() {
    await invoke('clear_logs');
    logs.value = [];
  }

  return {
    logs,
    refreshLogs,
    clearLogs,
  };
}
