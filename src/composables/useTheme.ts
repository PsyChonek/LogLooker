import { ref, watchEffect } from 'vue';

export interface ThemeDefinition {
  id: string;
  name: string;
  /** Swatch preview colors shown in the theme selector */
  preview: {
    accent: string;
    surfaceLight: string;
    surfaceDark: string;
  };
}

export const themes: ThemeDefinition[] = [
  {
    id: 'gray',
    name: 'Gray',
    preview: { accent: '#3b82f6', surfaceLight: '#fafafa', surfaceDark: '#262626' },
  },
  {
    id: 'ocean',
    name: 'Ocean',
    preview: { accent: '#06b6d4', surfaceLight: '#f0f6fc', surfaceDark: '#1b3048' },
  },
  {
    id: 'forest',
    name: 'Forest',
    preview: { accent: '#10b981', surfaceLight: '#eff6ef', surfaceDark: '#1f351f' },
  },
  {
    id: 'sunset',
    name: 'Sunset',
    preview: { accent: '#f97316', surfaceLight: '#faf5ef', surfaceDark: '#402f1e' },
  },
  {
    id: 'orchid',
    name: 'Orchid',
    preview: { accent: '#8b5cf6', surfaceLight: '#f7f3fa', surfaceDark: '#392747' },
  },
];

function loadTheme(): string {
  const stored = localStorage.getItem('theme');
  return themes.some((t) => t.id === stored) ? (stored as string) : 'gray';
}

const theme = ref(loadTheme());
// Falls back to the pre-theme-selector 'darkMode' key so existing users keep their variant
const dark = ref(localStorage.getItem('darkMode') === 'true');

watchEffect(() => {
  localStorage.setItem('theme', theme.value);
  localStorage.setItem('darkMode', String(dark.value));
  document.documentElement.dataset.theme = theme.value;
  document.documentElement.classList.toggle('dark', dark.value);
});

export function useTheme() {
  return { theme, dark, themes };
}
