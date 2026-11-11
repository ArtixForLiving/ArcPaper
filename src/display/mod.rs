//! Multi-monitor display topology, geometry, and enumeration.

pub mod topology;

pub use topology::{
    DisplayRect, MonitorId, MonitorInfo, RawMonitorRecord, TopologyDiff, VirtualDesktopBounds,
    compute_virtual_bounds, diff_topologies, resolve_stable_monitor_ids,
};

#[cfg(windows)]
pub use topology::enumerate_monitors;
