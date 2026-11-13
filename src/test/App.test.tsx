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
      if (cmd === 'open_port' || cmd === 'reopen_port') return Promise.resolve(1);
      if (cmd === 'close_port') return Promise.resolve(true);
      if (cmd === 'confirm_port_removed') return Promise.resolve(false);
      if (cmd === 'clear_data') return Promise.resolve(2);
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
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    mockInvoke.mockClear();

    emit('serial-status', {
      connected: false,
      connection_id: 1,
      error: 'Read failed',
    });
    await flush();

    expect(mockInvoke).toHaveBeenCalledWith('close_port', { connectionId: 1 });
    expect(mockInvoke).toHaveBeenCalledWith('confirm_port_removed', { portName: 'COM1' });
    expect(window.alert).toHaveBeenCalledWith('Serial disconnected: Read failed');
  });

  it('still releases the handle when the disconnect carries no error text', async () => {
    render(<App />);
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    mockInvoke.mockClear();

    emit('serial-status', {
      connected: false,
      connection_id: 1,
      error: null,
    });
    await flush();

    expect(mockInvoke).toHaveBeenCalledWith('close_port', { connectionId: 1 });
    expect(window.alert).not.toHaveBeenCalled();
  });

  it('does not close the port on a connected=true status', async () => {
    render(<App />);
    await flush();
    mockInvoke.mockClear();

    emit('serial-status', { connected: true, error: null });

    expect(mockInvoke).not.toHaveBeenCalledWith('close_port');
  });

  it('updates the UI before confirming a delayed unplug and retains diagnostics', async () => {
    const diagnostic = vi.spyOn(console, 'error').mockImplementation(() => {});
    let finishConfirmation!: (removed: boolean) => void;
    mockInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'list_ports') return ['XIAO (COM4)'];
      if (cmd === 'open_port') return 1;
      if (cmd === 'close_port') return true;
      if (cmd === 'confirm_port_removed') {
        return new Promise<boolean>((resolve) => (finishConfirmation = resolve));
      }
      return undefined;
    });
    render(<App />);
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    emit('serial-status', {
      connected: false,
      connection_id: 1,
      error: 'Failed to read from port: Os { code: 22 }',
    });
    await flush();
    expect(screen.getByRole('button', { name: 'Connect' })).toBeInTheDocument();
    expect(mockInvoke).toHaveBeenCalledWith('close_port', { connectionId: 1 });
    expect(mockInvoke).toHaveBeenCalledWith('confirm_port_removed', { portName: 'COM4' });
    expect(window.alert).not.toHaveBeenCalled();
    await act(async () => {
      finishConfirmation(true);
    });
    expect(window.alert).not.toHaveBeenCalled();
    expect(diagnostic).toHaveBeenCalledWith(
      'Serial disconnected:',
      expect.stringContaining('code: 22')
    );
  });

  it('does not show an old read error once another open attempt has started', async () => {
    let finishConfirmation!: (removed: boolean) => void;
    let finishOpen!: (id: number) => void;
    let openCount = 0;
    mockInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'list_ports') return ['COM1'];
      if (cmd === 'open_port') {
        openCount += 1;
        if (openCount === 1) return 1;
        return new Promise<number>((resolve) => (finishOpen = resolve));
      }
      if (cmd === 'close_port') return true;
      if (cmd === 'confirm_port_removed') {
        return new Promise<boolean>((resolve) => (finishConfirmation = resolve));
      }
      return undefined;
    });
    render(<App />);
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    emit('serial-status', { connected: false, connection_id: 1, error: 'Read failed' });
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await act(async () => {
      finishConfirmation(false);
    });
    expect(window.alert).not.toHaveBeenCalled();
    await act(async () => {
      finishOpen(2);
    });
    expect(screen.getByRole('button', { name: 'Disconnect' })).toBeInTheDocument();
  });

  it('ignores an old disconnect event after the same port is reopened', async () => {
    render(<App />);
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Disconnect' }));
    await flush();
    mockInvoke.mockResolvedValueOnce(2);
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    mockInvoke.mockClear();
    emit('serial-status', {
      connected: false,
      connection_id: 1,
      error: 'old',
    });
    await flush();
    expect(screen.getByRole('button', { name: 'Disconnect' })).toBeInTheDocument();
    expect(mockInvoke.mock.calls.filter(([cmd]) => cmd === 'close_port')).toHaveLength(0);
    expect(window.alert).not.toHaveBeenCalled();
  });

  it('ignores a delayed cleanup reply once a new connection has opened', async () => {
    render(<App />);
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    let finishClose!: (closed: boolean) => void;
    mockInvoke.mockImplementationOnce(
      () =>
        new Promise<boolean>((resolve) => {
          finishClose = resolve;
        })
    );
    emit('serial-status', {
      connected: false,
      connection_id: 1,
      error: 'old',
    });
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Disconnect' }));
    await flush();
    mockInvoke.mockResolvedValueOnce(2);
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    await act(async () => {
      finishClose(true);
    });
    expect(screen.getByRole('button', { name: 'Disconnect' })).toBeInTheDocument();
    expect(window.alert).not.toHaveBeenCalled();
  });

  it('handles an unplug that arrives before the open reply', async () => {
    let finishOpen!: (id: number) => void;
    mockInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'list_ports') return ['COM1'];
      if (cmd === 'open_port') return new Promise<number>((resolve) => (finishOpen = resolve));
      if (cmd === 'close_port' || cmd === 'confirm_port_removed') return true;
      return undefined;
    });
    render(<App />);
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    emit('serial-status', {
      connected: false,
      connection_id: 1,
      error: 'removed',
    });
    await act(async () => {
      finishOpen(1);
    });
    await flush();
    expect(mockInvoke).toHaveBeenCalledWith('close_port', { connectionId: 1 });
    expect(screen.getByRole('button', { name: 'Connect' })).toBeInTheDocument();
    expect(window.alert).not.toHaveBeenCalled();
  });

  it('keeps the displayed byte count across disconnect, failed open and reconnect until Clear', async () => {
    render(<App />);
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    emit('data-update', { capture_id: 1, total_bytes: 10 });
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Disconnect' }));
    await flush();
    expect(screen.getByText(/Total: 10 bytes/)).toBeInTheDocument();
    mockInvoke.mockRejectedValueOnce('busy');
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    expect(screen.getByText(/Total: 10 bytes/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    expect(screen.getByText(/Total: 10 bytes/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Clear' }));
    await flush();
    expect(screen.getByText(/Total: 0 bytes/)).toBeInTheDocument();
    emit('data-update', { capture_id: 1, total_bytes: 20 });
    expect(screen.getByText(/Total: 0 bytes/)).toBeInTheDocument();
  });

  it('shows new data that arrives before the Clear reply even at the previous byte count', async () => {
    let finishClear!: (captureId: number) => void;
    let finishOldFetch!: (payload: unknown) => void;
    let cleared = false;
    const row = (text: string) => ({
      lines: [{ offset: 0, text, timestamp: null }],
      total_lines: 1,
      start_line: 0,
      reaches_end: true,
    });
    mockInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'list_ports') return ['COM1'];
      if (cmd === 'clear_data') return new Promise<number>((resolve) => (finishClear = resolve));
      if (cmd === 'get_ascii_lines') {
        if (cleared) return row('new capture');
        return new Promise((resolve) => (finishOldFetch = resolve));
      }
      return undefined;
    });

    render(<App />);
    await flush();
    emit('data-update', { capture_id: 1, total_bytes: 10 });
    await flush();
    fireEvent.click(screen.getByRole('button', { name: 'Clear' }));
    cleared = true;
    emit('data-update', { capture_id: 2, total_bytes: 10 });
    expect(await screen.findByText('new capture')).toBeInTheDocument();
    await act(async () => {
      finishClear(2);
      finishOldFetch(row('old capture'));
    });
    expect(screen.getByText(/Total: 10 bytes/)).toBeInTheDocument();
    expect(screen.getByText('new capture')).toBeInTheDocument();
    expect(screen.queryByText('old capture')).not.toBeInTheDocument();
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

  it('keeps the selected port through unplug and reconnects before the next refresh', async () => {
    vi.useFakeTimers();
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const device = 'XIAO (COM4)';
    let available = ['COM1', device];
    mockInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'list_ports') return available;
      if (cmd === 'open_port') {
        if (!available.includes(device)) throw new Error('COM4 not found');
        return 1;
      }
      if (cmd === 'close_port' || cmd === 'confirm_port_removed') return true;
      return undefined;
    });

    render(<App />);
    await flush();
    const port = screen.getByRole<HTMLSelectElement>('combobox', { name: 'Port' });
    fireEvent.change(port, { target: { value: device } });
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    expect(screen.getByRole('button', { name: 'Reload' })).toBeDisabled();

    available = ['COM1'];
    emit('serial-status', { connected: false, connection_id: 1, error: 'USB unplugged' });
    await flush();
    await act(() => vi.advanceTimersByTimeAsync(2000));
    expect(port).toHaveValue(device);
    expect(port.selectedOptions[0]).toHaveTextContent(`${device} (not present)`);

    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    expect(window.alert).toHaveBeenCalledWith(expect.stringContaining('COM4 not found'));
    expect(port).toHaveValue(device);

    available = ['COM1', device];
    mockInvoke.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await flush();
    expect(mockInvoke).toHaveBeenCalledWith(
      'open_port',
      expect.objectContaining({ portName: device })
    );
    expect(screen.getByRole('button', { name: 'Disconnect' })).toBeInTheDocument();
  });

  it('reloads immediately and keeps the selected COM number when its label changes', async () => {
    vi.useFakeTimers();
    let finishOldPoll!: (ports: string[]) => void;
    mockInvoke.mockImplementationOnce(async () => ['COM1', 'XIAO (COM4)']);
    render(<App />);
    await flush();
    const port = screen.getByRole('combobox', { name: 'Port' });
    fireEvent.change(port, { target: { value: 'XIAO (COM4)' } });

    mockInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'list_ports')
        return new Promise<string[]>((resolve) => (finishOldPoll = resolve));
      return undefined;
    });
    await act(() => vi.advanceTimersByTimeAsync(2000));
    mockInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'list_ports') return ['COM1', 'USB Serial Device (COM4)', 'COM7'];
      return undefined;
    });
    mockInvoke.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Reload' }));
    await flush();
    expect(mockInvoke).toHaveBeenCalledWith('list_ports');
    expect(port).toHaveValue('USB Serial Device (COM4)');
    expect(screen.getByRole('option', { name: 'COM7' })).toBeInTheDocument();

    await act(async () => finishOldPoll(['COM1']));
    expect(port).toHaveValue('USB Serial Device (COM4)');
    expect(screen.getByRole('option', { name: 'COM7' })).toBeInTheDocument();
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

    it('opens a fresh install with DTR and RTS off so no board is reset', async () => {
      render(<App />);
      await flush();
      expect(screen.getByLabelText('DtrEnable')).not.toBeChecked();
      expect(screen.getByLabelText('RtsEnable')).not.toBeChecked();

      fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
      await flush();
      expect(mockInvoke).toHaveBeenCalledWith('open_port', {
        portName: 'COM1',
        config: expect.objectContaining({ dtr: false, rts: false, baud_rate: 115200 }),
      });
    });

    it('explains DTR and RTS for Arduino boards on hover', async () => {
      render(<App />);
      await flush();
      const marks = screen.getAllByText('?');
      expect(marks).toHaveLength(2);
      expect(marks[0].getAttribute('title')).toMatch(/Arduino Uno.*reset/);
      expect(marks[0].getAttribute('title')).toMatch(/native USB/);
      expect(marks[1].getAttribute('title')).toMatch(/ESP32.*reset/);
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

      fireEvent.click(screen.getByLabelText('Line Wrap')); // default OFF (C# version)

      expect(localStorage.getItem('sme.receive.lineWrap')).toBe('true');
    });
  });

  // ---------------- Save / Copy with timestamps (issue #4) ----------------

  describe('save and copy with timestamps', () => {
    async function renderWithData() {
      render(<App />);
      await flush();
      emit('data-update', { capture_id: 1, total_bytes: 10 });
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
        separator: ' > ', // default separator (C# version)
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
        timestampSeparator: ' > ',
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

  // ---------------- Auto Connect (SYS-F-807) ----------------

  describe('auto connect', () => {
    let available: string[];
    let reopenFails: boolean;

    beforeEach(() => {
      localStorage.clear();
      available = ['Pico (COM1)'];
      reopenFails = false;
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'list_ports') return Promise.resolve(available);
        if (cmd === 'reopen_port') {
          return reopenFails ? Promise.reject('Access is denied.') : Promise.resolve(2);
        }
        if (cmd === 'open_port') return Promise.resolve(1);
        if (cmd === 'close_port') return Promise.resolve(true);
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

    /** Render, connect, turn Auto Connect on, then lose the device */
    async function connectAndLoseDevice() {
      vi.useFakeTimers({ shouldAdvanceTime: true });
      render(<App />);
      await flush();
      fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
      await flush();
      expect(invoked('open_port')).toHaveLength(1);
      fireEvent.click(screen.getByLabelText('Auto Connect'));
      await flush();

      available = [];
      emit('serial-status', {
        connected: false,
        connection_id: 1,
        error: 'Device removed',
      });
      await flush();
    }

    it('waits for the lost device without a blocking alert', async () => {
      await connectAndLoseDevice();

      expect(mockInvoke).toHaveBeenCalledWith('close_port', { connectionId: 1 });
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
      expect(screen.getByRole('status')).toHaveTextContent('Connected automatically');
      expect(mockInvoke).toHaveBeenCalledWith('set_reconnecting', { waiting: false });
      expect(lastArgs('reopen_port')).toMatchObject({ portName: 'Pico (COM1)' });
      // Continuing the capture, never a new session
      expect(invoked('open_port')).toHaveLength(1);

      const attempts = invoked('reopen_port').length;
      await tick(2000);
      expect(invoked('reopen_port')).toHaveLength(attempts); // stopped polling
    });

    it('slows down and says so when the port is listed but cannot be opened', async () => {
      await connectAndLoseDevice();
      available = ['Pico (COM1)'];
      reopenFails = true; // e.g. another program holds the port

      // 200 ms retries until the 5th failure (one tick per retry: each attempt awaits)
      for (let i = 0; i < 6; i++) await tick(200);
      const fast = invoked('reopen_port').length;
      expect(fast).toBeGreaterThanOrEqual(5);
      expect(screen.getByRole('status')).toHaveTextContent('(cannot open the port)');

      for (let i = 0; i < 10; i++) await tick(200);
      expect(invoked('reopen_port').length - fast).toBeLessThanOrEqual(2); // 1 s retries
    });

    it('matches on the device path even if the friendly name changes', async () => {
      await connectAndLoseDevice();
      available = ['USB Serial Device (COM1)'];
      await tick(500);
      expect(lastArgs('reopen_port')).toMatchObject({ portName: 'USB Serial Device (COM1)' });
    });

    it('Cancel stops waiting, turns Auto Connect off and never reopens', async () => {
      await connectAndLoseDevice();

      fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
      await flush();
      expect(mockInvoke).toHaveBeenCalledWith('set_reconnecting', { waiting: false });
      expect(screen.getByRole('button', { name: 'Connect' })).toBeInTheDocument();
      expect(screen.getByLabelText('Auto Connect')).not.toBeChecked();

      available = ['Pico (COM1)'];
      await tick(2000);
      expect(invoked('reopen_port')).toHaveLength(0);
    });

    it('Disconnect turns Auto Connect off so the port is not reopened (C# version)', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true });
      render(<App />);
      await flush();
      fireEvent.click(screen.getByLabelText('Auto Connect'));
      await tick(200);
      expect(screen.getByRole('button', { name: 'Disconnect' })).toBeInTheDocument();

      fireEvent.click(screen.getByRole('button', { name: 'Disconnect' }));
      await flush();
      expect(mockInvoke).toHaveBeenCalledWith('close_port');
      expect(screen.getByLabelText('Auto Connect')).not.toBeChecked();
      expect(localStorage.getItem('sme.autoConnect')).toBe('false');

      const attempts = invoked('reopen_port').length;
      await tick(2000);
      expect(invoked('reopen_port')).toHaveLength(attempts);
    });

    it('turning the option off while waiting stops waiting', async () => {
      await connectAndLoseDevice();
      fireEvent.click(screen.getByLabelText('Auto Connect'));
      await flush();

      available = ['Pico (COM1)'];
      await tick(2000);
      expect(invoked('reopen_port')).toHaveLength(0);
      expect(localStorage.getItem('sme.autoConnect')).toBe('false');
    });

    it('connects to the selected port when turned on while idle', async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true });
      render(<App />);
      await flush();
      fireEvent.click(screen.getByLabelText('Auto Connect'));
      await tick(200);

      expect(lastArgs('reopen_port')).toMatchObject({ portName: 'Pico (COM1)' });
      expect(invoked('open_port')).toHaveLength(0);
      expect(screen.getByRole('button', { name: 'Disconnect' })).toBeInTheDocument();
      expect(localStorage.getItem('sme.autoConnect')).toBe('true');
    });

    it('connects to the remembered port at startup', async () => {
      localStorage.setItem('sme.autoConnect', 'true');
      localStorage.setItem('sme.lastPort', JSON.stringify('Uno (COM3)'));
      available = ['Pico (COM1)', 'Uno (COM3)'];
      vi.useFakeTimers({ shouldAdvanceTime: true });
      render(<App />);
      await tick(200);

      expect(lastArgs('reopen_port')).toMatchObject({ portName: 'Uno (COM3)' });
      expect(screen.getByRole('button', { name: 'Disconnect' })).toBeInTheDocument();
    });

    it('waits at startup for a remembered port that is not plugged in', async () => {
      localStorage.setItem('sme.autoConnect', 'true');
      localStorage.setItem('sme.lastPort', JSON.stringify('Uno (COM3)'));
      vi.useFakeTimers({ shouldAdvanceTime: true });
      render(<App />);
      await tick(1000);

      expect(invoked('reopen_port')).toHaveLength(0);
      expect(screen.getByRole('status')).toHaveTextContent('Waiting for COM3…');
      const portSelect = screen.getAllByRole('combobox')[0] as HTMLSelectElement;
      expect(portSelect.selectedOptions[0].textContent).toBe('Uno (COM3) (not present)');

      available = ['Pico (COM1)', 'Uno (COM3)'];
      await tick(400);
      expect(lastArgs('reopen_port')).toMatchObject({ portName: 'Uno (COM3)' });
    });

    it('does not carry over the old Auto Reconnect setting', async () => {
      localStorage.setItem('sme.autoReconnect', 'true');
      vi.useFakeTimers({ shouldAdvanceTime: true });
      render(<App />);
      await tick(1000);
      expect(screen.getByLabelText('Auto Connect')).not.toBeChecked();
      expect(invoked('reopen_port')).toHaveLength(0);
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
