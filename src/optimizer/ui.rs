// optimizer/ui.rs — UI & rendering layer optimizations
//
// Passes:
//   quicklook_refresh       — kill qlmanage + clear thumbnail cache
//   font_cache_rebuild      — remove ATSFontCache (rebuilds on launch)
//   prevent_network_dsstore — disable DS_Store writes on SMB/AFP/WebDAV
//   dock_refresh            — performance defaults + killall Dock

use std::process::Command;
use log::{debug, info};
use crate::config::Config;
use crate::cleaner::ops::safe_remove;

pub fn apply(cfg: &Config) -> anyhow::Result<()> {
    let opt = &cfg.optimizer;
    if !opt.enabled { return Ok(()); }

    let mut passes = 0usize;

    if opt.quicklook_refresh {
        refresh_quicklook();
        passes += 1;
    }

    if opt.font_cache_rebuild {
        rebuild_font_cache();
        passes += 1;
    }

    if opt.prevent_network_dsstore {
        prevent_network_dsstore();
        passes += 1;
    }

    if opt.dock_refresh {
        tune_dock();
        passes += 1;
    }

    // Finder tweaks — always applied as part of UI pass
    tune_finder();
    passes += 1;

    info!("ui optimizer: {passes} pass(es) applied");
    Ok(())
}

// ── QuickLook ─────────────────────────────────────────────────────────────────

/// Kill qlmanage and remove the thumbnail disk cache.
/// Stale QuickLook caches can bloat ~/Library/Caches/com.apple.QuickLook.ThumbnailsAgent
/// and cause blank previews for updated files.
fn refresh_quicklook() {
    run("killall", &["-9", "qlmanage"]);
    run("killall", &["-9", "QuickLookUIService"]);

    let ql_cache = home_join("Library/Caches/com.apple.QuickLook.ThumbnailsAgent");
    safe_remove(&ql_cache, false);

    // Also clear the system-level QL cache (requires root — silent skip if not)
    if unsafe { libc::geteuid() } == 0 {
        run("qlmanage", &["-r", "cache"]);
    } else {
        run("qlmanage", &["-r"]);
    }

    info!("ui: QuickLook cache cleared");
}

// ── Font cache ────────────────────────────────────────────────────────────────

/// Remove the ATS font cache. Fixes corrupt/slow font rendering.
/// macOS rebuilds it automatically on next login.
/// Requires: atsutil databases -remove (may need sudo for /Library path)
fn rebuild_font_cache() {
    let user_ats = home_join("Library/Caches/com.apple.ATS");
    safe_remove(&user_ats, false);

    let _ = Command::new("atsutil")
        .args(["databases", "-remove"])
        .output();

    run("killall", &["ATSServer"]);

    info!("ui: font cache cleared");
}

// ── Network DS_Store prevention ───────────────────────────────────────────────

/// Tell Finder not to write .DS_Store files on network volumes or USB drives.
/// This prevents polluting shared SMB/AFP servers and external drives.
fn prevent_network_dsstore() {
    run("defaults", &[
        "write", "com.apple.desktopservices",
        "DSDontWriteNetworkStores", "-bool", "true",
    ]);
    run("defaults", &[
        "write", "com.apple.desktopservices",
        "DSDontWriteUSBStores", "-bool", "true",
    ]);

    info!("ui: DS_Store writes suppressed on network/USB volumes");
}

// ── Dock tuning ───────────────────────────────────────────────────────────────

/// Apply Dock performance & cleanliness defaults.
fn tune_dock() {
    // Instant autohide (no delay, fast animation)
    run("defaults", &["write", "com.apple.dock", "autohide-delay",         "-float", "0"]);
    run("defaults", &["write", "com.apple.dock", "autohide-time-modifier", "-float", "0.2"]);

    // Remove the auto-hide animation entirely if user has autohide on
    run("defaults", &["write", "com.apple.dock", "enable-spring-load-actions-on-all-items", "-bool", "true"]);

    // Don't show recent apps in Dock
    run("defaults", &["write", "com.apple.dock", "show-recents", "-bool", "false"]);

    // Speed up Mission Control animations
    run("defaults", &["write", "com.apple.dock", "expose-animation-duration", "-float", "0.1"]);

    // Minimize using scale effect (faster than genie)
    run("defaults", &["write", "com.apple.dock", "mineffect", "-string", "scale"]);

    run("killall", &["Dock"]);

    info!("ui: Dock tuned");
}

// ── Finder tuning ─────────────────────────────────────────────────────────────

fn tune_finder() {
    run("defaults", &["write", "NSGlobalDomain", "AppleShowAllExtensions", "-bool", "true"]);
    run("defaults", &["write", "com.apple.finder", "ShowPathbar", "-bool", "true"]);
    run("defaults", &["write", "com.apple.finder", "ShowStatusBar", "-bool", "true"]);

    // "Nlsv" = list view
    run("defaults", &["write", "com.apple.finder", "FXPreferredViewStyle", "-string", "Nlsv"]);

    // "SCcf" = search current folder
    run("defaults", &["write", "com.apple.finder", "FXDefaultSearchScope", "-string", "SCcf"]);

    // Disable the extension-change warning (not a safety feature)
    run("defaults", &["write", "com.apple.finder", "FXEnableExtensionChangeWarning", "-bool", "false"]);

    run("defaults", &["write", "com.apple.desktopservices", "DSDontWriteNetworkStores", "-bool", "true"]);
    run("defaults", &["write", "com.apple.desktopservices", "DSDontWriteUSBStores",     "-bool", "true"]);

    // Expand save panel by default
    run("defaults", &["write", "NSGlobalDomain", "NSNavPanelExpandedStateForSaveMode",  "-bool", "true"]);
    run("defaults", &["write", "NSGlobalDomain", "NSNavPanelExpandedStateForSaveMode2", "-bool", "true"]);

    run("killall", &["Finder"]);
    debug!("ui: Finder tuned");
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn run(cmd: &str, args: &[&str]) {
    debug!("$ {cmd} {}", args.join(" "));
    let _ = Command::new(cmd).args(args).output();
}

fn home_join<P: AsRef<std::path::Path>>(p: P) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(p)
}
