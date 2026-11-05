import { useCallback, useEffect, useRef, useState } from 'react';

/** How long a toast stays up (the C# version closed its message box after 1 s) */
export const TOAST_MS = 1500;

/**
 * A short, non-blocking confirmation ("Copied", "Saved"). window.alert is not used
 * because it blocks the JS thread, which would stall reconnect polling.
 * A new message replaces the current one and restarts the timer.
 */
export function useToast(): [string | null, (text: string) => void] {
  const [text, setText] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const show = useCallback((message: string) => {
    if (timerRef.current !== null) clearTimeout(timerRef.current);
    setText(message);
    timerRef.current = setTimeout(() => {
      timerRef.current = null;
      setText(null);
    }, TOAST_MS);
  }, []);

  useEffect(
    () => () => {
      if (timerRef.current !== null) clearTimeout(timerRef.current);
    },
    []
  );

  return [text, show];
}
