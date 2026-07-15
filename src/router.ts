import { createRouter, createWebHistory } from 'vue-router';
import ServicesView from '@/views/ServicesView.vue';
import SearchView from '@/views/SearchView.vue';
import FilesView from '@/views/FilesView.vue';
import LogView from '@/views/LogView.vue';

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', name: 'services', component: ServicesView },
    { path: '/search', name: 'search', component: SearchView },
    { path: '/files', name: 'files', component: FilesView },
    { path: '/log', name: 'log', component: LogView },
  ],
});

export default router;
