// optimizer/system.rs — deep system optimizations
//
// Passes (all gated by config flags):
//
//   launch_services_rebuild  — lsregister -kill -r -domain local/system/user
//   sqlite_vacuum            — VACUUM all .db files in ~/Library (safe read-write check first)
//   quarantine_cleanup       — DELETE FROM LSQuarantineEvent WHERE age > threshold
//   saved_state_cleanup      — remove stale .savedState bundles
//   broken_launch_agents     — unload + remove plists whose binary is gone
//   notification_center_cleanup — prune NC prefs for uninstalled apps
//   coreduet_cleanup         — remove CoreDuet activity DB (rebuilds on next login)
//   periodic_maintenance     — run macOS daily/weekly/monthly maintenance scripts

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};
use log::{debug, info, warn};
use crate::config::Config;
use crate::cleaner::ops::{safe_remove, safe_remove_old_files};

pub fn apply(cfg: &Config) -> anyhow::Result<()> {
    let opt = &cfg.optimizer;
    if !opt.enabled { return Ok(()); }

    let mut passes = 0usize;

    if opt.launch_services_rebuild {
        rebuild_launch_services();
        passes += 1;
    }

    if opt.sqlite_vacuum {
        vacuum_sqlite_dbs();
        passes += 1;
    }

    if opt.quarantine_cleanup {
        cleanup_quarantine_db(cfg);
        passes += 1;
    }

    if opt.saved_state_cleanup {
        cleanup_saved_states(cfg);
        passes += 1;
    }

    if opt.broken_launch_agents {
        remove_broken_launch_agents();
        passes += 1;
    }

    if opt.notification_center_cleanup {
        cleanup_nc_prefs();
        passes += 1;
    }

    if opt.coreduet_cleanup {
        cleanup_coreduet();
        passes += 1;
    }

    if opt.disable_sudden_motion_sensor {
        // SSD machines: SMS is a no-op hardware feature; disable it
        run("pmset", &["-a", "sms", "0"]);
        passes += 1;
    }

    if opt.increase_fd_limit {
        // Raise per-process fd limit for the current session
        run("launchctl", &["limit", "maxfiles", "524288", "524288"]);
        passes += 1;
    }

    if opt.periodic_maintenance {
        run_periodic_scripts();
        passes += 1;
    }

    if opt.purge_inactive_memory &&
        // SAFETY: geteuid() has no preconditions; always safe to call.
        unsafe { libc::geteuid() } == 0 {
        run("purge", &[]);
        passes += 1;
    }

    if opt.disable_reopen_windows {
        run("defaults", &[
            "write", "com.apple.loginwindow",
            "TALLogoutSavesState", "-bool", "false",
        ]);
        run("defaults", &[
            "write", "NSGlobalDomain",
            "NSQuitAlwaysKeepsWindows", "-bool", "false",
        ]);
        passes += 1;
    }

    info!("system optimizer: {passes} pass(es) applied");
    Ok(())
}

// ── LaunchServices ────────────────────────────────────────────────────────────

/// Rebuild the LaunchServices database.
/// Fixes "Open With" duplicates, broken default app associations,
/// and stale UTI registrations left by uninstalled apps.
fn rebuild_launch_services() {
    debug!("optimizer: rebuilding LaunchServices database");

    // lsregister binary location varies by macOS version
    let lsreg = find_lsregister();
    if let Some(bin) = lsreg {
        run(&bin, &["-kill", "-r",
            "-domain", "local",
            "-domain", "system",
            "-domain", "user",
        ]);
        info!("optimizer: LaunchServices rebuilt");
    } else {
        warn!("optimizer: lsregister not found — skipping LS rebuild");
    }
}

fn find_lsregister() -> Option<String> {
    let candidates = [
        "/System/Library/Frameworks/CoreServices.framework/\
         Versions/A/Frameworks/LaunchServices.framework/\
         Versions/A/Support/lsregister",
        "/System/Library/Frameworks/CoreServices.framework/\
         Frameworks/LaunchServices.framework/Support/lsregister",
    ];
    for c in &candidates {
        if Path::new(c).exists() { return Some(c.to_string()); }
    }
    None
}

// ── SQLite VACUUM ─────────────────────────────────────────────────────────────

/// VACUUM all .db files in ~/Library that are not currently open (checked via
/// lsof prefix scan) and are smaller than 500 MB (to bound runtime).
fn vacuum_sqlite_dbs() {
    debug!("optimizer: vacuuming SQLite databases");

    let roots = [
        home_join("Library/Application Support"),
        home_join("Library/Messages"),
    ];

    let mut vacuumed = 0usize;

    for root in &roots {
        if crate::guard::is_hard_blocked(root) { continue; }
        let Ok(rd) = std::fs::read_dir(root) else { continue };

        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(sub) = std::fs::read_dir(&path) {
                    for se in sub.flatten() {
                        vacuumed += vacuum_if_eligible(&se.path());
                    }
                }
            } else {
                vacuumed += vacuum_if_eligible(&path);
            }
        }
    }

    // Also vacuum the Spotlight, NC, and LS databases
    let system_dbs = [
        home_join("Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.db"),
    ];
    for db in &system_dbs {
        vacuumed += vacuum_if_eligible(db);
    }

    info!("optimizer: vacuumed {vacuumed} SQLite database(s)");
}

fn vacuum_if_eligible(path: &Path) -> usize {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "db" && ext != "sqlite" && ext != "sqlite3" { return 0; }
    if crate::guard::is_hard_blocked(path) { return 0; }

    // Size guard: skip > 500 MB (VACUUM on huge DBs can take minutes)
    let size = path.metadata().map(|m| m.len()).unwrap_or(0);
    if size > 500 * 1_048_576 {
        debug!("optimizer: skipping large db {}", path.display());
        return 0;
    }
    if size == 0 { return 0; }

    // Use BEGIN EXCLUSIVE to atomically probe for a write lock rather than
    // relying on lsof, which only checks whether any process has the *directory*
    // open — not whether sqlite3 itself holds a write lock on this specific file.
    // A failed EXCLUSIVE transaction means the DB is actively locked; we skip it.
    let lock_probe = Command::new("sqlite3")
        .args([path.to_str().unwrap_or(""), "BEGIN EXCLUSIVE; ROLLBACK;"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !lock_probe {
        debug!("optimizer: db write-locked, skipping {}", path.display());
        return 0;
    }

    let ok = Command::new("sqlite3")
        .args([path.to_str().unwrap_or(""), "VACUUM;"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if ok {
        debug!("optimizer: vacuumed {}", path.display());
        1
    } else {
        0
    }
}

// ── Quarantine DB ─────────────────────────────────────────────────────────────

/// Prune stale Gatekeeper quarantine events.
/// The DB grows unbounded; pruning old events has no functional effect.
fn cleanup_quarantine_db(cfg: &Config) {
    let db = home_join("Library/Preferences/com.apple.LaunchServices/\
        com.apple.launchservices.secure.db");
    if !db.exists() { return; }

    let cutoff_days = cfg.thresholds.log_max_age_days;
    let cutoff_ts   = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(cutoff_days * 86400);

    // CoreFoundation timestamps start at 2001-01-01 (978307200s Unix offset)
    let cf_cutoff = cutoff_ts.saturating_sub(978_307_200);

    let sql = format!(
        "DELETE FROM LSQuarantineEvent \
         WHERE LSQuarantineTimeStamp < {cf_cutoff};"
    );

    let ok = Command::new("sqlite3")
        .args([db.to_str().unwrap_or(""), &sql])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if ok {
        info!("optimizer: quarantine DB pruned (events older than {cutoff_days}d)");
    }
}

// ── Saved Application State ───────────────────────────────────────────────────

/// Remove .savedState bundles older than saved_state_max_age_days
/// or whose parent app no longer exists on disk.
fn cleanup_saved_states(cfg: &Config) {
    let states_dir = home_join("Library/Saved Application State");
    if !states_dir.exists() { return; }

    let max_age = Duration::from_secs(cfg.optimizer.saved_state_max_age_days * 86400);
    let cutoff = SystemTime::now().checked_sub(max_age)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let Ok(rd) = std::fs::read_dir(&states_dir) else { return };
    let mut removed = 0usize;

    for entry in rd.flatten() {
        let path = entry.path();
        let name = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if !name.ends_with(".savedState") { continue; }

        let bid = name.trim_end_matches(".savedState");

        if cfg.whitelist.contains_bundle(bid) { continue; }
        if crate::guard::is_critical_bundle(bid) { continue; }

        let app_gone = !app_is_installed(bid);
        let stale    = path.metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);

        if app_gone || stale {
            if std::fs::remove_dir_all(&path).is_ok() {
                debug!("optimizer: removed saved state {name}");
                removed += 1;
            }
        }
    }

    info!("optimizer: removed {removed} stale saved state bundle(s)");
}

fn app_is_installed(bid: &str) -> bool {
    let out = Command::new("mdfind")
        .args([&format!("kMDItemCFBundleIdentifier == '{bid}'")])
        .output();
    match out {
        Ok(o) => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
        Err(_) => true, // assume installed on error (conservative)
    }
}

// ── Broken LaunchAgents ───────────────────────────────────────────────────────

/// Remove LaunchAgent plists whose binary path no longer exists.
/// Unloads from launchd first.
fn remove_broken_launch_agents() {
    let la_dir = home_join("Library/LaunchAgents");
    let Ok(rd) = std::fs::read_dir(&la_dir) else { return };

    let mut removed = 0usize;

    for entry in rd.flatten() {
        let plist = entry.path();
        if plist.extension().and_then(|e| e.to_str()) != Some("plist") { continue; }

        let name = plist.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if name.starts_with("com.apple.") { continue; }

        let binary = plist_get_binary(&plist);
        let Some(bin) = binary else { continue };

        if !Path::new(&bin).exists() {
            debug!("optimizer: broken agent {name} (binary gone: {bin})");

            let _ = Command::new("launchctl")
                .args(["unload", plist.to_str().unwrap_or("")])
                .output();

            if std::fs::remove_file(&plist).is_ok() {
                info!("optimizer: removed broken agent {name}");
                removed += 1;
            }
        }
    }

    if removed > 0 {
        info!("optimizer: removed {removed} broken launch agent(s)");
    }
}

fn plist_get_binary(plist: &Path) -> Option<String> {
    let out = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :ProgramArguments:0", plist.to_str()?])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        let out2 = Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :Program", plist.to_str()?])
            .output()
            .ok()?;
        let s2 = String::from_utf8_lossy(&out2.stdout).trim().to_string();
        if s2.is_empty() { None } else { Some(s2) }
    } else {
        Some(s)
    }
}

// ── Notification Center prefs ─────────────────────────────────────────────────

/// Remove NC pref entries for apps that are no longer installed.
fn cleanup_nc_prefs() {
    // We only read the registered apps list; actual prune is done by
    // deleting the per-app domain written by notifications.rs.
    // This pass removes domains for bundle IDs where no .app exists.
    let home = std::env::var("HOME").unwrap_or_default();
    let plist = format!("{home}/Library/Preferences/com.apple.ncprefs.plist");

    let out = Command::new("defaults")
        .args(["export", &plist, "-"])
        .output();
    let Ok(o) = out else { return };
    let text = String::from_utf8_lossy(&o.stdout);

    let mut pruned = 0usize;

    for line in text.lines() {
        let t = line.trim();
        if !t.starts_with("<string>") { continue; }
        let inner = t.trim_start_matches("<string>").trim_end_matches("</string>");
        if !inner.contains('.') || inner.contains('/') { continue; }

        if !app_is_installed(inner) {
            let domain = format!("com.apple.notificationcenterui.{inner}");
            let _ = Command::new("defaults")
                .args(["delete", &domain])
                .output();
            debug!("optimizer: pruned NC prefs for {inner}");
            pruned += 1;
        }
    }

    if pruned > 0 {
        let _ = Command::new("killall").args(["-HUP", "NotificationCenter"]).output();
        info!("optimizer: pruned {pruned} stale NC pref domain(s)");
    }
}

// ── CoreDuet ─────────────────────────────────────────────────────────────────

/// CoreDuet collects app usage data for Siri Suggestions.
/// Its DB grows over time; deleting it is safe — it rebuilds on next login.
/// Requires killing coreduetd first (non-root, user-space daemon).
fn cleanup_coreduet() {
    let db_dir = home_join("Library/Application Support/Knowledge");
    if !db_dir.exists() { return; }

    run("launchctl", &["kill", "SIGTERM", "user/501/com.apple.coreduetd"]);
    std::thread::sleep(std::time::Duration::from_millis(500));

    let dbs = [
        db_dir.join("knowledgeC.db"),
        db_dir.join("knowledgeC.db-shm"),
        db_dir.join("knowledgeC.db-wal"),
    ];

    for db in &dbs {
        if db.exists() {
            let _ = std::fs::remove_file(db);
        }
    }

    run("launchctl", &["kickstart", "-k", "user/501/com.apple.coreduetd"]);

    info!("optimizer: CoreDuet knowledge DB cleared");
}

// ── BSD periodic scripts ──────────────────────────────────────────────────────

/// Run macOS periodic maintenance scripts if they haven't run recently.
/// daily: rotate logs, rebuild locate DB
/// weekly: rebuild whatis DB
/// monthly: accounting summaries
fn run_periodic_scripts() {
    for period in &["daily", "weekly", "monthly"] {
        let _ = Command::new("periodic").arg(period).output();
        debug!("optimizer: periodic {period} ran");
    }
    info!("optimizer: BSD periodic maintenance complete");
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn run(cmd: &str, args: &[&str]) {
    debug!("$ {cmd} {}", args.join(" "));
    let _ = Command::new(cmd).args(args).output();
}

fn home_join<P: AsRef<Path>>(p: P) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(p)
}
