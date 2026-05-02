// guard.rs — safety layer
//
// Four risk classes addressed:
//   1. Codesign damage     — never touch files inside .app/Contents/
//   2. File lock / EBUSY   — lsof check for large dirs, pgrep for processes
//   3. Silent TCC escalation — block FDA-requiring paths, no DB writes
//   4. Critical cache mis-kill — hard-blocked bundle segment list

use std::path::Path;
use std::process::Command;
use log::debug;

// ── hard-blocked segments ────────────────────────────────────────────────────

static BLOCKED_SEGMENTS: &[&str] = &[
    // ① Codesign: never individually delete files inside a live .app bundle
    ".app/Contents/",

    // ② System identity
    "/System/",
    "/usr/bin/", "/usr/lib/", "/usr/sbin/",
    "/bin/", "/sbin/",
    "/etc/", "/private/etc/",
    "/Library/Extensions/",
    "/Library/StagedExtensions/",

    // ③ Critical caches — removal causes visible system breakage
    "com.apple.systempreferences",
    "com.apple.SystemSettings",
    "com.apple.controlcenter",
    "com.apple.coreaudio",
    "com.apple.audio.",
    "coreaudiod",
    "com.apple.notificationcenterui",
    "com.apple.finder",
    "com.apple.dock.saved-state",
    "com.apple.security.",
    "com.apple.trustd",
    "com.apple.keychain",
    "com.apple.bird",
    "com.apple.CloudDocs",
    "org.cups.",

    // ④ User data
    "/Mobile Documents",
    "/.Trash",
];

static BLOCKED_ABSOLUTE: &[&str] = &[
    "/", "/bin", "/sbin", "/usr", "/System",
    "/Library", "/private", "/etc", "/var",
];

pub fn is_hard_blocked(path: &Path) -> bool {
    let s = path.to_string_lossy();

    if BLOCKED_ABSOLUTE.contains(&s.as_ref()) { return true; }

    for seg in BLOCKED_SEGMENTS {
        if s.contains(seg) {
            debug!("guard: hard-blocked {s}");
            return true;
        }
    }

    // /Library/* — allow-list only
    if s.starts_with("/Library/") {
        let rest = &s["/Library/".len()..];
        let allowed = rest.starts_with("Caches/")
            || rest.starts_with("Logs/")
            || rest.starts_with("LaunchAgents/")
            || rest.starts_with("LaunchDaemons/")
            || rest.starts_with("Application Support/")
            || rest.starts_with("Preferences/")
            || rest.starts_with("Updates/")
            || rest.starts_with("PrivilegedHelperTools/");
        if !allowed {
            debug!("guard: /Library/{rest} not in allow-list");
            return true;
        }
    }

    false
}

pub fn is_inside_app_bundle(path: &Path) -> bool {
    path.to_string_lossy().contains(".app/Contents/")
}

// ── process occupancy ─────────────────────────────────────────────────────────

pub fn is_process_using(path: &Path) -> bool {
    // Quick stem heuristic
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        if stem.len() >= 3 {
            let running = Command::new("pgrep")
                .args(["-f", stem])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if running {
                debug!("guard: process uses stem '{stem}'");
                return true;
            }
        }
    }
    // lsof for directories
    if path.is_dir() {
        let path_str = path.to_string_lossy();
        if let Ok(o) = Command::new("lsof").args(["+D", path_str.as_ref()]).output() {
            let in_use = o.status.success()
                && !String::from_utf8_lossy(&o.stdout).trim().is_empty();
            if in_use {
                debug!("guard: lsof in use: {path_str}");
                return true;
            }
        }
    }
    false
}

// ── codesign ─────────────────────────────────────────────────────────────────

pub fn codesign_ok(app: &Path) -> bool {
    Command::new("codesign")
        .args(["--verify", "--deep", "--strict", app.to_str().unwrap_or("")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── TCC ──────────────────────────────────────────────────────────────────────

pub fn requires_fda(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/Library/Mail/")
        || s.contains("/Library/Messages/")
        || s.contains("/Library/Safari/")
        || s.contains("/private/var/db/")
        || s.contains("/private/var/folders/")
        || s.contains("com.apple.security")
        || s.contains("com.apple.trustd")
}

// ── critical bundle IDs ───────────────────────────────────────────────────────

pub fn is_critical_bundle(bid: &str) -> bool {
    if bid.starts_with("com.apple.") {
        let safe = bid.starts_with("com.apple.dt.")
            || bid.starts_with("com.apple.CoreSimulator")
            || bid.starts_with("com.apple.iphonesimulator")
            || bid == "org.swift.swiftpm";
        return !safe;
    }
    matches!(bid,
        "org.cups" | "com.1password.1password"
        | "com.agilebits.onepassword7" | "com.bitwarden.desktop"
    )
}
