import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import App from '../App';

/*
 * App-level behaviour for the disconnect / hotplug / log-error seams.
 *
 * - GAP-08 (SYS-F-107): a fatal read error must also release the backend port
 *   handle, otherwise the UI says "disconnected" while SerialState still owns
 *   the OS handle (DEBT-1).
 * - GAP-07 (SYS-F-107): active hotplug detection is done by polling
 *   `list_ports` every 2s (no cross-platform OS notification without native
 *   code), so the dropdown updates live when a device is plugged in.
 * - GAP-09 (SYS-F-205): the backend `log-error` event must reach the user.
 *
 * The remaining device-level behaviour (a real unplug producing the read
 * error in the worker thread) can only be covered by E2E with hardware.
 */

// jsdom has no ResizeObserver; the receive panel's scroll hook constructs one.
class MockResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver = MockResizeObserver as unknown as typeof ResizeObserver;

type EventHandler = (event: { payload: unknown }) => void;

const handlers = new Map<string, EventHandler>();
const unlisten = vi.fn();

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

/** Wait for the pending `invoke` promises inside the effects to settle */
async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

function emit(event: string, payload: unknown) {
  const handler = handlers.get(event);
  if (!handler) throw new Error(`no listener registered for "${event}"`);
  act(() => {
    handler({ payload });
  });
}

describe('App', () => {
  beforeEach(() => {
    handlers.clear();
    vi.clearAllMocks();
    vi.spyOn(window, 'alert').mockImplementation(() => {});

    mockListen.mockImplementation((event: string, handler: unknown) => {
      handlers.set(event, handler as EventHandler);
      return Promise.resolve(unlisten);
    });
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'list_ports') return Promise.resolve(['COM1']);
      return Promise.resolve(undefined);
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  // ---------------- GAP-08 ----------------

  it('releases the backend port handle when a disconnect is detected', async () => {
    render(<App />);
    await flush();
    mockInvoke.mockClear();

    emit('serial-status', { connected: false, error: 'Device removed' });

    expect(mockInvoke).toHaveBeenCalledWith('close_port');
    expect(window.alert).toHaveBeenCalledWith('Serial disconnected: Device removed');
  });

  it('still releases the handle when the disconnect carries no error text', async () => {
    render(<App />);
    await flush();
    mockInvoke.mockClear();

    emit('serial-status', { connected: null, error: null });

    expect(mockInvoke).toHaveBeenCalledWith('close_port');
    expect(window.alert).not.toHaveBeenCalled();
  });

  it('does not close the port on a connected=true status', async () => {
    render(<App />);
    await flush();
    mockInvoke.mockClear();

    emit('serial-status', { connected: true, error: null });

    expect(mockInvoke).not.toHaveBeenCalledWith('close_port');
  });

  // ---------------- GAP-09 ----------------

  it('surfaces log-error events to the user', async () => {
    render(<App />);
    await flush();

    emit('log-error', { message: 'No space left on device' });

    expect(window.alert).toHaveBeenCalledWith('ログ書き込みエラー: No space left on device');
  });

  // ---------------- GAP-07 ----------------

  it('polls list_ports on an interval so hotplugged devices appear', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    render(<App />);
    await flush();

    const initialCalls = mockInvoke.mock.calls.filter(([cmd]) => cmd === 'list_ports').length;
    expect(initialCalls).toBe(1);

    // A device is plugged in between polls
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'list_ports') return Promise.resolve(['COM1', 'COM7']);
      return Promise.resolve(undefined);
    });

    await act(async () => {
      vi.advanceTimersByTime(2000);
      await Promise.resolve();
    });

    const polledCalls = mockInvoke.mock.calls.filter(([cmd]) => cmd === 'list_ports').length;
    expect(polledCalls).toBe(2);

    const options = Array.from(document.querySelectorAll('option')).map((o) => o.textContent);
    expect(options).toContain('COM7');
  });

  it('stops polling once unmounted', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { unmount } = render(<App />);
    await flush();

    unmount();
    mockInvoke.mockClear();

    await act(async () => {
      vi.advanceTimersByTime(10000);
      await Promise.resolve();
    });

    expect(mockInvoke.mock.calls.filter(([cmd]) => cmd === 'list_ports')).toHaveLength(0);
  });
});
