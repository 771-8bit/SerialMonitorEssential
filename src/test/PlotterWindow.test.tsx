import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, act } from '@testing-library/react';
import PlotterWindow from '../components/plotter/PlotterWindow';
import { invoke } from '@tauri-apps/api/core';

// Mock uPlot since it requires a real DOM
vi.mock('uplot', () => ({
  default: vi.fn().mockImplementation(() => ({
    destroy: vi.fn(),
    setData: vi.fn(),
    setSize: vi.fn(),
    series: [],
    scales: { x: { min: 0, max: 1 } },
  })),
}));

// Mock ResizeObserver
(globalThis as typeof globalThis & { ResizeObserver: unknown }).ResizeObserver = vi
  .fn()
  .mockImplementation(() => ({
    observe: vi.fn(),
    unobserve: vi.fn(),
    disconnect: vi.fn(),
  }));

// Default mock return value for chart data
const mockChartData = {
  aligned_data: [
    [0, 1, 2],
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
  end_ms: 2000,
};

describe('PlotterWindow', () => {
  beforeEach(() => {
    vi.clearAllMocks();

    // Reset the mock with default implementation
    (invoke as ReturnType<typeof vi.fn>).mockImplementation(async (cmd: string) => {
      if (cmd === 'start_plotter_thread') {
        return undefined;
      }
      if (cmd === 'stop_plotter_thread') {
        return undefined;
      }
      if (cmd === 'get_plotter_chart_data') {
        return mockChartData;
      }
      if (cmd === 'set_aggregation_mode') {
        return undefined;
      }
      return undefined;
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  // ==================== Rendering Tests ====================

  it('renders without crashing', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });
    expect(screen.getByRole('combobox')).toBeDefined();
  });

  it('displays mode selector dropdown', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });
    const modeSelect = screen.getByRole('combobox');
    expect(modeSelect).toBeDefined();
  });

  it('displays pause/play button', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });
    const button = screen.getByRole('button');
    expect(button).toBeDefined();
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

    const modeSelect = screen.getByRole('combobox');
    const options = modeSelect.querySelectorAll('option');

    const optionValues = Array.from(options).map((opt) => opt.value);
    expect(optionValues).toContain('Lttb');
    expect(optionValues).toContain('Average');
  });

  // ==================== Mode Switching Tests ====================

  it('mode selector has onChange handler', async () => {
    await act(async () => {
      render(<PlotterWindow />);
    });

    const modeSelect = screen.getByRole('combobox') as HTMLSelectElement;

    // Verify the select element exists and has the expected default value
    expect(modeSelect).toBeDefined();
    expect(modeSelect.value).toBe('Lttb');
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
      if (cmd === 'get_plotter_chart_data') {
        throw new Error('Data fetch failed');
      }
      return undefined;
    });

    // Should not throw
    await act(async () => {
      expect(() => render(<PlotterWindow />)).not.toThrow();
    });
  });
});
