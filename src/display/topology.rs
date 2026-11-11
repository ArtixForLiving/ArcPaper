//! Multi-monitor topology definitions, geometry calculations, and hardware enumeration.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

#[cfg(windows)]
use windows::Win32::Foundation::{LPARAM, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITORINFOEXW,
};
#[cfg(windows)]
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
#[cfg(windows)]
use windows::core::BOOL;

#[cfg(windows)]
const MONITORINFOF_PRIMARY: u32 = 1;
#[cfg(windows)]
const EDD_GET_DEVICE_INTERFACE_NAME: u32 = 1;

/// Unique, stable identifier for a display monitor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonitorId(pub String);

impl MonitorId {
    /// Reference to the underlying ID string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MonitorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for MonitorId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for MonitorId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for MonitorId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// 2D rectangle in virtual desktop screen space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DisplayRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl DisplayRect {
    /// Rectangle pixel width (clamped to non-negative).
    pub fn width(self) -> i32 {
        (self.right - self.left).max(0)
    }

    /// Rectangle pixel height (clamped to non-negative).
    pub fn height(self) -> i32 {
        (self.bottom - self.top).max(0)
    }

    /// Whether width or height is zero.
    pub fn is_empty(self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// Compute intersection area with another rectangle in square pixels.
    pub fn intersection_area(self, other: DisplayRect) -> i64 {
        let inter_left = self.left.max(other.left);
        let inter_right = self.right.min(other.right);
        let inter_top = self.top.max(other.top);
        let inter_bottom = self.bottom.min(other.bottom);
        let w = (inter_right - inter_left).max(0) as i64;
        let h = (inter_bottom - inter_top).max(0) as i64;
        w * h
    }
}

/// Resolved monitor metadata with geometry and stable identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub id: MonitorId,
    pub friendly_name: String,
    pub gdi_name: String,
    pub rect: DisplayRect,
    pub dpi: u32,
    pub is_primary: bool,
}

/// Bounding rectangle encompassing all active monitors in the virtual desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VirtualDesktopBounds {
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: u32,
    pub height: u32,
}

/// Structural differences between two display topology snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TopologyDiff {
    pub added: Vec<MonitorId>,
    pub removed: Vec<MonitorId>,
    pub changed: Vec<MonitorId>,
    pub unchanged: Vec<MonitorId>,
}

impl TopologyDiff {
    /// True if any monitors were added, removed, or changed.
    pub fn has_changes(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty() || !self.changed.is_empty()
    }
}

/// Coalesces burst display change and DPI notifications into a single debounced event.
#[derive(Debug, Clone)]
pub struct DisplayDebounce {
    delay: std::time::Duration,
    pending_deadline: Option<std::time::Instant>,
}

impl DisplayDebounce {
    /// Creates a new debounce timer with the specified quiet window duration.
    pub fn new(delay: std::time::Duration) -> Self {
        Self {
            delay,
            pending_deadline: None,
        }
    }

    /// Triggers or extends the debounce deadline to `now + delay`.
    pub fn trigger_at(&mut self, now: std::time::Instant) {
        self.pending_deadline = Some(now + self.delay);
    }

    /// Returns the active deadline if a debounce window is currently pending.
    pub fn next_deadline(&self) -> Option<std::time::Instant> {
        self.pending_deadline
    }

    /// Returns true if a debounce window is currently active and waiting to elapse.
    pub fn is_pending(&self) -> bool {
        self.pending_deadline.is_some()
    }

    /// Returns true if the debounce window has elapsed, resetting internal state to idle.
    pub fn poll_ready(&mut self, now: std::time::Instant) -> bool {
        match self.pending_deadline {
            Some(deadline) if now >= deadline => {
                self.pending_deadline = None;
                true
            }
            _ => false,
        }
    }
}

/// Raw record read from hardware/OS enumeration before stabilization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMonitorRecord {
    pub gdi_name: String,
    pub friendly_name: String,
    pub device_interface_id: String,
    pub legacy_device_id: String,
    pub rect: DisplayRect,
    pub dpi: u32,
    pub is_primary: bool,
}

/// Trims whitespace and NUL characters, discarding anything after an embedded NUL.
fn sanitize_str(s: &str) -> String {
    let trimmed = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    match trimmed.split_once('\0') {
        Some((prefix, _)) => prefix
            .trim_end_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_string(),
        None => trimmed.to_string(),
    }
}

#[cfg(windows)]
fn utf16_slice_to_string(slice: &[u16]) -> String {
    let len = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
    String::from_utf16_lossy(&slice[..len])
}

struct IntermediateRecord {
    friendly_name: String,
    gdi_name: String,
    rect: DisplayRect,
    dpi: u32,
    is_primary: bool,
}

/// Resolves raw monitor records into deterministic, stable `MonitorInfo` structures.
pub fn resolve_stable_monitor_ids(records: Vec<RawMonitorRecord>) -> Vec<MonitorInfo> {
    let mut groups: BTreeMap<String, Vec<IntermediateRecord>> = BTreeMap::new();

    for r in records {
        let gdi_name = sanitize_str(&r.gdi_name);
        let friendly_sanitized = sanitize_str(&r.friendly_name);
        let friendly_name = if !friendly_sanitized.is_empty() {
            friendly_sanitized
        } else {
            gdi_name.clone()
        };

        let dev_iface = sanitize_str(&r.device_interface_id);
        let legacy = sanitize_str(&r.legacy_device_id);

        let base_id = if !dev_iface.is_empty() {
            dev_iface
        } else if !legacy.is_empty() {
            legacy
        } else if !gdi_name.is_empty() {
            gdi_name.clone()
        } else {
            "UNKNOWN_DISPLAY".to_string()
        };

        groups.entry(base_id).or_default().push(IntermediateRecord {
            friendly_name,
            gdi_name,
            rect: r.rect,
            dpi: r.dpi,
            is_primary: r.is_primary,
        });
    }

    let mut resolved = Vec::new();

    for (base_id, mut list) in groups {
        if list.len() == 1 {
            let item = list.remove(0);
            resolved.push(MonitorInfo {
                id: MonitorId(base_id),
                friendly_name: item.friendly_name,
                gdi_name: item.gdi_name,
                rect: item.rect,
                dpi: item.dpi,
                is_primary: item.is_primary,
            });
        } else {
            list.sort_by(|a, b| {
                a.rect
                    .left
                    .cmp(&b.rect.left)
                    .then_with(|| a.rect.top.cmp(&b.rect.top))
                    .then_with(|| a.gdi_name.cmp(&b.gdi_name))
            });

            for (index, item) in list.into_iter().enumerate() {
                let suffix = index + 1;
                resolved.push(MonitorInfo {
                    id: MonitorId(format!("{base_id}#{suffix}")),
                    friendly_name: item.friendly_name,
                    gdi_name: item.gdi_name,
                    rect: item.rect,
                    dpi: item.dpi,
                    is_primary: item.is_primary,
                });
            }
        }
    }

    // Sort final list deterministically: primary monitor first, then (rect.left, rect.top, id)
    resolved.sort_by(|a, b| {
        b.is_primary
            .cmp(&a.is_primary)
            .then_with(|| a.rect.left.cmp(&b.rect.left))
            .then_with(|| a.rect.top.cmp(&b.rect.top))
            .then_with(|| a.id.cmp(&b.id))
    });

    resolved
}

/// Computes the bounding rectangle encompassing all monitors.
pub fn compute_virtual_bounds(monitors: &[MonitorInfo]) -> VirtualDesktopBounds {
    if monitors.is_empty() {
        return VirtualDesktopBounds::default();
    }

    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;

    for m in monitors {
        min_x = min_x.min(m.rect.left);
        min_y = min_y.min(m.rect.top);
        max_x = max_x.max(m.rect.right);
        max_y = max_y.max(m.rect.bottom);
    }

    let width = (max_x as i64 - min_x as i64).max(0) as u32;
    let height = (max_y as i64 - min_y as i64).max(0) as u32;

    VirtualDesktopBounds {
        origin_x: min_x,
        origin_y: min_y,
        width,
        height,
    }
}

/// Diffs previous and next monitor topologies.
pub fn diff_topologies(prev: &[MonitorInfo], next: &[MonitorInfo]) -> TopologyDiff {
    let prev_map: HashMap<&MonitorId, &MonitorInfo> = prev.iter().map(|m| (&m.id, m)).collect();
    let next_map: HashMap<&MonitorId, &MonitorInfo> = next.iter().map(|m| (&m.id, m)).collect();

    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut unchanged = Vec::new();

    for m_next in next {
        if let Some(m_prev) = prev_map.get(&m_next.id) {
            if m_prev.rect != m_next.rect
                || m_prev.dpi != m_next.dpi
                || m_prev.is_primary != m_next.is_primary
            {
                changed.push(m_next.id.clone());
            } else {
                unchanged.push(m_next.id.clone());
            }
        } else {
            added.push(m_next.id.clone());
        }
    }

    let mut removed = Vec::new();
    for m_prev in prev {
        if !next_map.contains_key(&m_prev.id) {
            removed.push(m_prev.id.clone());
        }
    }

    TopologyDiff {
        added,
        removed,
        changed,
        unchanged,
    }
}

#[cfg(windows)]
unsafe extern "system" fn monitor_enum_proc(
    hmon: HMONITOR,
    _hdc: HDC,
    _lprect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let records = unsafe { &mut *(lparam.0 as *mut Vec<RawMonitorRecord>) };

    let mut mi: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(hmon, &mut mi.monitorInfo) }.as_bool() {
        return BOOL(1);
    }

    let is_primary = (mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;
    let rect = DisplayRect {
        left: mi.monitorInfo.rcMonitor.left,
        top: mi.monitorInfo.rcMonitor.top,
        right: mi.monitorInfo.rcMonitor.right,
        bottom: mi.monitorInfo.rcMonitor.bottom,
    };

    let gdi_name = utf16_slice_to_string(&mi.szDevice);

    let mut dpi_x = 0u32;
    let mut dpi_y = 0u32;
    let dpi = if unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
        .is_ok()
        && dpi_x > 0
    {
        dpi_x
    } else {
        96
    };

    let dev_name = windows::core::PCWSTR::from_raw(mi.szDevice.as_ptr());

    // 1. Query device interface ID
    let mut dd_iface: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
    dd_iface.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
    let has_iface =
        unsafe { EnumDisplayDevicesW(dev_name, 0, &mut dd_iface, EDD_GET_DEVICE_INTERFACE_NAME) }
            .as_bool();
    let device_interface_id = if has_iface {
        utf16_slice_to_string(&dd_iface.DeviceID)
    } else {
        String::new()
    };

    // 2. Query legacy device ID and friendly name
    let mut dd_legacy: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
    dd_legacy.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
    let has_legacy = unsafe { EnumDisplayDevicesW(dev_name, 0, &mut dd_legacy, 0) }.as_bool();
    let (legacy_device_id, friendly_name) = if has_legacy {
        (
            utf16_slice_to_string(&dd_legacy.DeviceID),
            utf16_slice_to_string(&dd_legacy.DeviceString),
        )
    } else {
        (String::new(), String::new())
    };

    records.push(RawMonitorRecord {
        gdi_name,
        friendly_name,
        device_interface_id,
        legacy_device_id,
        rect,
        dpi,
        is_primary,
    });

    BOOL(1)
}

/// Enumerates connected Windows displays via Win32 GDI and HiDPI APIs.
#[cfg(windows)]
pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    let mut records = Vec::new();
    let lparam = LPARAM((&raw mut records) as isize);
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_enum_proc), lparam);
    }
    resolve_stable_monitor_ids(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_bounds_with_negative_and_l_shaped_monitors() {
        // Empty monitor list returns default (0, 0, 0, 0)
        assert_eq!(compute_virtual_bounds(&[]), VirtualDesktopBounds::default());

        let mon_b = MonitorInfo {
            id: MonitorId("MON_B".into()),
            friendly_name: "Side Monitor".into(),
            gdi_name: r"\\.\DISPLAY2".into(),
            rect: DisplayRect {
                left: -1920,
                top: -360,
                right: 0,
                bottom: 720,
            },
            dpi: 96,
            is_primary: false,
        };

        let mon_a = MonitorInfo {
            id: MonitorId("MON_A".into()),
            friendly_name: "Main Monitor".into(),
            gdi_name: r"\\.\DISPLAY1".into(),
            rect: DisplayRect {
                left: 0,
                top: 0,
                right: 3840,
                bottom: 2160,
            },
            dpi: 144,
            is_primary: true,
        };

        let bounds = compute_virtual_bounds(&[mon_b, mon_a]);
        assert_eq!(
            bounds,
            VirtualDesktopBounds {
                origin_x: -1920,
                origin_y: -360,
                width: 5760,
                height: 2520,
            }
        );

        // Verify DisplayRect helper methods
        let rect = DisplayRect {
            left: 100,
            top: 200,
            right: 1100,
            bottom: 800,
        };
        assert_eq!(rect.width(), 1000);
        assert_eq!(rect.height(), 600);
        assert!(!rect.is_empty());

        let empty_rect = DisplayRect {
            left: 50,
            top: 50,
            right: 50,
            bottom: 100,
        };
        assert_eq!(empty_rect.width(), 0);
        assert!(empty_rect.is_empty());

        let other = DisplayRect {
            left: 500,
            top: 400,
            right: 1500,
            bottom: 1000,
        };
        // Overlap: x in [500, 1100) -> 600, y in [400, 800) -> 400 => area 240,000
        assert_eq!(rect.intersection_area(other), 240_000);

        let disjoint = DisplayRect {
            left: 2000,
            top: 2000,
            right: 3000,
            bottom: 3000,
        };
        assert_eq!(rect.intersection_area(disjoint), 0);
    }

    #[test]
    fn test_resolve_stable_ids_handles_duplicates_and_empty_device_ids() {
        // Unique device_interface_id keeps clean ID without suffix
        let r1 = RawMonitorRecord {
            gdi_name: r"\\.\DISPLAY1".into(),
            friendly_name: "Dell UltraSharp".into(),
            device_interface_id: "  \\\\?\\DISPLAY#DEL1234#5&1234#{e6f07b4f}\0  ".into(),
            legacy_device_id: r"MONITOR\DEL1234\0".into(),
            rect: DisplayRect {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1440,
            },
            dpi: 120,
            is_primary: true,
        };

        // Identical monitor 1: legacy_device_id collision
        let r2 = RawMonitorRecord {
            gdi_name: r"\\.\DISPLAY2".into(),
            friendly_name: "Samsung 4K".into(),
            device_interface_id: "".into(),
            legacy_device_id: "MONITOR\\SAM0001\0".into(),
            rect: DisplayRect {
                left: 2560,
                top: 0,
                right: 6400,
                bottom: 2160,
            },
            dpi: 144,
            is_primary: false,
        };

        // Identical monitor 2: legacy_device_id collision, negative left
        let r3 = RawMonitorRecord {
            gdi_name: r"\\.\DISPLAY3".into(),
            friendly_name: "".into(), // empty friendly_name should fallback to gdi_name
            device_interface_id: "".into(),
            legacy_device_id: r"MONITOR\SAM0001".into(),
            rect: DisplayRect {
                left: -3840,
                top: 0,
                right: 0,
                bottom: 2160,
            },
            dpi: 144,
            is_primary: false,
        };

        // Empty IDs fallback to UNKNOWN_DISPLAY (#1 and #2)
        let r4 = RawMonitorRecord {
            gdi_name: "".into(),
            friendly_name: "".into(),
            device_interface_id: " \0 ".into(),
            legacy_device_id: "".into(),
            rect: DisplayRect {
                left: 6400,
                top: 0,
                right: 8320,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: false,
        };

        let r5 = RawMonitorRecord {
            gdi_name: "".into(),
            friendly_name: "".into(),
            device_interface_id: "".into(),
            legacy_device_id: "".into(),
            rect: DisplayRect {
                left: 8320,
                top: 0,
                right: 10240,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: false,
        };

        // Pass in shuffled order
        let input = vec![r5.clone(), r3.clone(), r1.clone(), r4.clone(), r2.clone()];
        let resolved = resolve_stable_monitor_ids(input);

        assert_eq!(resolved.len(), 5);

        // Deterministic ordering: primary first, then (rect.left, rect.top, id)
        // 1. Primary: r1 (rect.left: 0) -> ID "\\?\DISPLAY#DEL1234#5&1234#{e6f07b4f}" (no suffix)
        assert_eq!(
            resolved[0].id.as_str(),
            r"\\?\DISPLAY#DEL1234#5&1234#{e6f07b4f}"
        );
        assert!(resolved[0].is_primary);
        assert_eq!(resolved[0].friendly_name, "Dell UltraSharp");

        // 2. r3: rect.left -3840 -> collision on MONITOR\SAM0001, sorted before r2 (2560), so #1
        assert_eq!(resolved[1].id.as_str(), r"MONITOR\SAM0001#1");
        assert_eq!(resolved[1].rect.left, -3840);
        // Fallback friendly name when empty is gdi_name
        assert_eq!(resolved[1].friendly_name, r"\\.\DISPLAY3");

        // 3. r2: rect.left 2560 -> collision on MONITOR\SAM0001, so #2
        assert_eq!(resolved[2].id.as_str(), r"MONITOR\SAM0001#2");
        assert_eq!(resolved[2].rect.left, 2560);
        assert_eq!(resolved[2].friendly_name, "Samsung 4K");

        // 4. r4: rect.left 6400 -> UNKNOWN_DISPLAY#1
        assert_eq!(resolved[3].id.as_str(), "UNKNOWN_DISPLAY#1");
        assert_eq!(resolved[3].rect.left, 6400);

        // 5. r5: rect.left 8320 -> UNKNOWN_DISPLAY#2
        assert_eq!(resolved[4].id.as_str(), "UNKNOWN_DISPLAY#2");
        assert_eq!(resolved[4].rect.left, 8320);

        // Same result if input order is different
        let resolved_alt = resolve_stable_monitor_ids(vec![r1, r2, r3, r4, r5]);
        assert_eq!(resolved, resolved_alt);
    }

    #[test]
    fn test_topology_diff_detects_add_remove_and_dpi_change() {
        let mon1 = MonitorInfo {
            id: MonitorId("M1".into()),
            friendly_name: "Display 1".into(),
            gdi_name: r"\\.\DISPLAY1".into(),
            rect: DisplayRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: true,
        };

        let mon2 = MonitorInfo {
            id: MonitorId("M2".into()),
            friendly_name: "Display 2".into(),
            gdi_name: r"\\.\DISPLAY2".into(),
            rect: DisplayRect {
                left: 1920,
                top: 0,
                right: 3840,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: false,
        };

        let mon3 = MonitorInfo {
            id: MonitorId("M3".into()),
            friendly_name: "Display 3".into(),
            gdi_name: r"\\.\DISPLAY3".into(),
            rect: DisplayRect {
                left: 3840,
                top: 0,
                right: 5760,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: false,
        };

        let prev = vec![mon1.clone(), mon2.clone(), mon3.clone()];

        // No changes diff
        let diff_same = diff_topologies(&prev, &prev);
        assert!(!diff_same.has_changes());
        assert!(diff_same.added.is_empty());
        assert!(diff_same.removed.is_empty());
        assert!(diff_same.changed.is_empty());
        assert_eq!(diff_same.unchanged.len(), 3);

        // mon2 has DPI change (96 -> 144)
        let mut mon2_changed = mon2.clone();
        mon2_changed.dpi = 144;

        // mon4 is added
        let mon4 = MonitorInfo {
            id: MonitorId("M4".into()),
            friendly_name: "Display 4".into(),
            gdi_name: r"\\.\DISPLAY4".into(),
            rect: DisplayRect {
                left: -1920,
                top: 0,
                right: 0,
                bottom: 1080,
            },
            dpi: 96,
            is_primary: false,
        };

        // Next topology: mon1 (unchanged), mon2_changed (changed), mon4 (added). mon3 is removed.
        let next = vec![mon1.clone(), mon2_changed, mon4.clone()];

        let diff = diff_topologies(&prev, &next);
        assert!(diff.has_changes());
        assert_eq!(diff.added, vec![MonitorId("M4".into())]);
        assert_eq!(diff.removed, vec![MonitorId("M3".into())]);
        assert_eq!(diff.changed, vec![MonitorId("M2".into())]);
        assert_eq!(diff.unchanged, vec![MonitorId("M1".into())]);
    }

    #[cfg(windows)]
    #[test]
    fn test_enumerate_monitors_sanity() {
        let monitors = enumerate_monitors();
        // If run in an environment with connected displays, verify sanity
        if !monitors.is_empty() {
            let primary_count = monitors.iter().filter(|m| m.is_primary).count();
            assert_eq!(primary_count, 1, "Expected exactly 1 primary monitor");

            for m in &monitors {
                assert!(!m.id.as_str().is_empty(), "Monitor ID must not be empty");
                assert!(m.rect.width() > 0, "Monitor width must be positive");
                assert!(m.rect.height() > 0, "Monitor height must be positive");
                assert!(m.dpi >= 96, "Monitor DPI must be >= 96");
            }

            // Ensure all monitor IDs are unique
            let mut ids = monitors.iter().map(|m| &m.id).collect::<Vec<_>>();
            ids.sort();
            let orig_len = ids.len();
            ids.dedup();
            assert_eq!(ids.len(), orig_len, "All monitor IDs must be unique");
        }
    }

    #[test]
    fn test_display_debounce_coalesces_burst_events() {
        let mut debounce = DisplayDebounce::new(std::time::Duration::from_millis(200));
        let start = std::time::Instant::now();

        assert!(!debounce.is_pending());
        assert_eq!(debounce.next_deadline(), None);
        assert!(!debounce.poll_ready(start));

        // First trigger at T+0ms
        debounce.trigger_at(start);
        assert!(debounce.is_pending());
        assert_eq!(
            debounce.next_deadline(),
            Some(start + std::time::Duration::from_millis(200))
        );
        assert!(!debounce.poll_ready(start + std::time::Duration::from_millis(50)));

        // Second burst trigger at T+50ms (extends deadline to T+250ms)
        let t50 = start + std::time::Duration::from_millis(50);
        debounce.trigger_at(t50);
        assert_eq!(
            debounce.next_deadline(),
            Some(t50 + std::time::Duration::from_millis(200))
        );

        // Third burst trigger at T+120ms (extends deadline to T+320ms)
        let t120 = start + std::time::Duration::from_millis(120);
        debounce.trigger_at(t120);
        let final_deadline = t120 + std::time::Duration::from_millis(200);
        assert_eq!(debounce.next_deadline(), Some(final_deadline));

        // At T+300ms, not ready yet
        assert!(!debounce.poll_ready(start + std::time::Duration::from_millis(300)));
        assert!(debounce.is_pending());

        // At T+320ms (exact deadline), ready!
        assert!(debounce.poll_ready(final_deadline));
        // After being consumed, it resets
        assert!(!debounce.is_pending());
        assert_eq!(debounce.next_deadline(), None);
        assert!(!debounce.poll_ready(final_deadline + std::time::Duration::from_millis(10)));
    }
}
