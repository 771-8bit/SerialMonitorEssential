//! Systematic state-transition tests (Chow 0-switch / 1-switch coverage).
//!
//! Executes §4 of `docs/24_vv_plan.md` against the composed backend state
//! machine of `docs/22_architecture_description.md` §5 (SM-2 DataStore
//! generation × SM-3 PlotterThread, with the aggregator's `enabled` flag from
//! SM-4 as the third axis).
//!
//! # Model
//!
//! State = (store slot, plotter thread, aggregator enabled)
//!
//! | axis            | values                                       |
//! |-----------------|----------------------------------------------|
//! | store slot      | `None` \| `Some(gen k)` (identity = `Arc` ptr)|
//! | plotter thread  | `Stopped` \| `Running`                       |
//! | aggregator      | `enabled` \| `disabled`                      |
//!
//! Reachability under the event alphabet below: `StartPlotter` always sets
//! `Running + enabled` and `StopPlotter` always sets `Stopped + disabled`
//! (that pairing IS the window-lifecycle contract, INV-5). No event in the
//! alphabet moves the two apart, so `(Running, disabled)` and
//! `(Stopped, enabled)` are **unreachable**; the composed machine has
//! 2 (store) × 2 (thread/enabled) = **4 reachable abstract states**, plus the
//! store generation counter. `check()` asserts the coupling after every event,
//! so a regression that breaks it fails here rather than silently widening the
//! state space.
//!
//! # Event alphabet
//!
//! | id | event          | models                                                |
//! |----|----------------|-------------------------------------------------------|
//! | E1 | `AttachStore`  | `open_port` while the slot is empty (SM-2 None→Active) |
//! | E2 | `SwapStore`    | `clear_data` with the port open / port reopen          |
//! | E3 | `DetachStore`  | `clear_data` with the port closed (SM-2 Active→None)   |
//! | E4 | `StartPlotter` | `start_plotter_thread` (full command semantics)        |
//! | E5 | `StopPlotter`  | window `Destroyed` handler / `stop_plotter_thread`     |
//! | E6 | `Data`         | one CSV line arriving in the current store             |
//! | E7 | `Clear`        | `aggregator.clear()` alone (frontend-driven reset)     |
//!
//! # Invariants checked after every event
//!
//! - **version monotonicity** (SYS-NF-106): `check_version().version` never
//!   decreases.
//! - **INV-5**: thread `Running` ⟺ collection enabled.
//! - **INV-follow** (INV-1): while `Running` + store `Some` + enabled, a new
//!   line must reach the aggregator within the timeout.
//! - **INV-halt**: while `Stopped` or store `None` or disabled, a new line must
//!   NOT reach the aggregator (150 ms grace).
//! - **INV-2 / INV-swap-reset**: a store swap observed by an attached thread
//!   drops the previous generation's points (aggregator back to 0).

use crate::plotter::{PlotterAggregator, PlotterThread};
use crate::serial::data_store::DataStore;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Same shape as `plotter::thread::SharedDataStore` (that alias is private to
/// the plotter module).
type SharedStore = Arc<Mutex<Option<Arc<DataStore>>>>;

/// Polling interval for `wait_for` - tight, so the suite stays fast.
const POLL: Duration = Duration::from_millis(5);
/// Upper bound for anything the plotter thread must do (it polls at 10 ms).
const TIMEOUT: Duration = Duration::from_secs(2);
/// How long we let a "must not flow" situation sit before declaring it halted.
const NO_FLOW_GRACE: Duration = Duration::from_millis(150);

/// Wait until `cond` holds, or panic after `TIMEOUT`.
fn wait_for(mut cond: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if cond() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("Timed out after {:?} waiting for: {}", TIMEOUT, what);
        }
        thread::sleep(POLL);
    }
}

/// Latest value plotted on `ch0`, parsed back to a number.
fn latest_ch0_of(aggregator: &PlotterAggregator) -> Option<f64> {
    aggregator
        .get_channel_info()
        .into_iter()
        .find(|info| info.name == "ch0")
        .and_then(|info| info.latest_value)
        .and_then(|value| value.parse::<f64>().ok())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    AttachStore,
    SwapStore,
    DetachStore,
    StartPlotter,
    StopPlotter,
    Data,
    Clear,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::AttachStore => "E1:AttachStore",
            Event::SwapStore => "E2:SwapStore",
            Event::DetachStore => "E3:DetachStore",
            Event::StartPlotter => "E4:StartPlotter",
            Event::StopPlotter => "E5:StopPlotter",
            Event::Data => "E6:Data",
            Event::Clear => "E7:Clear",
        }
    }
}

const ALL_EVENTS: [Event; 7] = [
    Event::AttachStore,
    Event::SwapStore,
    Event::DetachStore,
    Event::StartPlotter,
    Event::StopPlotter,
    Event::Data,
    Event::Clear,
];

/// How many times each oracle actually fired (reported per test, so a
/// refactor that silently makes an oracle vacuous is visible).
#[derive(Debug, Default, Clone, Copy)]
struct OracleStats {
    /// INV-follow assertions (a line had to reach the plot).
    follow: usize,
    /// INV-halt assertions (a line must NOT reach the plot).
    halt: usize,
    /// INV-2 assertions (a swap had to wipe the previous generation).
    swap_reset: usize,
    /// S-2 assertions (a fresh thread had to replay an existing backlog).
    backlog_replay: usize,
}

impl OracleStats {
    fn add(&mut self, other: OracleStats) {
        self.follow += other.follow;
        self.halt += other.halt;
        self.swap_reset += other.swap_reset;
        self.backlog_replay += other.backlog_replay;
    }
}

/// The composed system under test: the shared store slot, the aggregator and
/// (optionally) a running plotter thread - i.e. exactly what `SerialState` +
/// `PlotterState` hold in `lib.rs`, without Tauri.
struct TestEnv {
    handle: SharedStore,
    aggregator: PlotterAggregator,
    thread: Option<PlotterThread>,
    /// Monotonic sequence used to make every pushed line unique.
    seq: u64,
    /// Highest `check_version().version` observed so far.
    last_version: u64,
    /// True once data has been observed flowing from the CURRENT store
    /// instance through the CURRENT thread. This is the only way to know that
    /// the thread has actually resolved (attached to) that instance, which in
    /// turn decides whether the next swap must clear the aggregator: the
    /// initial attach deliberately does NOT clear (§5.3 asymmetry), only
    /// `Attached(k) → Attached(k+1)` does.
    attached: bool,
    /// Description of the event currently being applied (failure messages).
    ctx: String,
    /// Oracle firing counts for this env.
    stats: OracleStats,
}

impl TestEnv {
    fn new() -> Self {
        let aggregator = PlotterAggregator::new();
        // Initial state mirrors the app before the plotter window is opened:
        // no store, no thread, collection disabled.
        assert!(!aggregator.is_enabled(), "aggregator starts disabled");
        Self {
            handle: Arc::new(Mutex::new(None)),
            aggregator,
            thread: None,
            seq: 0,
            last_version: 0,
            attached: false,
            ctx: String::from("<init>"),
            stats: OracleStats::default(),
        }
    }

    fn store(&self) -> Option<Arc<DataStore>> {
        self.handle.lock().expect("store handle poisoned").clone()
    }

    fn set_store(&self, store: Option<Arc<DataStore>>) {
        *self.handle.lock().expect("store handle poisoned") = store;
    }

    fn running(&self) -> bool {
        self.thread.is_some()
    }

    /// The follow condition of INV-1: data pushed now must reach the plot.
    fn follows(&self) -> bool {
        self.running() && self.store().is_some() && self.aggregator.is_enabled()
    }

    /// Latest value plotted on `ch0` (the first CSV column), if any.
    ///
    /// Used as the per-line oracle: it identifies the exact line that arrived,
    /// which `total_points()` alone cannot do. Valid because every test env
    /// stays well below `bucket_size` (10) raw points per channel, so the most
    /// recent point is always still in `raw_buffer`.
    fn latest_ch0(&self) -> Option<f64> {
        latest_ch0_of(&self.aggregator)
    }

    /// Short description of the current abstract state (for failure messages).
    fn state_label(&self) -> String {
        format!(
            "at [{}]: store={}, thread={}, enabled={}",
            self.ctx,
            if self.store().is_some() {
                "Some"
            } else {
                "None"
            },
            if self.running() { "Running" } else { "Stopped" },
            self.aggregator.is_enabled()
        )
    }

    // ==================== Event alphabet ====================

    /// E1 `AttachStore` - a fresh `Arc<DataStore>` lands in the shared slot
    /// (models `open_port` succeeding while no store exists).
    fn attach_store(&mut self) {
        let store = Arc::new(DataStore::new().expect("create DataStore"));
        self.set_store(Some(store));
        // A brand new instance: the thread has not resolved it yet.
        self.attached = false;
    }

    /// E2 `SwapStore` - the slot is replaced by a NEW instance (models
    /// `clear_data` with the port open, and port reopen). Asserts INV-2 when
    /// the thread was demonstrably attached to the previous generation.
    fn swap_store(&mut self) {
        let was_attached = self.attached;
        let pre = self.aggregator.total_points();

        let store = Arc::new(DataStore::new().expect("create DataStore"));
        self.set_store(Some(store));
        self.attached = false;

        // INV-swap-reset (INV-1 + INV-2): an attached thread must notice the
        // new generation within one poll and drop the old generation's points.
        // The new store is empty, so "only post-swap points" == 0 points.
        // Skipped when the thread never attached to the previous instance
        // (then this IS an initial attach, which by design keeps the data).
        if was_attached && self.follows() && pre > 0 {
            let agg = self.aggregator.clone();
            let label = self.state_label();
            wait_for(
                || agg.total_points() == 0,
                &format!("aggregator cleared after store swap, INV-2 ({})", label),
            );
            self.stats.swap_reset += 1;
        }
    }

    /// E3 `DetachStore` - the slot becomes `None` (models `clear_data` with
    /// the port closed).
    fn detach_store(&mut self) {
        self.set_store(None);
        self.attached = false;
    }

    /// E4 `StartPlotter` - full `start_plotter_thread` semantics: stop the old
    /// thread, clear the aggregator, enable collection, start a new thread on
    /// the SHARED handle (not a captured store).
    fn start_plotter(&mut self) {
        if let Some(mut thread) = self.thread.take() {
            thread.stop();
        }
        self.aggregator.clear();
        self.aggregator.set_enabled(true);
        self.thread = Some(PlotterThread::start(
            Arc::clone(&self.handle),
            self.aggregator.clone(),
        ));
        self.attached = false;

        // S-2 (`Connect → PlotterOpen`): a store that already holds data must
        // be replayed from offset 0 by the fresh thread.
        let backlog = self.store().map(|s| s.total_bytes()).unwrap_or(0);
        if backlog > 0 {
            let agg = self.aggregator.clone();
            let label = self.state_label();
            wait_for(
                || agg.total_points() > 0,
                &format!("store backlog ingested after StartPlotter, S-2 ({})", label),
            );
            self.attached = true;
            self.stats.backlog_replay += 1;
        }
    }

    /// E5 `StopPlotter` - the window-destroyed handler: stop the thread AND
    /// disable collection (INV-5). `stop()` joins, so no read can still be in
    /// flight afterwards.
    fn stop_plotter(&mut self) {
        if let Some(mut thread) = self.thread.take() {
            thread.stop();
        }
        self.aggregator.set_enabled(false);
        self.attached = false;
    }

    /// E6 `Data` - one unique CSV line into the CURRENT store, followed by the
    /// INV-follow / INV-halt oracle for the state it was applied in.
    fn data(&mut self) {
        self.seq += 1;
        let line = format!("{},{}\n", self.seq, self.seq * 2);
        let pre = self.aggregator.total_points();

        let Some(store) = self.store() else {
            // No store in the slot: there is nowhere for a line to arrive, so
            // INV-halt holds vacuously and there is nothing to assert.
            return;
        };

        let follows = self.follows();
        // Only a thread that is already attached to THIS store instance is
        // guaranteed not to clear the aggregator while the line is in flight.
        // On an initial attach (`attached == false`) the thread may still be
        // resolving the slot, and going Attached(k) -> Attached(k+1) clears by
        // design (§5.3), so `total_points()` can legitimately drop to 0 and
        // come back - which is why the primary oracle identifies the line
        // itself instead of counting points.
        let strict_growth = self.attached;
        let label = self.state_label();
        let expected = self.seq as f64;
        store.push_test_data(line.as_bytes());
        let agg = self.aggregator.clone();

        if follows {
            // INV-follow (INV-1): this exact line must reach the aggregator
            // within one or two 10 ms polls.
            let probe = self.aggregator.clone();
            wait_for(
                || latest_ch0_of(&probe) == Some(expected),
                &format!("line {} to reach the aggregator ({})", self.seq, label),
            );
            assert!(
                agg.total_points() > 0,
                "INV-follow: the plot is empty right after a line arrived ({})",
                label
            );
            if strict_growth {
                assert!(
                    agg.total_points() > pre,
                    "INV-follow: point count did not grow ({} -> {}) ({})",
                    pre,
                    agg.total_points(),
                    label
                );
            }
            self.attached = true;
            self.stats.follow += 1;
        } else {
            // INV-halt: nothing may appear. `stop()` joined the thread and
            // `set_enabled(false)` gates `add_data_points_batch`, so the grace
            // period only guards against a stray late writer.
            thread::sleep(NO_FLOW_GRACE);
            assert_eq!(
                agg.total_points(),
                pre,
                "INV-halt violated: points appeared while halted ({})",
                label
            );
            self.stats.halt += 1;
        }
    }

    /// E7 `Clear` - `aggregator.clear()` on its own (frontend reset). The
    /// thread stays attached at its current read offset, so later data still
    /// flows.
    fn clear(&mut self) {
        self.aggregator.clear();
    }

    fn apply(&mut self, event: Event) {
        match event {
            Event::AttachStore => self.attach_store(),
            Event::SwapStore => self.swap_store(),
            Event::DetachStore => self.detach_store(),
            Event::StartPlotter => self.start_plotter(),
            Event::StopPlotter => self.stop_plotter(),
            Event::Data => self.data(),
            Event::Clear => self.clear(),
        }
    }

    /// Invariants that must hold in EVERY composed state.
    fn check(&mut self, ctx: &str) {
        // SYS-NF-106: the version counter never runs backwards.
        let version = self.aggregator.check_version().version;
        assert!(
            version >= self.last_version,
            "[{}] version decreased: {} -> {} ({})",
            ctx,
            self.last_version,
            version,
            self.state_label()
        );
        self.last_version = version;

        // INV-5: the thread and the collection flag are switched together.
        assert_eq!(
            self.aggregator.is_enabled(),
            self.running(),
            "[{}] INV-5 violated: enabled/thread out of sync ({})",
            ctx,
            self.state_label()
        );
    }

    fn apply_and_check(&mut self, event: Event, ctx: &str) {
        self.ctx = format!("{} @ {}", event.name(), ctx);
        self.apply(event);
        let label = self.ctx.clone();
        self.check(&label);
    }

    /// Explicit teardown: `PlotterThread` has a `Drop` that stops the thread,
    /// but stopping here keeps the 49 iterations deterministic.
    fn shutdown(&mut self) {
        if let Some(mut thread) = self.thread.take() {
            thread.stop();
        }
        self.aggregator.set_enabled(false);
    }
}

/// Final probe: bring the system back to a live state and prove data flows
/// again. A pair that wedges the system fails here.
fn probe_recovery(env: &mut TestEnv, ctx: &str) {
    if env.store().is_none() {
        env.apply_and_check(Event::AttachStore, &format!("probe of {}", ctx));
    }
    if !env.running() {
        env.apply_and_check(Event::StartPlotter, &format!("probe of {}", ctx));
    }
    assert!(
        env.follows(),
        "[{}] probe failed to reach a live state ({})",
        ctx,
        env.state_label()
    );
    // `data()` asserts INV-follow internally; the extra asserts name the pair.
    env.apply_and_check(Event::Data, &format!("probe of {}", ctx));
    assert_eq!(
        env.latest_ch0(),
        Some(env.seq as f64),
        "[{}] system wedged: the probe line never reached the plot",
        ctx
    );
    assert!(
        env.aggregator.total_points() > 0,
        "[{}] system wedged: the plot is empty after the probe line",
        ctx
    );
}

/// Prefix that brings a fresh `TestEnv` to the state from which `first` is a
/// meaningful transition.
fn canonical_prefix(first: Event) -> &'static [Event] {
    match first {
        // E1 must land on an EMPTY slot to model "open_port when none" - so
        // the prefix only starts the plotter (S-1: PlotterOpen → Connect).
        Event::AttachStore => &[Event::StartPlotter],
        // E4 must land on a STOPPED thread to model a real start. A store with
        // a backlog makes the initial attach observable
        // (S-2: Connect → PlotterOpen; the backlog must be replayed).
        Event::StartPlotter => &[Event::AttachStore, Event::Data],
        // Everything else runs from the canonical armed state:
        // store attached, thread running + enabled, one line already flowed.
        _ => &[Event::AttachStore, Event::StartPlotter, Event::Data],
    }
}

// ==================== 0-switch: every event at least once ====================

/// 0-switch (transition) coverage: one run through the whole event alphabet
/// from a live state, with the invariant oracle after every event.
#[test]
fn zero_switch_all_events() {
    let started = Instant::now();
    let mut env = TestEnv::new();

    // The sequence is chosen so that every event is applied at least once from
    // a state where it is meaningful, and so that both branches of the
    // INV-follow / INV-halt oracle are exercised.
    let sequence: &[(Event, &str)] = &[
        // PlotterOpen before the port is open: thread runs Detached.
        (Event::StartPlotter, "start with no store (Detached)"),
        // S-1: PlotterOpen → Connect. Initial attach, no clear.
        (Event::AttachStore, "attach first store"),
        (Event::Data, "data flows after initial attach"),
        // Frontend reset.
        (Event::Clear, "aggregator clear alone"),
        (Event::Data, "data flows after clear"),
        // S-3 / S-4: Clear with port open / reopen. Aggregator must be cleared.
        (Event::SwapStore, "swap store (INV-2)"),
        (Event::Data, "data flows from the new generation"),
        // S-6: Clear with the port closed. Thread must survive Detached.
        (Event::DetachStore, "detach store"),
        (Event::Data, "no store: nothing to push"),
        // S-8: back to Attached.
        (Event::AttachStore, "re-attach after detach"),
        (Event::Data, "data flows after re-attach"),
        // S-9: window destroyed. INV-5.
        (Event::StopPlotter, "stop plotter (INV-5)"),
        (Event::Data, "INV-halt while stopped"),
        // S-10: window reopened. Backlog must be replayed.
        (Event::StartPlotter, "restart plotter, replay backlog"),
        (Event::Data, "data flows after restart"),
    ];

    for (event, what) in sequence {
        env.apply_and_check(*event, what);
    }

    // Every event in the alphabet was used at least once.
    for event in ALL_EVENTS {
        assert!(
            sequence.iter().any(|(e, _)| *e == event),
            "0-switch coverage gap: {} never applied",
            event.name()
        );
    }

    probe_recovery(&mut env, "zero_switch_all_events");
    env.shutdown();

    // Both branches of the flow oracle must have fired, otherwise the sequence
    // silently stopped proving anything.
    assert!(env.stats.follow >= 5, "INV-follow never exercised enough");
    assert!(env.stats.halt >= 1, "INV-halt never exercised");
    assert!(env.stats.swap_reset >= 1, "INV-2 never exercised");
    assert!(env.stats.backlog_replay >= 1, "S-2 never exercised");
    eprintln!(
        "[0-switch] {} events, oracles {:?}, completed in {:?}",
        sequence.len(),
        env.stats,
        started.elapsed()
    );
}

// ==================== 1-switch: all 49 ordered event pairs ====================

/// 1-switch (transition-pair) coverage: all 7 × 7 ordered pairs of the event
/// alphabet, each from a canonical prefix, with the invariant oracle after
/// every event and a recovery probe at the end.
#[test]
fn one_switch_all_event_pairs() {
    let started = Instant::now();
    let mut executed = 0usize;
    let mut stats = OracleStats::default();

    for first in ALL_EVENTS {
        for second in ALL_EVENTS {
            let pair = format!("({} -> {})", first.name(), second.name());
            // Logged so a panic names the pair that broke.
            eprintln!("[1-switch] {}", pair);

            let mut env = TestEnv::new();
            for prefix in canonical_prefix(first) {
                env.apply_and_check(*prefix, &format!("prefix of {}", pair));
            }
            env.apply_and_check(first, &format!("1st of {}", pair));
            env.apply_and_check(second, &format!("2nd of {}", pair));
            probe_recovery(&mut env, &pair);
            env.shutdown();
            stats.add(env.stats);
            executed += 1;
        }
    }

    assert_eq!(executed, 49, "all ordered pairs must run");
    // Every oracle must have fired somewhere in the sweep.
    assert!(stats.follow >= 49, "INV-follow never exercised enough");
    assert!(stats.halt >= 1, "INV-halt never exercised");
    assert!(stats.swap_reset >= 1, "INV-2 never exercised");
    assert!(stats.backlog_replay >= 1, "S-2 never exercised");
    eprintln!(
        "[1-switch] {} pairs, oracles {:?}, completed in {:?}",
        executed,
        stats,
        started.elapsed()
    );
}
