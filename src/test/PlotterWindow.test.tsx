import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, act, fireEvent } from '@testing-library/react';
import PlotterWindow from '../components/plotter/PlotterWindow';
import { invoke } from '@tauri-apps/api/core';

// ==================== uPlot mock ====================
// uPlot needs a real canvas, so it is replaced by a recorder that keeps the
// options object (to fire the chart hooks from tests) and the instance
// (to assert on setScale/setData calls).

interface MockUPlotInstance {
  destroy: ReturnType<typeof vi.fn>;
  setData: ReturnType<typeof vi.fn>;
  setSize: ReturnType<typeof vi.fn>;
  setScale: ReturnType<typeof vi.fn>;
  redraw: ReturnType<typeof vi.fn>;
  series: { show?: boolean }[];
  scales: { x: { min: number; max: number }; y: { min: number; max: number } };
  data: (number | null)[][];
  over: HTMLElement;
}

type ChartHook = (u: MockUPlotInstance, key?: string) => void;

interface MockUPlotOptions {
  hooks: { setScale: ChartHook[]; setSelect: ChartHook[]; init: ChartHook[] };
}

const uplotState = vi.hoisted(() => ({
  options: [] as unknown[],
  instances: [] as unknown[],
}));

vi.mock('uplot', () => ({
  // must be a real function: LineChart calls `new uPlot(...)`
  default: vi.fn(function MockUPlot(opts: unknown, data: unknown) {
    const instance = {
      destroy: vi.fn(),
      setData: vi.fn(),
      setSize: vi.fn(),
      setScale: vi.fn(),
      redraw: vi.fn(),
      series: [],
      scales: { x: { min: 0, max: 1 }, y: { min: 0, max: 1 } },
      data,
      over: document.createElement('div'),
    };
    uplotState.options.push(opts);
    uplotState.instances.push(instance);
    return instance;
  }),
}));

const lastChartOptions = () =>
  uplotState.options[uplotState.options.length - 1] as MockUPlotOptions;
const lastChartInstance = () =>
  uplotState.instances[uplotState.instances.length - 1] as MockUPlotInstance;

// Mock ResizeObserver (must be constructible - LineChart does `new ResizeObserver`)
class MockResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}
(globalThis as typeof globalThis & { ResizeObserver: unknown }).ResizeObserver = MockResizeObserver;

// ==================== Backend fixtures ====================

interface DataRequest {
  time_min_ms: number | null;
  time_max_ms: number | null;
  pixel_width: number;
  is_realtime: boolean;
}

interface ChartPayloadLike {
  aligned_data: (number | null)[][];
  channel_names: string[];
  band_data: null;
  state_data: Record<string, unknown[]>;
  channels: unknown[];
  start_ms: number;
  end_ms: number;
}

// Default mock return value for chart data
const mockChartData: ChartPayloadLike = {
  aligned_data: [
    [58, 59, 60],
    [10, 20, 30],
  ],
  channel_names: ['ch0'],
  band_data: null,
  state_data: {},
  channels: [
    {
      name: 'ch0',
      channel_type: 'Line',
      latest_value: '30.00',
      point_count: 3,
    },
  ],
  start_ms: 0,
  end_ms: 60000,
};

// Backend answer after a clear(): no data at all
const emptyChartData: ChartPayloadLike = {
  aligned_data: [[]],
  channel_names: [],
  band_data: null,
  state_data: {},
  channels: [],
  start_ms: 0,
  end_ms: 0,
};

// Backend answer for a live window that has scrolled past the last sample:
// no points, but start/end echo the requested window.
const emptyWindowData: ChartPayloadLike = {
  ...emptyChartData,
  start_ms: 60000,
  end_ms: 70000,
};

// Mutable backend state driven by individual tests
let backendVersion = 0;
let backendHasData = true;
let backendPayload: ChartPayloadLike = mockChartData;
let dataRequests: DataRequest[] = [];

const lastDataRequest = () => dataRequests[dataRequests.length - 1];

// ==================== requestAnimationFrame control ====================
// The component drives the sliding window from a rAF loop; tests step it
// frame by frame instead of relying on jsdom's timing.

let rafQueue: FrameRequestCallback[] = [];

async function runFrames(count = 1) {
  for (let i = 0; i < count; i++) {
    const callbacks = rafQueue;
    rafQueue = [];
    await act(async () => {
      callbacks.forEach((cb) => cb(performance.now()));
      // let the async loop body (version check + data fetch) settle
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }
}

describe('PlotterWindow', () => {
  beforeEach(() => {
    vi.clearAllMocks();

    uplotState.options.length = 0;
    uplotState.instances.length = 0;

    backendVersion = 0;
    backendHasData = true;
    backendPayload = mockChartData;
    dataRequests = [];

    rafQueue = [];
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
      rafQueue.push(cb);
      return rafQueue.length;
    });
    vi.stubGlobal('cancelAnimationFrame', () => {});

    // Reset the mock with default implementation
    (invoke as ReturnType<typeof vi.fn>).mockImplementation(
      async (cmd: string, args?: { request?: DataRequest }) => {
        if (cmd === 'start_plotter_thread') {
          return undefined;
        }
        if (cmd === 'stop_plotter_thread') {
          return undefined;
        }
        if (cmd === 'check_plotter_version') {
          return { version: backendVersion, has_data: backendHasData };
        }
        if (cmd === 'get_plotter_chart_data') {
          if (args?.request) dataRequests.push(args.request);
          return backendPayload;
        }
        if (cmd === 'set_aggregation_mode') {
          return undefined;
        }
        return undefined;
      }
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  /** Render and pump frames until the first payload has been applied */
  async function renderWithData() {
    const utils = render(<PlotterWindow />);
    await act(async () => {
      await Promise.resolve();
    });
    backendVersion = 1;
    await runFrames(2);
    return utils;
  }

  // ==================== Rendering Tests ====================

  it('renders without crashing', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });
    expect(screen.getByRole('combobox', { name: 'Downsample mode' })).toBeDefined();
  });

  it('displays mode selector dropdown', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });
    expect(screen.getByRole('combobox', { name: 'Downsample mode' })).toBeDefined();
  });

  it('displays pause/play button', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });
    expect(screen.getByRole('button', { name: /Pause/ })).toBeDefined();
  });

  // ==================== Thread Lifecycle Tests ====================

  it('starts plotter thread on mount', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });

    expect(invoke).toHaveBeenCalledWith('start_plotter_thread');
  });

  it('stops plotter thread on unmount', async () => {
    let unmountFn: () => void;

    await act(async () => {
      const { unmount } = render(<PlotterWindow />);
      unmountFn = unmount;
    });

    expect(invoke).toHaveBeenCalledWith('start_plotter_thread');

    await act(async () => {
      unmountFn();
    });

    expect(invoke).toHaveBeenCalledWith('stop_plotter_thread');
  });

  // ==================== Mode Options Tests ====================

  it('has LTTB and Average options in mode selector', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });

    const modeSelect = screen.getByRole('combobox', { name: 'Downsample mode' });
    const optionValues = Array.from(modeSelect.querySelectorAll('option')).map((opt) => opt.value);
    expect(optionValues).toContain('Lttb');
    expect(optionValues).toContain('Average');
  });

  // ==================== Mode Switching Tests ====================

  it('mode selector has onChange handler', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });

    const modeSelect = screen.getByRole('combobox', {
      name: 'Downsample mode',
    }) as HTMLSelectElement;

    // Verify the select element exists and has the expected default value
    expect(modeSelect).toBeDefined();
    expect(modeSelect.value).toBe('Lttb');
  });

  // ==================== Window Selector Tests ====================

  it('renders the window selector with a 10s default', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });

    const windowSelect = screen.getByRole('combobox', { name: 'Time window' }) as HTMLSelectElement;
    expect(windowSelect.value).toBe('10');
    expect(screen.getByText('Window:')).toBeDefined();
  });

  it('lists all window widths in the window selector', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });

    const windowSelect = screen.getByRole('combobox', { name: 'Time window' });
    const optionValues = Array.from(windowSelect.querySelectorAll('option')).map((o) => o.value);
    expect(optionValues).toEqual(['1', '2', '5', '10', '30', '60', '120', '300']);
    const optionLabels = Array.from(windowSelect.querySelectorAll('option')).map(
      (o) => o.textContent
    );
    expect(optionLabels).toEqual(['1s', '2s', '5s', '10s', '30s', '60s', '120s', '300s']);
  });

  // ==================== Sliding Window Request Tests ====================

  it('bootstraps with an open-ended request before the first payload', async () => {
    render(<PlotterWindow />);
    await act(async () => {
      await Promise.resolve();
    });

    backendVersion = 1;
    await runFrames(1);

    expect(dataRequests).toHaveLength(1);
    expect(dataRequests[0].time_min_ms).toBeNull();
    expect(dataRequests[0].time_max_ms).toBeNull();
    expect(dataRequests[0].is_realtime).toBe(true);
  });

  it('requests an explicit sliding window once the right edge is known', async () => {
    await renderWithData();

    // A new version forces another fetch - this one must carry the window
    backendVersion = 2;
    await runFrames(1);

    const req = lastDataRequest();
    expect(req.is_realtime).toBe(true);
    expect(req.time_min_ms).not.toBeNull();
    expect(req.time_max_ms).not.toBeNull();
    // 10s window anchored at the payload's end_ms (60000), plus the elapsed
    // wall time of the frames that ran in between.
    expect(req.time_max_ms!).toBeGreaterThanOrEqual(60000);
    const span = req.time_max_ms! - req.time_min_ms!;
    expect(span).toBeGreaterThanOrEqual(10000);
    expect(span).toBeLessThanOrEqual(10002); // floor/ceil rounding only
  });

  it('clamps the window start at zero near the beginning of the stream', async () => {
    backendPayload = { ...mockChartData, end_ms: 2000 };
    await renderWithData();

    backendVersion = 2;
    await runFrames(1);

    expect(lastDataRequest().time_min_ms).toBe(0);
  });

  it('refetches with the new width when the window selector changes', async () => {
    await renderWithData();

    const windowSelect = screen.getByRole('combobox', { name: 'Time window' });
    await act(async () => {
      fireEvent.change(windowSelect, { target: { value: '30' } });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect((windowSelect as HTMLSelectElement).value).toBe('30');
    const req = lastDataRequest();
    const span = req.time_max_ms! - req.time_min_ms!;
    expect(span).toBeGreaterThanOrEqual(30000);
    expect(span).toBeLessThanOrEqual(30002);
  });

  it('scrolls the x window every frame without refetching', async () => {
    await renderWithData();

    const fetchesAfterFirstPayload = dataRequests.length;
    const chart = lastChartInstance();
    chart.setScale.mockClear();

    await runFrames(3);

    // No version change -> no data request, but the window kept moving
    expect(dataRequests).toHaveLength(fetchesAfterFirstPayload);
    const xCalls = chart.setScale.mock.calls.filter((c) => c[0] === 'x');
    expect(xCalls.length).toBeGreaterThanOrEqual(3);
    const [, firstRange] = xCalls[0] as [string, { min: number; max: number }];
    const [, lastRange] = xCalls[xCalls.length - 1] as [string, { min: number; max: number }];
    expect(lastRange.max).toBeGreaterThan(firstRange.max);
    // Fixed width window
    expect(lastRange.max - lastRange.min).toBeCloseTo(10, 6);
    expect(firstRange.max - firstRange.min).toBeCloseTo(10, 6);
  });

  // ==================== View State Tests ====================

  it('shows the LIVE status by default and no LIVE button', async () => {
    await renderWithData();

    expect(screen.getByText('● LIVE')).toBeDefined();
    expect(screen.queryByRole('button', { name: /▶ LIVE/ })).toBeNull();
  });

  it('shows the paused status when the pause button is clicked', async () => {
    await renderWithData();

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Pause/ }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(screen.getByText('⏸ Paused')).toBeDefined();
  });

  it('enters Inspect and shows the LIVE button when the user drag-selects', async () => {
    await renderWithData();

    // Simulate uPlot's drag-select by firing the hook the chart registered
    await act(async () => {
      lastChartOptions().hooks.setSelect[0](lastChartInstance());
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(screen.getByText('🔍 Inspect')).toBeDefined();
    expect(screen.getByRole('button', { name: /▶ LIVE/ })).toBeDefined();
  });

  it('returns to LIVE when the LIVE button is clicked', async () => {
    await renderWithData();

    await act(async () => {
      lastChartOptions().hooks.setSelect[0](lastChartInstance());
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(screen.getByText('🔍 Inspect')).toBeDefined();

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /▶ LIVE/ }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(screen.getByText('● LIVE')).toBeDefined();
    // Back to LIVE re-anchors on the newest data (open-ended bootstrap request)
    const req = lastDataRequest();
    expect(req.time_min_ms).toBeNull();
    expect(req.time_max_ms).toBeNull();
    expect(req.is_realtime).toBe(true);
  });

  it('fetches the selected range while inspecting (scrollback)', async () => {
    await renderWithData();

    await act(async () => {
      lastChartOptions().hooks.setSelect[0](lastChartInstance());
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // uPlot reports the new visible x range (seconds) after a pan/zoom
    const chart = lastChartInstance();
    chart.scales.x = { min: 12.5, max: 34.5 };
    await act(async () => {
      lastChartOptions().hooks.setScale[0](chart, 'x');
      // LineChart debounces range notifications by 200ms
      await new Promise((resolve) => setTimeout(resolve, 300));
    });

    const req = lastDataRequest();
    expect(req.is_realtime).toBe(false);
    expect(req.time_min_ms).toBe(12500);
    expect(req.time_max_ms).toBe(34500);
  });

  // ==================== Empty payload / clear handling ====================

  it('keeps the chart when the live window scrolled past the last sample', async () => {
    await renderWithData();
    expect(screen.getByText('ch0:')).toBeDefined();

    // The window moved beyond the data: backend returns an empty payload
    // whose start/end echo the requested (still advancing) window
    backendPayload = emptyWindowData;
    backendVersion = 2;
    await runFrames(1);

    expect(screen.queryByText('No data yet.')).toBeNull();
    expect(screen.getByText('ch0:')).toBeDefined();
  });

  it('clears the chart when the backend reports no data (Clear)', async () => {
    await renderWithData();
    expect(screen.getByText('ch0:')).toBeDefined();

    backendPayload = emptyChartData;
    backendHasData = false;
    backendVersion = 2;
    await runFrames(1);

    expect(screen.getByText('No data yet.')).toBeDefined();
  });

  // ==================== Error Handling Tests ====================

  it('handles start_plotter_thread failure gracefully', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockImplementation(async (cmd: string) => {
      if (cmd === 'start_plotter_thread') {
        throw new Error('Serial port not open');
      }
      return undefined;
    });

    // Should not throw
    await act(async () => {
      expect(() => render(<PlotterWindow />)).not.toThrow();
    });
  });

  it('handles get_plotter_chart_data failure gracefully', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockImplementation(async (cmd: string) => {
      if (cmd === 'start_plotter_thread') {
        return undefined;
      }
      if (cmd === 'check_plotter_version') {
        return { version: 1, has_data: true };
      }
      if (cmd === 'get_plotter_chart_data') {
        throw new Error('Data fetch failed');
      }
      return undefined;
    });

    // Should not throw
    render(<PlotterWindow />);
    await runFrames(2);

    expect(screen.getByText(/Data fetch failed/)).toBeDefined();
  });
});
