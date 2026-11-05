import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, act, fireEvent, screen } from '@testing-library/react';
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
    localStorage.clear();
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

    expect(window.alert).toHaveBeenCalledWith('Log write error: No space left on device');
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

  // ---------------- Settings survive a restart (issue #7) ----------------

  describe('remembered settings', () => {
    it('restores the serial settings and the last chosen port', async () => {
      localStorage.setItem(
        'sme.serialConfig',
        JSON.stringify({
          baud_rate: 921600,
          data_bits: 8,
          flow_control: 'None',
          parity: 'Even',
          stop_bits: 1,
          dtr: true,
          rts: false,
        })
      );
      localStorage.setItem('sme.lastPort', JSON.stringify('COM7'));
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'list_ports') return Promise.resolve(['COM1', 'COM7']);
        return Promise.resolve(undefined);
      });

      render(<App />);
      await flush();

      expect(screen.getByText('921600 bps')).toBeInTheDocument();
      expect(screen.getByRole('combobox', { name: 'Port' })).toHaveValue('COM7');
      expect(screen.getByLabelText('RtsEnable')).not.toBeChecked();
    });

    it('keeps the preferred port when it is absent at startup', async () => {
      localStorage.setItem('sme.lastPort', JSON.stringify('COM7'));
      render(<App />);
      await flush();

      // Falls back to what is there, without forgetting the preference
      expect(screen.getByRole('combobox', { name: 'Port' })).toHaveValue('COM1');
      expect(localStorage.getItem('sme.lastPort')).toBe('"COM7"');
    });

    it('saves a changed setting', async () => {
      render(<App />);
      await flush();

      fireEvent.click(screen.getByLabelText('Line Wrap'));

      expect(localStorage.getItem('sme.receive.lineWrap')).toBe('false');
    });
  });

  // ---------------- Save / Copy with timestamps (issue #4) ----------------

  describe('save and copy with timestamps', () => {
    async function renderWithData() {
      render(<App />);
      await flush();
      emit('data-update', { total_bytes: 10 });
    }

    it('saves text with timestamps when Timestamp is ON', async () => {
      const { save } = await import('@tauri-apps/plugin-dialog');
      vi.mocked(save).mockResolvedValue('C:/tmp/log.txt');
      await renderWithData();

      fireEvent.click(screen.getByRole('button', { name: 'Save' }));
      await flush();

      expect(vi.mocked(save)).toHaveBeenCalledWith(
        expect.objectContaining({ defaultPath: 'serial_log.txt' })
      );
      expect(mockInvoke).toHaveBeenCalledWith('export_text_with_timestamps', {
        path: 'C:/tmp/log.txt',
        separator: ' ',
      });
    });

    it('saves the raw bytes when Timestamp is OFF', async () => {
      const { save } = await import('@tauri-apps/plugin-dialog');
      vi.mocked(save).mockResolvedValue('C:/tmp/log.bin');
      await renderWithData();

      fireEvent.click(screen.getByLabelText('Timestamp'));
      fireEvent.click(screen.getByRole('button', { name: 'Save' }));
      await flush();

      expect(mockInvoke).toHaveBeenCalledWith('export_log', { path: 'C:/tmp/log.bin' });
      expect(mockInvoke).not.toHaveBeenCalledWith('export_text_with_timestamps', expect.anything());
    });

    it('copies with the chosen separator when Timestamp is ON', async () => {
      await renderWithData();

      fireEvent.change(screen.getByTitle('Separator between Timestamp and Data'), {
        target: { value: ',' },
      });
      fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
      await flush();

      expect(mockInvoke).toHaveBeenCalledWith('get_clipboard_text', {
        mode: 'ascii',
        timestampSeparator: ',',
      });
    });

    it('copies the hex rows with timestamps in the Hex view', async () => {
      await renderWithData();

      fireEvent.click(
        screen.getByTitle('Toggle between Hex and ASCII view').querySelector('input')!
      );
      fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
      await flush();

      expect(mockInvoke).toHaveBeenCalledWith('get_clipboard_text', {
        mode: 'hex',
        timestampSeparator: ' ',
      });
    });

    it('copies the hex rows without timestamps when Timestamp is OFF', async () => {
      await renderWithData();

      fireEvent.click(
        screen.getByTitle('Toggle between Hex and ASCII view').querySelector('input')!
      );
      fireEvent.click(screen.getByLabelText('Timestamp'));
      fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
      await flush();

      expect(mockInvoke).toHaveBeenCalledWith('get_clipboard_text', {
        mode: 'hex',
        timestampSeparator: null,
      });
    });

    it('confirms a copy with a toast that goes away by itself', async () => {
      const writeText = vi.fn().mockResolvedValue(undefined);
      Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'list_ports') return Promise.resolve(['COM1']);
        if (cmd === 'get_clipboard_text') return Promise.resolve('hello');
        return Promise.resolve(undefined);
      });
      await renderWithData();

      vi.useFakeTimers();
      fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });

      expect(writeText).toHaveBeenCalledWith('hello');
      expect(screen.getByText('Copied')).toHaveAttribute('role', 'status');
      act(() => {
        vi.advanceTimersByTime(2000);
      });
      expect(screen.queryByText('Copied')).toBeNull();
    });

    it('says there is nothing to copy or save when the capture is empty', async () => {
      render(<App />);
      await flush();

      fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
      await flush();
      expect(screen.getByText('Nothing to copy')).toBeInTheDocument();

      fireEvent.click(screen.getByRole('button', { name: 'Save' }));
      await flush();
      expect(screen.getByText('Nothing to save')).toBeInTheDocument();
      expect(window.alert).not.toHaveBeenCalled();
    });

    it('saves the raw bytes in the Hex view even with Timestamp ON', async () => {
      const { save } = await import('@tauri-apps/plugin-dialog');
      vi.mocked(save).mockResolvedValue('C:/tmp/log.bin');
      await renderWithData();

      fireEvent.click(
        screen.getByTitle('Toggle between Hex and ASCII view').querySelector('input')!
      );
      fireEvent.click(screen.getByRole('button', { name: 'Save' }));
      await flush();

      expect(mockInvoke).toHaveBeenCalledWith('export_log', { path: 'C:/tmp/log.bin' });
      expect(mockInvoke).not.toHaveBeenCalledWith('export_text_with_timestamps', expect.anything());
    });
  });

  // ---------------- Auto reconnect (SYS-F-807) ----------------

  describe('auto reconnect', () => {
    let available: string[];
    let reopenFails: boolean;

    beforeEach(() => {
      localStorage.clear();
      available = ['Pico (COM1)'];
      reopenFails = false;
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'list_ports') return Promise.resolve(available);
        if (cmd === 'reopen_port') {
          return reopenFails ? Promise.reject('Access is denied.') : Promise.resolve(undefined);
        }
        return Promise.resolve(undefined);
      });
    });

    const invoked = (cmd: string) => mockInvoke.mock.calls.filter(([c]) => c === cmd);
    const lastArgs = (cmd: string) => {
      const calls = invoked(cmd);
      return calls[calls.length - 1]?.[1];
    };

    async function tick(ms: number) {
      await act(async () => {
        vi.advanceTimersByTime(ms);
        await Promise.resolve();
        await Promise.resolve();
      });
      await flush();
    }

    /** Render, turn Auto Reconnect on, connect, then lose the device */
    async function connectAndLoseDevice() {
      vi.useFakeTimers({ shouldAdvanceTime: true });
      render(<App />);
      await flush();
      fireEvent.click(screen.getByLabelText('Auto Reconnect'));
      fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
      await flush();
      expect(invoked('open_port')).toHaveLength(1);

      available = [];
      emit('serial-status', { connected: false, error: 'Device removed' });
      await flush();
    }

    it('waits for the lost device without a blocking alert', async () => {
      await connectAndLoseDevice();

      expect(mockInvoke).toHaveBeenCalledWith('close_port');
      expect(window.alert).not.toHaveBeenCalled();
      expect(mockInvoke).toHaveBeenCalledWith('set_reconnecting', { waiting: true });
      expect(screen.getByRole('status')).toHaveTextContent('Waiting for COM1…');
      expect(screen.getByRole('button', { name: 'Cancel' })).toBeInTheDocument();

      // Another device shows up; the hotplug poll must not move the selection to it
      available = ['COM9'];
      await tick(2000);
      const portSelect = screen.getAllByRole('combobox')[0] as HTMLSelectElement;
      expect(portSelect.value).toBe('Pico (COM1)');
      expect(portSelect.selectedOptions[0].textContent).toBe('Pico (COM1) (not present)');
    });

    it('reopens the same capture when the device returns, retrying failed opens', async () => {
      await connectAndLoseDevice();

      await tick(1000);
      expect(invoked('reopen_port')).toHaveLength(0); // not back yet

      // Back, but Windows still refuses the open for a moment
      available = ['Pico (COM1)'];
      reopenFails = true;
      await tick(500);
      expect(invoked('reopen_port').length).toBeGreaterThan(0);
      expect(screen.getByRole('button', { name: 'Cancel' })).toBeInTheDocument();

      reopenFails = false;
      await tick(500);
      expect(screen.getByRole('button', { name: 'Disconnect' })).toBeInTheDocument();
      expect(screen.getByRole('status')).toHaveTextContent('Reconnected');
      expect(mockInvoke).toHaveBeenCalledWith('set_reconnecting', { waiting: false });
      expect(lastArgs('reopen_port')).toMatchObject({ portName: 'Pico (COM1)' });
      // Continuing the capture, never a new session
      expect(invoked('open_port')).toHaveLength(1);

      const attempts = invoked('reopen_port').length;
      await tick(2000);
      expect(invoked('reopen_port')).toHaveLength(attempts); // stopped polling
    });

    it('matches on the device path even if the friendly name changes', async () => {
      await connectAndLoseDevice();
      available = ['USB Serial Device (COM1)'];
      await tick(500);
      expect(lastArgs('reopen_port')).toMatchObject({ portName: 'USB Serial Device (COM1)' });
    });

    it('Cancel stops waiting and never reopens', async () => {
      await connectAndLoseDevice();

      fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
      await flush();
      expect(mockInvoke).toHaveBeenCalledWith('set_reconnecting', { waiting: false });
      expect(screen.getByRole('button', { name: 'Connect' })).toBeInTheDocument();

      available = ['Pico (COM1)'];
      await tick(2000);
      expect(invoked('reopen_port')).toHaveLength(0);
    });

    it('turning the option off while waiting stops waiting', async () => {
      await connectAndLoseDevice();
      fireEvent.click(screen.getByLabelText('Auto Reconnect'));
      await flush();

      available = ['Pico (COM1)'];
      await tick(2000);
      expect(invoked('reopen_port')).toHaveLength(0);
      expect(localStorage.getItem('sme.autoReconnect')).toBe('false');
    });

    it('remembers the option across restarts', async () => {
      render(<App />);
      await flush();
      fireEvent.click(screen.getByLabelText('Auto Reconnect'));
      expect(localStorage.getItem('sme.autoReconnect')).toBe('true');
    });
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
