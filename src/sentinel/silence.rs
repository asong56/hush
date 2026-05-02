// sentinel/silence.rs — background agent suppression + force-quit-on-close
//
// block_agents():
//   Iterates ~/Library/LaunchAgents, skips Apple + whitelisted,
//   disables non-essential agents via `launchctl disable`.
//   Does NOT kill running processes (use crush for that).
//
// force_quit_on_close():
//   Writes NSQuitAlwaysKeepsWindows = false globally and per-app.
//   Also disables Dock reopen-at-login and hides recent apps.
//   Never touches Apple system processes.

use std::process::Command;
use log::{debug, info};
use crate::config::Config;

// ── block background agents ───────────────────────────────────────────────────

pub fn block_agents(cfg: &Config) -> anyhow::Result<()> {
    if !cfg.silence.block_background_agents { return Ok(()); }

    let la_dir = home_join("Library/LaunchAgents");
    let Ok(rd) = std::fs::read_dir(&la_dir) else { return Ok(()) };

    let uid = unsafe { libc::getuid() };
    let mut blocked = 0usize;

    for entry in rd.flatten() {
        let plist = entry.path();
        if plist.extension().and_then(|e| e.to_str()) != Some("plist") { continue; }

        let name = plist.file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        if name.starts_with("com.apple.") { continue; }

        if cfg.whitelist.bundle_ids.iter().any(|b| name.contains(b.as_str())) {
            continue;
        }

        // per-app override: allow_background = true
        let allowed = cfg.silence.per_app_overrides.iter().any(|(bid, ov)| {
            name.contains(bid.as_str()) && ov.allow_background
        });
        if allowed { continue; }

        // Disable the agent (prevents it loading on next login)
        let service = format!("gui/{uid}/{name}");
        let _ = Command::new("launchctl")
            .args(["disable", &service])
            .output();

        debug!("silence: disabled agent {name}");
        blocked += 1;
    }

    info!("silence: blocked {blocked} launch agents");
    Ok(())
}

// ── force quit on window close ────────────────────────────────────────────────

pub fn force_quit_on_close(cfg: &Config) -> anyhow::Result<()> {
    if !cfg.silence.force_quit_on_window_close { return Ok(()); }

    defaults_write("NSGlobalDomain", "NSQuitAlwaysKeepsWindows", "-bool", "false");

    // Disable "reopen windows on login"
    if cfg.optimizer.disable_reopen_windows {
        defaults_write(
            "com.apple.loginwindow",
            "TALLogoutSavesState", "-bool", "false",
        );
    }

    let apps_dir = std::path::Path::new("/Applications");
    if let Ok(rd) = std::fs::read_dir(apps_dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("app") { continue; }

            let bid = get_bundle_id_quick(&path);
            if let Some(ref b) = bid {
                if cfg.whitelist.contains_bundle(b) { continue; }
                if b.starts_with("com.apple.") { continue; }
                defaults_write(b, "NSQuitAlwaysKeepsWindows", "-bool", "false");
            }
        }
    }

    // Dock: don't show running apps when they have no windows
    defaults_write("com.apple.dock", "static-only", "-bool", "false");
    defaults_write("com.apple.dock", "show-recents", "-bool", "false");

    let _ = Command::new("killall").arg("Dock").output();

    info!("silence: force-quit-on-close applied");
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn defaults_write(domain: &str, key: &str, type_flag: &str, value: &str) {
    let _ = Command::new("defaults")
        .args(["write", domain, key, type_flag, value])
        .output();
}

fn get_bundle_id_quick(app: &std::path::Path) -> Option<String> {
    let plist = app.join("Contents/Info.plist");
    let out = Command::new("defaults")
        .args(["read", plist.to_str()?, "CFBundleIdentifier"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn home_join<P: AsRef<std::path::Path>>(p: P) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(p)
}
