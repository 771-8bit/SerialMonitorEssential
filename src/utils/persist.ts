import { useEffect, useState, type Dispatch, type SetStateAction } from 'react';

/**
 * Settings that survive an app restart (issue #7).
 *
 * Values are stored as JSON in localStorage under "sme.<key>". A missing,
 * unparsable or invalid stored value falls back to the default, so a corrupted
 * or outdated entry can never break startup. Received data, the connection
 * state and the send history are deliberately NOT persisted.
 */
const PREFIX = 'sme.';

export function loadSetting<T>(key: string, fallback: T, isValid: (v: unknown) => v is T): T {
  try {
    const raw = localStorage.getItem(PREFIX + key);
    if (raw === null) return fallback;
    const parsed: unknown = JSON.parse(raw);
    return isValid(parsed) ? parsed : fallback;
  } catch {
    return fallback;
  }
}

export function saveSetting<T>(key: string, value: T): void {
  try {
    localStorage.setItem(PREFIX + key, JSON.stringify(value));
  } catch {
    // Storage full or unavailable: the setting just isn't remembered
  }
}

/** useState that is loaded from and saved to localStorage */
export function usePersistentState<T>(
  key: string,
  fallback: T,
  isValid: (v: unknown) => v is T
): [T, Dispatch<SetStateAction<T>>] {
  const [value, setValue] = useState<T>(() => loadSetting(key, fallback, isValid));
  useEffect(() => {
    saveSetting(key, value);
  }, [key, value]);
  return [value, setValue];
}

// --- Validators --------------------------------------------------------------------

export const isBoolean = (v: unknown): v is boolean => typeof v === 'boolean';

export const isString = (v: unknown): v is string => typeof v === 'string';

export function isOneOf<T extends string | number>(values: readonly T[]) {
  return (v: unknown): v is T => values.includes(v as T);
}
