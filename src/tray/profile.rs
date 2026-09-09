//! Where the popover's WebView2 profile lives.
//!
//! WebView2 defaults its user-data folder to `<exe dir>\<exe>.WebView2\`.
//! Under Scoop the exe dir is the per-version folder, so every update threw
//! the profile away (the Customize layout, theme and dismissed welcome banner
//! all live in its `localStorage`); under Program Files it is not writable at
//! all. Pin it under the cache root instead, next to `detect.json` and the
//! update staging dir. Pure path logic, compiled on every OS so Linux CI tests
//! it. Only the Windows host calls it, hence the `dead_code` allowance.

#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

/// `<cache_root>/ai-usagebar/popover` — the WebView2 user-data folder.
/// `cache_root` is `crate::cache::xdg_cache_dir()` in production
/// (`%LOCALAPPDATA%` on Windows).
pub fn popover_data_dir(cache_root: &Path) -> PathBuf {
    cache_root.join("ai-usagebar").join("popover")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::popover_data_dir;

    #[test]
    fn popover_profile_lives_under_the_cache_root() {
        let root = Path::new("cache-root");
        assert_eq!(
            popover_data_dir(root),
            root.join("ai-usagebar").join("popover")
        );
    }

    #[test]
    fn popover_profile_never_depends_on_the_exe_dir() {
        let dir = popover_data_dir(Path::new("cache-root"));
        assert!(dir.starts_with("cache-root"));
        assert!(!dir.to_string_lossy().contains("WebView2"));
    }
}
