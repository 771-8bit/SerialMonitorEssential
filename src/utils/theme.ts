import { useEffect, useLayoutEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { usePersistentState, loadSetting, isOneOf } from './persist';

/**
 * Theme (issue #6): Dark / Light, or follow the OS ("System"). Default Dark keeps
 * the look the app always had. The choice is stored like the other settings, applied
 * as <html data-theme>, and followed live by every window (Main, Plotter, AI Guide):
 * localStorage fires a `storage` event in the other windows of the same origin.
 */
export type ThemeSetting = 'dark' | 'light' | 'system';
export type ResolvedTheme = 'dark' | 'light';

export const THEME_OPTIONS: readonly ThemeSetting[] = ['dark', 'light', 'system'];
const THEME_KEY = 'theme';
const isThemeSetting = isOneOf<ThemeSetting>(THEME_OPTIONS);

export function resolveTheme(setting: ThemeSetting, osPrefersDark: boolean): ResolvedTheme {
  if (setting === 'system') return osPrefersDark ? 'dark' : 'light';
  return setting;
}

function darkQuery(): MediaQueryList | null {
  return typeof window.matchMedia === 'function'
    ? window.matchMedia('(prefers-color-scheme: dark)')
    : null;
}

/** Current theme setting, its setter, and the theme actually applied */
export function useTheme(): [ThemeSetting, (t: ThemeSetting) => void, ResolvedTheme] {
  const [setting, setSetting] = usePersistentState<ThemeSetting>(THEME_KEY, 'dark', isThemeSetting);
  const [osDark, setOsDark] = useState(() => darkQuery()?.matches ?? true);

  // Follow the OS while on System
  useEffect(() => {
    const q = darkQuery();
    if (!q) return;
    const onChange = () => setOsDark(q.matches);
    q.addEventListener('change', onChange);
    return () => q.removeEventListener('change', onChange);
  }, []);

  // Follow a change made in another window
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === null || e.key === `sme.${THEME_KEY}`) {
        setSetting(loadSetting(THEME_KEY, 'dark', isThemeSetting));
      }
    };
    window.addEventListener('storage', onStorage);
    return () => window.removeEventListener('storage', onStorage);
  }, [setSetting]);

  const resolved = resolveTheme(setting, osDark);
  // A layout effect: it runs before every passive effect of the tree, so a child
  // that reads the theme colors in its own effect (the chart's axes) already
  // sees the new values
  useLayoutEffect(() => {
    document.documentElement.dataset.theme = resolved;
    // The title bar is drawn by the OS: tell it too (best effort)
    Promise.resolve()
      .then(() => invoke('set_window_theme', { theme: resolved }))
      .catch((e) => console.error(e));
  }, [resolved]);

  return [setting, setSetting, resolved];
}

/** Read a theme color token (for canvas drawing, which cannot use CSS variables) */
export function themeColor(token: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(token).trim();
  return v || fallback;
}
