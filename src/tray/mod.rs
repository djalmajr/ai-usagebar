//! Windows system-tray popover over `usage --json`.
//!
//! View-model helpers compile on every OS so Linux CI can test them. The
//! NotifyIcon + WebView2 event loop is Windows-only and never pulled into
//! the AUR/Linux graph.

pub mod hotkey;
mod icon;
mod payload;
// Scoop detection, manifest parsing and the update script: paths and text,
// no Windows API, so Linux CI tests it; only the host spawns anything.
mod scoop;

#[cfg(windows)]
mod host;
#[cfg(windows)]
mod startup;
#[cfg(windows)]
mod tui_launch;
// Release check, download and verification: `reqwest` and paths, no Windows
// API. It follows this module's rule — compile everywhere so Linux CI runs its
// tests — even though only the Windows host calls it.
mod update_flow;

pub use icon::{Severity, tray_icon_rgba};

/// Set on the process an update relaunches, so it waits for the old one to
/// release the single-instance mutex instead of quitting at once. Shared by
/// the in-place relaunch and the Scoop update script.
pub const RELAUNCH_ENV: &str = "AIUB_TRAY_RELAUNCH";
pub use payload::{POLL_INTERVAL, host_payload, worst_severity, wrap_report};

/// Process entry for `ai-usagebar-tray`.
pub fn run() -> i32 {
    #[cfg(windows)]
    {
        host::run()
    }
    #[cfg(not(windows))]
    {
        eprintln!(
            "ai-usagebar-tray is the Windows system-tray popover; it is not used on this OS."
        );
        1
    }
}
