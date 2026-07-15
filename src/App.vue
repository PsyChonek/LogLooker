<script setup lang="ts">
import '@/composables/useTheme';
import { getCurrentWindow } from '@tauri-apps/api/window';
import AppHeader from '@/components/AppHeader.vue';
import AuthBanner from '@/components/AuthBanner.vue';
import RawFileViewer from '@/components/RawFileViewer.vue';
import SearchChart from '@/components/SearchChart.vue';
import { useAppStore } from '@/stores/appStore';
import type { ChartViewParams, RawViewParams } from '@/types';

const store = useAppStore();

// Set by the open_raw_window / open_chart_window commands; such a window holds
// nothing but the raw file, or nothing but the chart
const injected = window as Window & {
  __RAW_VIEW__?: RawViewParams;
  __CHART_VIEW__?: ChartViewParams;
};
const rawView = injected.__RAW_VIEW__ ?? null;
const chartView = injected.__CHART_VIEW__ ?? null;

function closeWindow() {
  getCurrentWindow().close();
}
</script>

<template>
  <div class="min-h-screen bg-gray-50 dark:bg-gray-900 text-gray-800 dark:text-gray-100">
    <RawFileViewer
      v-if="rawView"
      windowed
      :service-id="rawView.serviceId"
      :service-name="rawView.serviceName"
      :file="rawView.file"
      :line="rawView.line ?? undefined"
      :initial-query="rawView.query ?? undefined"
      :initial-is-regex="rawView.isRegex"
      :initial-case-sensitive="rawView.caseSensitive"
      @close="closeWindow"
    />
    <div
      v-else-if="chartView"
      class="p-6"
    >
      <div class="mb-4 text-sm text-gray-500 dark:text-gray-400">
        Chart of
        <span class="font-mono text-gray-800 dark:text-gray-100">{{
          chartView.query || 'all lines'
        }}</span>
        — follows the search in the main window
      </div>
      <SearchChart
        windowed
        :query="chartView.query"
        :initial="chartView.request"
      />
    </div>
    <template v-else>
      <AppHeader title="LogLooker">
        <template #nav>
          <nav class="flex items-center gap-4 text-sm">
            <router-link
              v-for="link in [
                { to: '/', label: 'Services' },
                { to: '/search', label: 'Search' },
                { to: '/files', label: 'Files' },
                { to: '/log', label: 'Log' },
              ]"
              :key="link.to"
              :to="link.to"
              class="text-gray-500 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200"
              exact-active-class="text-blue-600 dark:text-blue-400 font-medium"
            >
              {{ link.label }}
            </router-link>
          </nav>
        </template>
      </AppHeader>
      <main class="p-6">
        <AuthBanner
          v-if="store.authNotice"
          :notice="store.authNotice"
          @dismiss="store.authNotice = null"
        />
        <router-view />
      </main>
    </template>
  </div>
</template>
