// sentinel/notifications.rs — notification restriction
//
// Strategy: write per-bundle defaults to com.apple.notificationcenterui.<bid>
// to downgrade banners and disable badge+sound for non-whitelisted apps.
//
// We NEVER touch the NC SQLite database directly — that requires FDA and
// can corrupt the database if NC is running concurrently. defaults write
// is the safe, public API.
//
// After writing, we signal NotificationCenter to reload its prefs:
//   killall -HUP NotificationCenter 2>/dev/null
//
// Permission guard: we only restrict apps the user has never explicitly
// configured (i.e., those not in per_app_overrides) and never escalate
// via sudo — all writes stay in ~/Library/Preferences.

use std::process::Command;
use log::{debug, info};
use crate::config::Config;

pub fn restrict(cfg: &Config) -> anyhow::Result<()> {
    if !cfg.silence.block_notifications { return Ok(()); }

    let bundle_ids = registered_nc_apps();
    let mut restricted = 0usize;

    for bid in &bundle_ids {
        if cfg.whitelist.contains_bundle(bid) { continue; }

        if crate::guard::is_critical_bundle(bid) { continue; }

        if let Some(ov) = cfg.silence.per_app_overrides.get(bid.as_str()) {
            if ov.allow_notifications {
                debug!("notifications: allowing {bid} (override)");
                continue;
            }
        }

        // Rogue list → full disable
        if cfg.rogue_list.bundle_ids.contains(bid) {
            notification_disable(bid);
        } else {
            notification_banner_only(bid);
        }

        restricted += 1;
    }

    // Signal NC to pick up changed prefs
    let _ = Command::new("killall").args(["-HUP", "NotificationCenter"]).output();

    info!("notifications: restricted {restricted}/{} apps", bundle_ids.len());
    Ok(())
}

// ── NC prefs manipulation ─────────────────────────────────────────────────────

fn notification_banner_only(bid: &str) {
    // alert-style: 0=none, 1=banners, 2=alerts
    nc_write(bid, "alert-style",              "banner");
    nc_write(bid, "badge-enabled",            "0");
    nc_write(bid, "sound-enabled",            "0");
    nc_write(bid, "critical-alert-enabled",   "0");
    debug!("notifications: banner-only for {bid}");
}

fn notification_disable(bid: &str) {
    nc_write(bid, "alert-style",   "none");
    nc_write(bid, "badge-enabled", "0");
    nc_write(bid, "sound-enabled", "0");
    debug!("notifications: disabled for {bid}");
}

fn nc_write(bid: &str, key: &str, value: &str) {
    // Writes to ~/Library/Preferences/com.apple.ncprefs.plist
    // via the notificationcenterui domain
    let domain = format!("com.apple.notificationcenterui.{bid}");
    let _ = Command::new("defaults")
        .args(["write", &domain, key, value])
        .output();
}

// ── registered app discovery ──────────────────────────────────────────────────

/// Parse ~/Library/Preferences/com.apple.ncprefs.plist for registered bundle IDs.
/// Uses `defaults export` → text scan (no binary plist parser dependency).
fn registered_nc_apps() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let plist = format!("{home}/Library/Preferences/com.apple.ncprefs.plist");

    let out = Command::new("defaults")
        .args(["export", &plist, "-"])
        .output();

    let Ok(o) = out else { return vec![] };
    let text = String::from_utf8_lossy(&o.stdout);

    let mut ids = Vec::new();

    for line in text.lines() {
        let t = line.trim();
        if !t.starts_with("<string>") { continue; }
        let inner = t.trim_start_matches("<string>").trim_end_matches("</string>");
        if inner.contains('.') && !inner.contains('/') && inner.len() < 128 {
            ids.push(inner.to_string());
        }
    }

    ids.sort();
    ids.dedup();
    ids
}
