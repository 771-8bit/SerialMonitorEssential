import { describe, it, expect, vi, beforeEach, afterEach, test } from 'vitest';
import { render, screen, act, fireEvent } from '@testing-library/react';
import PlotterWindow from '../components/plotter/PlotterWindow';
import { invoke } from '@tauri-apps/api/core';

/*
 * Systematic state-transition tests for SM-5 (plotter view state).
 *
 * Executes §4 of docs/24_vv_plan.md ("SM-5 について 1-switch 100%") against
 * the view state machine of docs/22_architecture_description.md §5.5.
 *
 * ==================== Reachable state set ====================
 *
 * PlotterWindow.tsx carries TWO independent booleans, not one tri-state:
 *
 *   isRunning   - false while paused (the rAF poll loop is torn down)
 *   isFollowing - false while inspecting (followRef mirrors it synchronously)
 *
 * so the real machine has 2 x 2 = 4 reachable states, which the footer
 * collapses onto 3 labels (`statusText` in PlotterWindow.tsx):
 *
 *   | model state         | isRunning | isFollowing | footer      | LIVE button |
 *   |---------------------|-----------|-------------|-------------|-------------|
 *   | LIVE                | true      | true        | '● LIVE'    | absent      |
 *   | Inspect             | true      | false       | '🔍 Inspect'| present     |
 *   | Paused-from-LIVE    | false     | true        | '⏸ Paused'  | absent      |
 *   | Paused-from-Inspect | false     | false       | '⏸ Paused'  | present     |
 *
 * Paused-from-Inspect IS reachable (Pause while inspecting) and is NOT the
 * same UI as Paused-from-LIVE: the ▶ LIVE button is only rendered in the
 * second one, because it is gated on `!isFollowing`, not on the footer label.
 * Both Paused states therefore have to be distinguished by the LIVE button.
 * A LiveClick in Paused-from-Inspect returns to follow mode but stays paused
 * (goLive does not touch isRunning), i.e. it lands in Paused-from-LIVE.
 *
 * ==================== Event alphabet ====================
 *
 *   PauseClick    - the ⏸ Pause button (only rendered while running)
 *   ResumeClick   - the ▶ Resume button (only rendered while paused)
 *   LiveClick     - the ▶ LIVE button (only rendered while !isFollowing)
 *   Interact      - uPlot's setSelect hook -> onUserInteraction (zoom/drag)
 *   WindowChange  - the Window <select>
 *   DataTick      - backend version bump + payload + one rAF step
 *   ResetPayload  - has_data:false then a fresh payload (backend Clear)
 *
 * Coverage: all 7 x 7 ordered pairs, each from the canonical LIVE-with-data
 * state. An event whose affordance does not exist in the current state is not
 * clickable; those are asserted as negative tests (the button is absent)
 * instead of being applied, and the state stays put.
 */

// ==================== uPlot mock ====================
// Same recorder harness as PlotterWindow.test.tsx: uPlot needs a real canvas,
// so it is replaced by a recorder that keeps the options object (to fire the
// chart hooks from tests) and the instance.

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

const mockChartData: ChartPayloadLike = {
  aligned_data: [
    [58, 59, 60],
    [10, 20, 30],
  ],
  channel_names: ['ch0'],
  band_data: null,
  state_data: {},
  channels: [{ name: 'ch0', channel_type: 'Line', latest_value: '30.00', point_count: 3 }],
  start_ms: 0,
  end_ms: 60000,
};

const emptyChartData: ChartPayloadLike = {
  aligned_data: [[]],
  channel_names: [],
  band_data: null,
  state_data: {},
  channels: [],
  start_ms: 0,
  end_ms: 0,
};

let backendVersion = 0;
let backendHasData = true;
let backendPayload: ChartPayloadLike = mockChartData;
let dataRequests: DataRequest[] = [];

// ==================== requestAnimationFrame control ====================

let rafQueue: FrameRequestCallback[] = [];

async function runFrames(count = 1) {
  for (let i = 0; i < count; i++) {
    const callbacks = rafQueue;
    rafQueue = [];
    await act(async () => {
      callbacks.forEach((cb) => cb(performance.now()));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }
}

// ==================== The model ====================

interface ViewState {
  running: boolean;
  following: boolean;
}

const LIVE: ViewState = { running: true, following: true };

const EVENTS = [
  'PauseClick',
  'ResumeClick',
  'LiveClick',
  'Interact',
  'WindowChange',
  'DataTick',
  'ResetPayload',
] as const;

type EventName = (typeof EVENTS)[number];

/** Footer label the component must render for a model state. */
function expectedStatus(state: ViewState): string {
  return !state.running ? '⏸ Paused' : state.following ? '● LIVE' : '🔍 Inspect';
}

/** Human-readable name of the model state (for failure messages). */
function stateName(state: ViewState): string {
  if (state.running) return state.following ? 'LIVE' : 'Inspect';
  return state.following ? 'Paused-from-LIVE' : 'Paused-from-Inspect';
}

/** Whether the affordance for `event` exists in `state`. */
function isApplicable(state: ViewState, event: EventName): boolean {
  switch (event) {
    case 'PauseClick':
      return state.running;
    case 'ResumeClick':
      return !state.running;
    case 'LiveClick':
      // The ▶ LIVE button is gated on !isFollowing
      return !state.following;
    default:
      // Interact / WindowChange / DataTick / ResetPayload are always possible;
      // some of them are simply no-ops in some states.
      return true;
  }
}

/** Expected next model state. */
function nextState(state: ViewState, event: EventName): ViewState {
  switch (event) {
    case 'PauseClick':
      return { ...state, running: false };
    case 'ResumeClick':
      return { ...state, running: true };
    case 'LiveClick':
      // goLive() restores follow mode but leaves isRunning alone
      return { ...state, following: true };
    case 'Interact':
      // handleUserInteraction() returns early when already inspecting
      return state.following ? { ...state, following: false } : state;
    default:
      return state;
  }
}

/**
 * Whether the component issues a one-shot / loop fetch for this transition.
 * - LiveClick always re-anchors (goLive -> fetchData)
 * - ResumeClick re-anchors only when it resumes into follow mode
 * - WindowChange refetches only while following AND running (SYS-F-606:
 *   Paused stops all frontend polling; the width takes effect on resume)
 * - DataTick / ResetPayload only reach the component through the rAF loop,
 *   which runs only while running AND following
 */
function expectsFetch(state: ViewState, event: EventName): boolean {
  switch (event) {
    case 'LiveClick':
      return true;
    case 'ResumeClick':
      return state.following;
    case 'WindowChange':
      return state.following && state.running;
    case 'DataTick':
    case 'ResetPayload':
      return state.running && state.following;
    default:
      return false;
  }
}

describe('PlotterWindow view FSM (SM-5)', () => {
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

    (invoke as ReturnType<typeof vi.fn>).mockImplementation(
      async (cmd: string, args?: { request?: DataRequest }) => {
        if (cmd === 'check_plotter_version') {
          return { version: backendVersion, has_data: backendHasData };
        }
        if (cmd === 'get_plotter_chart_data') {
          if (args?.request) dataRequests.push(args.request);
          return backendPayload;
        }
        return undefined;
      }
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  /** Render and pump frames until the first payload has been applied (LIVE). */
  async function renderLiveWithData() {
    render(<PlotterWindow />);
    await act(async () => {
      await Promise.resolve();
    });
    backendVersion = 1;
    await runFrames(2);
    expect(screen.getByText('● LIVE')).toBeDefined();
    expect(screen.getByText('ch0:')).toBeDefined();
  }

  async function click(name: RegExp) {
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  /** Current width of the live window, kept in sync with WindowChange. */
  let windowSec = 10;

  /**
   * Apply one event, or - when its affordance does not exist in `state` -
   * assert the affordance's absence and leave the state untouched.
   * Returns whether the event was actually applied.
   */
  async function applyEvent(state: ViewState, event: EventName): Promise<boolean> {
    if (!isApplicable(state, event)) {
      // Negative test for the transition that cannot happen here.
      switch (event) {
        case 'PauseClick':
          expect(screen.queryByRole('button', { name: /⏸ Pause/ })).toBeNull();
          expect(screen.getByRole('button', { name: /▶ Resume/ })).toBeDefined();
          break;
        case 'ResumeClick':
          expect(screen.queryByRole('button', { name: /▶ Resume/ })).toBeNull();
          expect(screen.getByRole('button', { name: /⏸ Pause/ })).toBeDefined();
          break;
        case 'LiveClick':
          expect(screen.queryByRole('button', { name: /▶ LIVE/ })).toBeNull();
          break;
      }
      return false;
    }

    switch (event) {
      case 'PauseClick':
        await click(/⏸ Pause/);
        break;
      case 'ResumeClick':
        await click(/▶ Resume/);
        break;
      case 'LiveClick':
        await click(/▶ LIVE/);
        break;
      case 'Interact':
        // uPlot's drag-select: fire the hook LineChart registered
        await act(async () => {
          lastChartOptions().hooks.setSelect[0](lastChartInstance());
          await new Promise((resolve) => setTimeout(resolve, 0));
        });
        break;
      case 'WindowChange': {
        // Alternate between two widths so every WindowChange is a real change.
        // Both are <= the 60 s of data, so the request span is never clamped.
        windowSec = windowSec === 30 ? 5 : 30;
        const select = screen.getByRole('combobox', { name: 'Time window' });
        await act(async () => {
          fireEvent.change(select, { target: { value: String(windowSec) } });
          await new Promise((resolve) => setTimeout(resolve, 0));
        });
        break;
      }
      case 'DataTick':
        backendVersion += 1;
        await runFrames(1);
        break;
      case 'ResetPayload':
        // Backend clear(): has_data goes false and the chart empties...
        backendHasData = false;
        backendPayload = emptyChartData;
        backendVersion += 1;
        await runFrames(1);
        // ...then a fresh session's data arrives.
        backendHasData = true;
        backendPayload = mockChartData;
        backendVersion += 1;
        await runFrames(1);
        break;
    }
    return true;
  }

  /**
   * Oracle: the footer label, the LIVE button and the shape of every request
   * issued during the event must match the model state.
   */
  function assertOracle(state: ViewState, issued: DataRequest[], ctx: string) {
    const status = expectedStatus(state);
    const where = `${ctx} -> expected ${stateName(state)}`;

    // Exactly one of the three status texts, and it is the expected one.
    const shown = ['● LIVE', '🔍 Inspect', '⏸ Paused'].filter(
      (text) => screen.queryByText(text) !== null
    );
    expect(shown, `${where}: footer status`).toEqual([status]);

    // The LIVE button exists iff we are not following.
    const liveButton = screen.queryByRole('button', { name: /▶ LIVE/ });
    if (state.following) {
      expect(liveButton, `${where}: ▶ LIVE must be hidden while following`).toBeNull();
    } else {
      expect(liveButton, `${where}: ▶ LIVE must be shown while inspecting`).not.toBeNull();
    }

    // Pause/Resume affordance matches isRunning.
    if (state.running) {
      expect(screen.queryByRole('button', { name: /⏸ Pause/ }), `${where}: pause`).not.toBeNull();
    } else {
      expect(screen.queryByRole('button', { name: /▶ Resume/ }), `${where}: resume`).not.toBeNull();
    }

    // Request shape: realtime iff following. In follow mode the range is
    // either the bootstrap (both null) or exactly the sliding window.
    for (const req of issued) {
      expect(req.is_realtime, `${where}: is_realtime of ${JSON.stringify(req)}`).toBe(
        state.following
      );
      if (state.following && req.time_min_ms !== null) {
        const span = req.time_max_ms! - req.time_min_ms;
        expect(span, `${where}: window span`).toBeGreaterThanOrEqual(windowSec * 1000);
        expect(span, `${where}: window span`).toBeLessThanOrEqual(windowSec * 1000 + 2);
      }
    }
  }

  // ==================== 0-switch: every transition once ====================

  it('0-switch: walks every SM-5 transition and reaches all four states', async () => {
    await renderLiveWithData();
    windowSec = 10;

    let state = LIVE;
    const visited = new Set<string>([stateName(state)]);

    // LIVE -> Inspect -> Paused-from-Inspect -> Paused-from-LIVE -> LIVE,
    // plus the self-loops (data / window / interact while inspecting).
    const walk: EventName[] = [
      'DataTick', // LIVE self-loop
      'WindowChange', // LIVE self-loop
      'ResetPayload', // LIVE self-loop (backend clear)
      'Interact', // LIVE -> Inspect
      'Interact', // Inspect self-loop (no-op)
      'DataTick', // Inspect self-loop (frozen)
      'WindowChange', // Inspect self-loop (no refetch)
      'PauseClick', // Inspect -> Paused-from-Inspect
      'ResumeClick', // Paused-from-Inspect -> Inspect
      'PauseClick', // Inspect -> Paused-from-Inspect
      'LiveClick', // Paused-from-Inspect -> Paused-from-LIVE
      'Interact', // Paused-from-LIVE -> Paused-from-Inspect
      'LiveClick', // Paused-from-Inspect -> Paused-from-LIVE
      'ResumeClick', // Paused-from-LIVE -> LIVE
      'PauseClick', // LIVE -> Paused-from-LIVE
      'ResumeClick', // Paused-from-LIVE -> LIVE
      'LiveClick', // impossible in LIVE (negative test)
    ];

    for (const event of walk) {
      const before = dataRequests.length;
      const applied = await applyEvent(state, event);
      if (applied) state = nextState(state, event);
      assertOracle(state, dataRequests.slice(before), `0-switch after ${event}`);
      visited.add(stateName(state));
    }

    expect([...visited].sort()).toEqual([
      'Inspect',
      'LIVE',
      'Paused-from-Inspect',
      'Paused-from-LIVE',
    ]);
  });

  // ==================== 1-switch: all 49 ordered pairs ====================

  const pairs: [EventName, EventName][] = EVENTS.flatMap((first) =>
    EVENTS.map((second) => [first, second] as [EventName, EventName])
  );

  test.each(pairs)('1-switch pair: %s -> %s', async (first, second) => {
    await renderLiveWithData();
    windowSec = 10;

    let state = LIVE;

    const beforeFirst = dataRequests.length;
    const firstApplied = await applyEvent(state, first);
    if (firstApplied) state = nextState(state, first);
    assertOracle(state, dataRequests.slice(beforeFirst), `after 1st (${first})`);
    if (firstApplied) {
      expect(dataRequests.length > beforeFirst, `${first} fetch expectation from LIVE`).toBe(
        expectsFetch(LIVE, first)
      );
    }

    const stateBeforeSecond = state;
    const beforeSecond = dataRequests.length;
    const secondApplied = await applyEvent(state, second);
    if (secondApplied) state = nextState(state, second);
    assertOracle(state, dataRequests.slice(beforeSecond), `after 2nd (${first} -> ${second})`);
    if (secondApplied) {
      expect(
        dataRequests.length > beforeSecond,
        `${second} fetch expectation from ${stateName(stateBeforeSecond)}`
      ).toBe(expectsFetch(stateBeforeSecond, second));
    }

    // Recovery probe: from wherever the pair landed, the user can always get
    // back to a live, data-flowing view. A pair that wedges the UI fails here.
    if (!state.following) {
      await applyEvent(state, 'LiveClick');
      state = nextState(state, 'LiveClick');
    }
    if (!state.running) {
      await applyEvent(state, 'ResumeClick');
      state = nextState(state, 'ResumeClick');
    }
    expect(stateName(state)).toBe('LIVE');

    const beforeProbe = dataRequests.length;
    await applyEvent(state, 'DataTick');
    assertOracle(state, dataRequests.slice(beforeProbe), `probe of ${first} -> ${second}`);
    expect(
      dataRequests.length,
      `pair ${first} -> ${second} wedged the live update loop`
    ).toBeGreaterThan(beforeProbe);
    expect(screen.getByText('ch0:')).toBeDefined();
  });

  // ==================== Inspect-mode request shape ====================

  it('Inspect fetches the inspected range with is_realtime false', async () => {
    await renderLiveWithData();
    windowSec = 10;

    let state = LIVE;
    await applyEvent(state, 'Interact');
    state = nextState(state, 'Interact');
    assertOracle(state, [], 'after Interact');

    // The only fetch path in Inspect is the debounced visible-range change.
    const before = dataRequests.length;
    const chart = lastChartInstance();
    chart.scales.x = { min: 12.5, max: 34.5 };
    await act(async () => {
      lastChartOptions().hooks.setScale[0](chart, 'x');
      await new Promise((resolve) => setTimeout(resolve, 300));
    });

    const issued = dataRequests.slice(before);
    expect(issued.length).toBeGreaterThan(0);
    for (const req of issued) {
      expect(req.is_realtime).toBe(false);
    }
    expect(issued[issued.length - 1].time_min_ms).toBe(12500);
    expect(issued[issued.length - 1].time_max_ms).toBe(34500);
    assertOracle(state, [], 'after scrollback');
  });
});
