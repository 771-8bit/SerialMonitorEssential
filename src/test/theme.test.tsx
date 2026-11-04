import { describe, it, expect, vi } from 'vitest';
import { act, render, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { resolveTheme, useTheme, type ThemeSetting } from '../utils/theme';

function Probe({ onReady }: { onReady: (set: (t: ThemeSetting) => void) => void }) {
  const [, setTheme] = useTheme();
  onReady(setTheme);
  return null;
}

describe('theme (issue #6)', () => {
  it('resolves System from the OS and keeps explicit choices', () => {
    expect(resolveTheme('system', true)).toBe('dark');
    expect(resolveTheme('system', false)).toBe('light');
    expect(resolveTheme('light', true)).toBe('light');
    expect(resolveTheme('dark', false)).toBe('dark');
  });

  it('defaults to Dark and applies the choice to <html data-theme>, persisted', async () => {
    let setTheme: (t: ThemeSetting) => void = () => {};
    render(<Probe onReady={(s) => (setTheme = s)} />);
    expect(document.documentElement.dataset.theme).toBe('dark');

    act(() => setTheme('light'));

    expect(document.documentElement.dataset.theme).toBe('light');
    expect(localStorage.getItem('sme.theme')).toBe('"light"');
    // The OS-drawn title bar follows as well
    await waitFor(() =>
      expect(vi.mocked(invoke)).toHaveBeenCalledWith('set_window_theme', { theme: 'light' })
    );
  });

  it('follows a change made in another window', () => {
    render(<Probe onReady={() => {}} />);

    localStorage.setItem('sme.theme', '"light"');
    act(() => {
      window.dispatchEvent(new StorageEvent('storage', { key: 'sme.theme' }));
    });

    expect(document.documentElement.dataset.theme).toBe('light');
  });
});
