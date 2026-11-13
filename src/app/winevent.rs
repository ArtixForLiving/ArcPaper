//! Event-driven WinEvent hook monitor and coalescer for auto-pause window tracking (FR-301).
//!
//! Subscribes to Windows accessibility events via `SetWinEventHook` (`WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS`)
//! to observe foreground window transitions, maximize/restore states, minimizations, location changes,
//! and DWM cloaking transitions with low latency (≤ 150 ms p95) and 0.0% CPU when desktop is idle.
//!
//! High-frequency event bursts (e.g. during window dragging or animations) are merged by an `EventCoalescer`
//! using a 40 ms settle window followed by a single 250 ms animation re-check.

use std::time::{Duration, Instant};

#[cfg(windows)]
use std::collections::BTreeMap;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
#[cfg(windows)]
use std::sync::{Arc, RwLock};

#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
#[cfg(windows)]
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EVENT_OBJECT_DESTROY, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, GA_ROOT, GetAncestor, GetMessageW, KillTimer, MSG,
    PEEK_MESSAGE_REMOVE_TYPE, PeekMessageW, PostThreadMessageW, SetTimer, TranslateMessage,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_QUIT, WM_TIMER, WM_USER,
};

#[cfg(windows)]
use crate::app::autopause_monitor::{
    collect_window_observations, evaluate_monitor_coverage, is_window_observable,
};
#[cfg(windows)]
use crate::display::{MonitorId, MonitorInfo};

/// Win32 Event constants monitored by ArcPaper.
pub const EVENT_SYSTEM_FOREGROUND_ID: u32 = 0x0003;
pub const EVENT_SYSTEM_MOVESIZEEND_ID: u32 = 0x000B;
pub const EVENT_SYSTEM_MINIMIZESTART_ID: u32 = 0x0016;
pub const EVENT_SYSTEM_MINIMIZEEND_ID: u32 = 0x0017;
pub const EVENT_OBJECT_DESTROY_ID: u32 = 0x8001;
pub const EVENT_OBJECT_SHOW_ID: u32 = 0x8002;
pub const EVENT_OBJECT_HIDE_ID: u32 = 0x8003;
pub const EVENT_OBJECT_LOCATIONCHANGE_ID: u32 = 0x800B;
pub const EVENT_OBJECT_CLOAKED_ID: u32 = 0x8017;
pub const EVENT_OBJECT_UNCLOAKED_ID: u32 = 0x8018;

/// Standard accessibility object identifier for top-level windows (`OBJID_WINDOW`).
pub const OBJID_WINDOW_ID: i32 = 0;
/// Standard accessibility child identifier indicating the element itself (`CHILDID_SELF`).
pub const CHILDID_SELF_ID: i32 = 0;

/// Default duration (in milliseconds) to coalesce rapid window event bursts.
pub const DEFAULT_SETTLE_MS: u64 = 40;
/// Default duration (in milliseconds) for a single post-transition animation settling check.
pub const DEFAULT_RECHECK_MS: u64 = 250;

/// Name of the dedicated Win32 message pump thread.
pub const WINEVENT_THREAD_NAME: &str = "ArcPaper-WinEventHook";

/// Pure filter verifying if an event ID is in ArcPaper's monitored set.
pub fn is_monitored_event(event: u32) -> bool {
    matches!(
        event,
        EVENT_SYSTEM_FOREGROUND_ID
            | EVENT_SYSTEM_MOVESIZEEND_ID
            | EVENT_SYSTEM_MINIMIZESTART_ID
            | EVENT_SYSTEM_MINIMIZEEND_ID
            | EVENT_OBJECT_DESTROY_ID
            | EVENT_OBJECT_SHOW_ID
            | EVENT_OBJECT_HIDE_ID
            | EVENT_OBJECT_LOCATIONCHANGE_ID
            | EVENT_OBJECT_CLOAKED_ID
            | EVENT_OBJECT_UNCLOAKED_ID
    )
}

/// Pure filter evaluating whether an incoming WinEvent is valid and relevant for auto-pause evaluation.
///
/// Filters out:
/// - Events outside the monitored range
/// - Non-window accessibility objects (`id_object != OBJID_WINDOW`)
/// - Child controls/elements (`id_child != CHILDID_SELF`)
/// - Null window handles (except for `EVENT_OBJECT_DESTROY` where the window may already be torn down)
/// - Non-top-level windows
pub fn should_process_winevent(
    event: u32,
    id_object: i32,
    id_child: i32,
    hwnd_is_null: bool,
    is_top_level: bool,
) -> bool {
    if !is_monitored_event(event) {
        return false;
    }
    if id_object != OBJID_WINDOW_ID || id_child != CHILDID_SELF_ID {
        return false;
    }
    if event != EVENT_OBJECT_DESTROY_ID && (hwnd_is_null || !is_top_level) {
        return false;
    }
    true
}

/// Action to be performed by the monitoring loop when a coalescer timer expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoalescerAction {
    /// 40 ms settle delay elapsed; perform full window observation and coverage evaluation.
    Evaluate,
    /// 250 ms animation re-check delay elapsed; perform second evaluation to catch slow transitions.
    Recheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoalescerPhase {
    Idle,
    Settling(Instant),
    Rechecking(Instant),
}

/// State machine that coalesces rapid window event bursts into a settled evaluation window (40 ms),
/// followed by an optional single re-check (250 ms) if coverage changed.
#[derive(Debug, Clone)]
pub struct EventCoalescer {
    settle_delay: Duration,
    recheck_delay: Duration,
    phase: CoalescerPhase,
}

impl EventCoalescer {
    /// Creates a coalescer with custom settle and recheck durations.
    pub fn new(settle_delay: Duration, recheck_delay: Duration) -> Self {
        Self {
            settle_delay,
            recheck_delay,
            phase: CoalescerPhase::Idle,
        }
    }

    /// Creates a coalescer configured with default timings (40 ms settle, 250 ms re-check).
    pub fn with_default_delays() -> Self {
        Self::new(
            Duration::from_millis(DEFAULT_SETTLE_MS),
            Duration::from_millis(DEFAULT_RECHECK_MS),
        )
    }

    /// Signals a new incoming event at timestamp `now`.
    ///
    /// Extends or starts the 40 ms settle window. If an animation re-check was pending,
    /// it is superseded by the new settle window.
    pub fn trigger(&mut self, now: Instant) {
        self.phase = CoalescerPhase::Settling(now + self.settle_delay);
    }

    /// Returns the active wake deadline, or `None` if idle.
    pub fn next_deadline(&self) -> Option<Instant> {
        match self.phase {
            CoalescerPhase::Settling(deadline) | CoalescerPhase::Rechecking(deadline) => {
                Some(deadline)
            }
            CoalescerPhase::Idle => None,
        }
    }

    /// Returns true if the coalescer is actively waiting for a deadline.
    pub fn is_active(&self) -> bool {
        self.phase != CoalescerPhase::Idle
    }

    /// Polls the coalescer at timestamp `now` to determine if a scheduled action is ready to execute.
    pub fn poll_action(&mut self, now: Instant) -> Option<CoalescerAction> {
        match self.phase {
            CoalescerPhase::Settling(deadline) if now >= deadline => {
                self.phase = CoalescerPhase::Idle;
                Some(CoalescerAction::Evaluate)
            }
            CoalescerPhase::Rechecking(deadline) if now >= deadline => {
                self.phase = CoalescerPhase::Idle;
                Some(CoalescerAction::Recheck)
            }
            _ => None,
        }
    }

    /// Informs the coalescer of the outcome of an evaluation.
    ///
    /// If `state_changed` is true, a single 250 ms re-check is scheduled (unless an incoming event
    /// has already initiated a new settle window). If false, remains in current state.
    pub fn on_evaluation_result(&mut self, now: Instant, state_changed: bool) {
        if state_changed && matches!(self.phase, CoalescerPhase::Idle) {
            self.phase = CoalescerPhase::Rechecking(now + self.recheck_delay);
        }
    }

    /// Resets the coalescer back to `Idle`.
    pub fn reset(&mut self) {
        self.phase = CoalescerPhase::Idle;
    }
}

// ---------------------------------------------------------------------------
// Windows-specific Hook Thread & Win32 Interop
// ---------------------------------------------------------------------------

#[cfg(windows)]
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
#[cfg(windows)]
static DIRTY_FLAG: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static OUR_PID: AtomicU32 = AtomicU32::new(0);

#[cfg(windows)]
const WM_WINEVENT_DIRTY: u32 = WM_USER + 101;
#[cfg(windows)]
const PM_NOREMOVE: PEEK_MESSAGE_REMOVE_TYPE = PEEK_MESSAGE_REMOVE_TYPE(0u32);

/// Checks whether an `HWND` is a top-level window (root ancestor equals self).
///
/// # Safety
/// Caller must ensure `hwnd` is a valid Win32 window handle or null.
#[cfg(windows)]
pub unsafe fn is_top_level_window(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    root == hwnd
}

/// WinEvent callback invoked by Windows on the hook thread.
///
/// Keeps processing strictly minimal: rejects noise, filters non-top-level windows and
/// non-observable location changes, sets `DIRTY_FLAG`, and notifies the thread message loop.
#[cfg(windows)]
unsafe extern "system" fn winevent_proc(
    _hwineventhook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    idobject: i32,
    idchild: i32,
    _ideventthread: u32,
    _dwmseventtime: u32,
) {
    let hwnd_is_null = hwnd.0.is_null();
    let is_top_level = if hwnd_is_null {
        false
    } else {
        unsafe { is_top_level_window(hwnd) }
    };

    if !should_process_winevent(event, idobject, idchild, hwnd_is_null, is_top_level) {
        return;
    }

    let our_pid = OUR_PID.load(Ordering::Relaxed);
    if event == EVENT_OBJECT_LOCATIONCHANGE_ID && !hwnd_is_null {
        let is_observable = unsafe { is_window_observable(hwnd, our_pid) };
        if !is_observable {
            return;
        }
    }

    let thread_id = HOOK_THREAD_ID.load(Ordering::Relaxed);
    if thread_id != 0 && !DIRTY_FLAG.swap(true, Ordering::SeqCst) {
        let _ = unsafe { PostThreadMessageW(thread_id, WM_WINEVENT_DIRTY, WPARAM(0), LPARAM(0)) };
    }
}

/// Handle managing the lifecycle of an active `ArcPaper-WinEventHook` monitor thread.
#[cfg(windows)]
pub struct WinEventMonitorHandle {
    thread_id: u32,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl WinEventMonitorHandle {
    /// Win32 thread ID of the active hook monitor.
    pub fn thread_id(&self) -> u32 {
        self.thread_id
    }

    /// Stops the monitor thread, unhooks accessibility events, and joins the worker.
    pub fn stop(&mut self) {
        if self.thread_id != 0 {
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            self.thread_id = 0;
        }
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
impl Drop for WinEventMonitorHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawns the dedicated `ArcPaper-WinEventHook` background thread with a Win32 message loop.
///
/// Installs hooks for system and object events, coalesces bursts via a 40 ms settle window,
/// schedules single 250 ms re-checks on coverage change, and consumes 0.0% CPU when idle.
#[cfg(windows)]
pub fn spawn_winevent_monitor_thread<F>(
    monitors_snapshot: Arc<RwLock<Vec<MonitorInfo>>>,
    on_cover_change: F,
) -> Result<WinEventMonitorHandle, Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(BTreeMap<MonitorId, bool>) + Send + 'static,
{
    let (tx_ready, rx_ready) = std::sync::mpsc::channel();
    let our_pid = std::process::id();

    let spawn_res = std::thread::Builder::new()
        .name(WINEVENT_THREAD_NAME.into())
        .spawn(move || {
            let thread_id = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
            HOOK_THREAD_ID.store(thread_id, Ordering::SeqCst);
            OUR_PID.store(our_pid, Ordering::SeqCst);
            DIRTY_FLAG.store(false, Ordering::SeqCst);

            // Force creation of the thread's message queue before signalling readiness
            let mut init_msg = MSG::default();
            unsafe {
                let _ = PeekMessageW(&mut init_msg, None, 0, 0, PM_NOREMOVE);
            }

            // Install hooks for foreground/minimize/movesize (0x0003..=0x0017)
            // and object show/hide/destroy/locationchange/cloak (0x8001..=0x8018)
            let hook1 = unsafe {
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_MINIMIZEEND,
                    None,
                    Some(winevent_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            let hook2 = unsafe {
                SetWinEventHook(
                    EVENT_OBJECT_DESTROY,
                    EVENT_OBJECT_UNCLOAKED,
                    None,
                    Some(winevent_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };

            if hook1.0.is_null() && hook2.0.is_null() {
                let _ = tx_ready.send(Err("Failed to install SetWinEventHook".to_string()));
                return;
            }

            let _ = tx_ready.send(Ok(thread_id));

            // Perform initial coverage evaluation immediately upon hook installation
            let initial_monitors = match monitors_snapshot.read() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };
            let initial_observations =
                unsafe { collect_window_observations(&initial_monitors, our_pid) };
            let initial_coverage =
                evaluate_monitor_coverage(&initial_monitors, &initial_observations);
            let mut last_coverage = Some(initial_coverage.clone());
            on_cover_change(initial_coverage);

            let mut msg = MSG::default();
            let mut coalescer = EventCoalescer::with_default_delays();
            let mut active_timer_id: usize = 0;

            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                match msg.message {
                    WM_WINEVENT_DIRTY => {
                        DIRTY_FLAG.store(false, Ordering::SeqCst);
                        let now = Instant::now();
                        coalescer.trigger(now);

                        if active_timer_id != 0 {
                            unsafe {
                                let _ = KillTimer(None, active_timer_id);
                            }
                        }
                        active_timer_id =
                            unsafe { SetTimer(None, 0, DEFAULT_SETTLE_MS as u32, None) };
                    }
                    WM_TIMER => {
                        if active_timer_id != 0 {
                            unsafe {
                                let _ = KillTimer(None, active_timer_id);
                            }
                            active_timer_id = 0;
                        }

                        let now = Instant::now();
                        if let Some(_action) = coalescer.poll_action(now) {
                            let monitors = match monitors_snapshot.read() {
                                Ok(guard) => guard.clone(),
                                Err(poisoned) => poisoned.into_inner().clone(),
                            };
                            let observations =
                                unsafe { collect_window_observations(&monitors, our_pid) };
                            let coverage = evaluate_monitor_coverage(&monitors, &observations);

                            let state_changed = last_coverage.as_ref() != Some(&coverage);
                            if state_changed {
                                last_coverage = Some(coverage.clone());
                                on_cover_change(coverage);
                            }

                            coalescer.on_evaluation_result(now, state_changed);
                        }

                        if let Some(deadline) = coalescer.next_deadline() {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            let delay_ms = (remaining.as_millis() as u32).max(1);
                            active_timer_id = unsafe { SetTimer(None, 0, delay_ms, None) };
                        }
                    }
                    _ => unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    },
                }
            }

            // Teardown: clean up active timers and hooks
            if active_timer_id != 0 {
                unsafe {
                    let _ = KillTimer(None, active_timer_id);
                }
            }
            if !hook1.0.is_null() {
                unsafe {
                    let _ = UnhookWinEvent(hook1);
                }
            }
            if !hook2.0.is_null() {
                unsafe {
                    let _ = UnhookWinEvent(hook2);
                }
            }
            HOOK_THREAD_ID.store(0, Ordering::SeqCst);
            DIRTY_FLAG.store(false, Ordering::SeqCst);
        });

    let join_handle = spawn_res?;
    match rx_ready.recv_timeout(Duration::from_millis(2000)) {
        Ok(Ok(thread_id)) => Ok(WinEventMonitorHandle {
            thread_id,
            thread_handle: Some(join_handle),
        }),
        Ok(Err(err_msg)) => Err(err_msg.into()),
        Err(e) => {
            Err(format!("Timeout waiting for WinEventHook thread initialization: {e}").into())
        }
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_winevent_filter_accepts_all_monitored_events() {
        let monitored = [
            EVENT_SYSTEM_FOREGROUND_ID,
            EVENT_SYSTEM_MOVESIZEEND_ID,
            EVENT_SYSTEM_MINIMIZESTART_ID,
            EVENT_SYSTEM_MINIMIZEEND_ID,
            EVENT_OBJECT_DESTROY_ID,
            EVENT_OBJECT_SHOW_ID,
            EVENT_OBJECT_HIDE_ID,
            EVENT_OBJECT_LOCATIONCHANGE_ID,
            EVENT_OBJECT_CLOAKED_ID,
            EVENT_OBJECT_UNCLOAKED_ID,
        ];

        for &event in &monitored {
            assert!(
                is_monitored_event(event),
                "event 0x{event:04X} should be recognised as monitored"
            );
            assert!(
                should_process_winevent(
                    event,
                    OBJID_WINDOW_ID,
                    CHILDID_SELF_ID,
                    false, // hwnd not null
                    true   // is top-level
                ),
                "event 0x{event:04X} should pass filter for valid top-level window"
            );
        }
    }

    #[test]
    fn test_winevent_filter_rejects_unmonitored_events() {
        let unmonitored = [
            0x0001, // EVENT_SYSTEM_SOUND
            0x0002, // EVENT_SYSTEM_ALERT
            0x0004, // EVENT_SYSTEM_MENUSTART
            0x0012, // EVENT_SYSTEM_DRAGDROPSTART
            0x8000, // EVENT_OBJECT_CREATE
            0x8004, // EVENT_OBJECT_REORDER
            0x800C, // EVENT_OBJECT_NAMECHANGE
            0x800E, // EVENT_OBJECT_VALUECHANGE
            0x9000, // Custom / unknown
        ];

        for &event in &unmonitored {
            assert!(
                !is_monitored_event(event),
                "event 0x{event:04X} must not be monitored"
            );
            assert!(
                !should_process_winevent(event, OBJID_WINDOW_ID, CHILDID_SELF_ID, false, true),
                "event 0x{event:04X} must be rejected by filter"
            );
        }
    }

    #[test]
    fn test_winevent_noise_rejection_child_and_non_window_objects() {
        let objid_client = -4;
        let objid_caret = -8;
        let child_element = 1;

        // Non-window object IDs
        assert!(!should_process_winevent(
            EVENT_OBJECT_LOCATIONCHANGE_ID,
            objid_client,
            CHILDID_SELF_ID,
            false,
            true
        ));
        assert!(!should_process_winevent(
            EVENT_OBJECT_LOCATIONCHANGE_ID,
            objid_caret,
            CHILDID_SELF_ID,
            false,
            true
        ));

        // Child elements
        assert!(!should_process_winevent(
            EVENT_OBJECT_LOCATIONCHANGE_ID,
            OBJID_WINDOW_ID,
            child_element,
            false,
            true
        ));

        // Null HWND
        assert!(!should_process_winevent(
            EVENT_OBJECT_LOCATIONCHANGE_ID,
            OBJID_WINDOW_ID,
            CHILDID_SELF_ID,
            true, // null HWND
            true
        ));

        // Non top-level window
        assert!(!should_process_winevent(
            EVENT_OBJECT_LOCATIONCHANGE_ID,
            OBJID_WINDOW_ID,
            CHILDID_SELF_ID,
            false,
            false // not top-level
        ));
    }

    #[test]
    fn test_winevent_destroy_allows_null_or_invalid_hwnd() {
        // Destroyed windows may already be cleared by the OS
        assert!(should_process_winevent(
            EVENT_OBJECT_DESTROY_ID,
            OBJID_WINDOW_ID,
            CHILDID_SELF_ID,
            true,  // null hwnd
            false  // not top-level
        ));
    }

    #[test]
    fn test_coalescer_burst_merging_within_settle_window() {
        let mut coalescer = EventCoalescer::with_default_delays();
        let t0 = Instant::now();

        // Initial event
        coalescer.trigger(t0);
        assert!(coalescer.is_active());
        assert_eq!(
            coalescer.next_deadline(),
            Some(t0 + Duration::from_millis(40))
        );

        // Rapid events extending deadline: t=10ms, t=20ms, t=30ms
        let t10 = t0 + Duration::from_millis(10);
        coalescer.trigger(t10);
        assert_eq!(
            coalescer.next_deadline(),
            Some(t10 + Duration::from_millis(40))
        );

        let t30 = t0 + Duration::from_millis(30);
        coalescer.trigger(t30);
        let final_settle = t30 + Duration::from_millis(40); // t0 + 70ms
        assert_eq!(coalescer.next_deadline(), Some(final_settle));

        // Polling before settle deadline returns None
        assert_eq!(coalescer.poll_action(t0 + Duration::from_millis(69)), None);
        assert!(coalescer.is_active());

        // Polling at or after settle deadline returns Evaluate
        assert_eq!(
            coalescer.poll_action(final_settle),
            Some(CoalescerAction::Evaluate)
        );
        assert!(!coalescer.is_active());
    }

    #[test]
    fn test_coalescer_schedules_single_250ms_recheck_on_state_change() {
        let mut coalescer = EventCoalescer::with_default_delays();
        let t0 = Instant::now();

        coalescer.trigger(t0);
        let t40 = t0 + Duration::from_millis(40);
        assert_eq!(coalescer.poll_action(t40), Some(CoalescerAction::Evaluate));

        // State changed: triggers 250ms re-check
        coalescer.on_evaluation_result(t40, true);
        assert!(coalescer.is_active());
        assert_eq!(
            coalescer.next_deadline(),
            Some(t40 + Duration::from_millis(250))
        );

        // Before 250ms recheck expires
        assert_eq!(
            coalescer.poll_action(t40 + Duration::from_millis(200)),
            None
        );

        // At 250ms recheck expiration
        let t290 = t40 + Duration::from_millis(250);
        assert_eq!(coalescer.poll_action(t290), Some(CoalescerAction::Recheck));

        // Recheck completes without further state change: transitions to Idle (0.0% CPU)
        coalescer.on_evaluation_result(t290, false);
        assert!(!coalescer.is_active());
        assert_eq!(coalescer.next_deadline(), None);
    }

    #[test]
    fn test_coalescer_no_recheck_when_state_unchanged() {
        let mut coalescer = EventCoalescer::with_default_delays();
        let t0 = Instant::now();

        coalescer.trigger(t0);
        let t40 = t0 + Duration::from_millis(40);
        assert_eq!(coalescer.poll_action(t40), Some(CoalescerAction::Evaluate));

        // Coverage state did not change
        coalescer.on_evaluation_result(t40, false);
        assert!(!coalescer.is_active());
        assert_eq!(coalescer.next_deadline(), None);
    }

    #[test]
    fn test_coalescer_event_burst_interrupts_pending_recheck() {
        let mut coalescer = EventCoalescer::with_default_delays();
        let t0 = Instant::now();

        coalescer.trigger(t0);
        let t40 = t0 + Duration::from_millis(40);
        let _ = coalescer.poll_action(t40);
        coalescer.on_evaluation_result(t40, true); // Rechecking until t40 + 250ms = t290

        // New event arrives at t=100ms during pending recheck
        let t100 = t0 + Duration::from_millis(100);
        coalescer.trigger(t100);

        // Should switch back to Settling at t100 + 40ms = t140
        assert_eq!(
            coalescer.next_deadline(),
            Some(t100 + Duration::from_millis(40))
        );
        let t140 = t100 + Duration::from_millis(40);
        assert_eq!(coalescer.poll_action(t140), Some(CoalescerAction::Evaluate));
    }

    #[test]
    fn test_coalescer_reset_and_initial_idle_state() {
        let mut coalescer = EventCoalescer::with_default_delays();
        assert!(!coalescer.is_active());
        assert_eq!(coalescer.next_deadline(), None);
        assert_eq!(coalescer.poll_action(Instant::now()), None);

        coalescer.trigger(Instant::now());
        assert!(coalescer.is_active());
        coalescer.reset();
        assert!(!coalescer.is_active());
        assert_eq!(coalescer.next_deadline(), None);
    }

    #[test]
    fn test_winevent_thread_name_constant() {
        assert_eq!(WINEVENT_THREAD_NAME, "ArcPaper-WinEventHook");
    }

    #[cfg(windows)]
    #[test]
    fn test_spawn_winevent_monitor_thread_lifecycle_smoke() {
        let monitors = Arc::new(RwLock::new(vec![MonitorInfo {
            id: MonitorId("SMOKE_MON".into()),
            friendly_name: "Smoke Monitor".into(),
            gdi_name: r"\\.\DISPLAY1".into(),
            rect: crate::display::DisplayRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: true,
        }]));

        let (tx, rx) = std::sync::mpsc::channel();
        let handle_res = spawn_winevent_monitor_thread(monitors, move |cov| {
            let _ = tx.send(cov);
        });

        assert!(
            handle_res.is_ok(),
            "Hook monitor thread should spawn successfully"
        );
        let mut handle = handle_res.unwrap();
        assert_ne!(handle.thread_id(), 0);

        // Initial coverage should have been emitted
        let cov = rx.recv_timeout(Duration::from_millis(1500));
        assert!(
            cov.is_ok(),
            "Initial coverage should be emitted upon thread start"
        );

        // Safe teardown
        handle.stop();
    }
}
