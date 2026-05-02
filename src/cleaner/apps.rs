// cleaner/apps.rs — app lifecycle management
//
// Fix: Spotlight fallback chain for get_last_used()
//   mdls -name kMDItemLastUsedDate   (primary — Spotlight index)
//   mdls -name kMDItemDateAdded      (fallback 1 — add date if never opened)
//   stat -f %a <path>                (fallback 2 — last-access time from filesystem)
//   None                             → conservative: do NOT classify as stale
//
// This means an app is only treated as "unused" if we have POSITIVE evidence
// it has not been opened within the threshold. Unknown = safe.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};
use log::{debug, info, warn};
use crate::config::Config;
use crate::error::{RemoveResult, SkipReason};
use super::ops::{safe_remove, dir_size_safe};

// ── AppInfo ───────────────────────────────────────────────────────────────────

pub struct AppInfo {
    pub name:           String,
    pub bundle_id:      Option<String>,
    pub path:           PathBuf,
    /// Positive evidence of last use. None = unknown (treated as recent).
    pub last_used:      Option<SystemTime>,
    /// Source of the last_used value (for audit display)
    pub last_used_src:  LastUsedSource,
    pub size:           u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastUsedSource {
    /// kMDItemLastUsedDate from Spotlight — most reliable
    SpotlightLastUsed,
    /// kMDItemDateAdded — app has never been opened
    SpotlightDateAdded,
    /// atime from stat — Spotlight unavailable
    FilesystemAtime,
    /// All sources failed — assume recently used (conservative)
    Unknown,
}

impl LastUsedSource {
    pub fn label(self) -> &'static str {
        match self {
            LastUsedSource::SpotlightLastUsed  => "spotlight",
            LastUsedSource::SpotlightDateAdded => "never opened",
            LastUsedSource::FilesystemAtime    => "atime",
            LastUsedSource::Unknown            => "unknown",
        }
    }
}

impl AppInfo {
    pub fn age_days(&self) -> Option<u64> {
        self.last_used.and_then(|t| {
            SystemTime::now().duration_since(t).ok().map(|d| d.as_secs() / 86400)
        })
    }
    /// An app is only considered "unused" if we have positive evidence of age.
    pub fn is_unused_for(&self, threshold: Duration) -> bool {
        match self.last_used {
            Some(t) => SystemTime::now().duration_since(t)
                .unwrap_or_default() >= threshold,
            None => false, // unknown = safe = not stale
        }
    }
}

// ── scan ──────────────────────────────────────────────────────────────────────

pub fn scan_apps(cfg: &Config) -> Vec<AppInfo> {
    let dirs = [
        PathBuf::from("/Applications"),
        home_join("Applications"),
    ];
    let mut apps = Vec::new();

    for dir in &dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("app") { continue; }

            let name = path.file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            if cfg.whitelist.contains_app(&name) { continue; }

            let bundle_id = get_bundle_id(&path);
            if let Some(ref bid) = bundle_id {
                if cfg.whitelist.contains_bundle(bid) { continue; }
                if crate::guard::is_critical_bundle(bid) { continue; }
            }

            let (last_used, last_used_src) = get_last_used_with_fallback(&path);
            let size = dir_size_safe(&path);

            apps.push(AppInfo { name, bundle_id, path, last_used, last_used_src, size });
        }
    }
    apps
}

// ── public operations ─────────────────────────────────────────────────────────

pub fn remove_rogue(cfg: &Config, dry_run: bool) -> anyhow::Result<()> {
    for app in scan_apps(cfg) {
        let is_rogue_name = cfg.rogue_list.app_names.iter().any(|n| *n == app.name);
        let is_rogue_bid  = app.bundle_id.as_ref()
            .map(|b| cfg.rogue_list.bundle_ids.iter().any(|r| r == b))
            .unwrap_or(false);
        if !is_rogue_name && !is_rogue_bid { continue; }
        warn!("rogue: removing {}", app.name);
        if dry_run {
            println!("  [dry] would remove rogue: {}", app.name);
        } else {
            uninstall_app(&app, cfg);
        }
    }
    Ok(())
}

pub fn clean_unused_cache(cfg: &Config, dry_run: bool) -> anyhow::Result<u64> {
    let threshold = Duration::from_secs(cfg.thresholds.cache_unused_days * 86400);
    let mut freed = 0u64;

    for app in scan_apps(cfg) {
        if !app.is_unused_for(threshold) { continue; }
        debug!("cache sweep: {} ({}d, src={})",
            app.name,
            app.age_days().unwrap_or(0),
            app.last_used_src.label(),
        );
        if let Some(ref bid) = app.bundle_id {
            let cache = home_join(format!("Library/Caches/{bid}"));
            if cache.exists() {
                let r = safe_remove(&cache, dry_run);
                r.log_errors();
                freed += r.freed_bytes;
            }
        }
    }

    info!("app cache sweep: freed {}", crate::fmt_bytes(freed));
    Ok(freed)
}

pub fn uninstall_stale(cfg: &Config, dry_run: bool) -> anyhow::Result<()> {
    let threshold = Duration::from_secs(cfg.thresholds.uninstall_unused_days * 86400);

    for app in scan_apps(cfg) {
        if !app.is_unused_for(threshold) { continue; }
        let days = app.age_days().unwrap_or(0);
        info!("uninstall: {} ({}d unused, src={})", app.name, days, app.last_used_src.label());
        if dry_run {
            println!("  [dry] would uninstall {} ({}d unused)", app.name, days);
        } else {
            uninstall_app(&app, cfg);
        }
    }
    Ok(())
}

pub fn audit_report(cfg: &Config) -> anyhow::Result<()> {
    let now = SystemTime::now();
    let mut apps = scan_apps(cfg);
    apps.sort_by(|a, b| {
        let age = |x: &AppInfo| x.last_used
            .and_then(|t| now.duration_since(t).ok())
            .unwrap_or_default();
        age(b).cmp(&age(a))
    });

    println!("\n  App Usage Audit");
    println!("  {:<36} {:>10} {:>12} {:>12}", "Application", "Last Used", "Source", "Size");
    println!("  {}", "─".repeat(74));

    for app in &apps {
        let age  = app.age_days().map(|d| format!("{d}d ago")).unwrap_or_else(|| "unknown".into());
        let size = crate::fmt_bytes(app.size);
        println!("  {:<36} {:>10} {:>12} {:>12}",
            truncate(&app.name, 36), age, app.last_used_src.label(), size);
    }
    println!();
    Ok(())
}

// ── last-used fallback chain ──────────────────────────────────────────────────

fn get_last_used_with_fallback(app: &Path) -> (Option<SystemTime>, LastUsedSource) {
    // ① Try Spotlight kMDItemLastUsedDate
    if let Some(t) = mdls_date(app, "kMDItemLastUsedDate") {
        return (Some(t), LastUsedSource::SpotlightLastUsed);
    }

    // ② Spotlight is indexed but app was never opened → use kMDItemDateAdded
    //    This is still valid evidence: age = time since install.
    if spotlight_is_available() {
        if let Some(t) = mdls_date(app, "kMDItemDateAdded") {
            return (Some(t), LastUsedSource::SpotlightDateAdded);
        }
    }

    // ③ Spotlight unavailable → fall back to filesystem atime
    //    atime is unreliable (noatime mounts, etc.) but better than unknown.
    if let Some(t) = atime(app) {
        return (Some(t), LastUsedSource::FilesystemAtime);
    }

    // ④ All sources failed → unknown → conservative (do not treat as stale)
    (None, LastUsedSource::Unknown)
}

fn mdls_date(app: &Path, key: &str) -> Option<SystemTime> {
    let out = Command::new("mdls")
        .args(["-name", key, "-raw", app.to_str()?])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    if s == "(null)" || s.is_empty() { return None; }
    parse_mdls_date(s)
}

/// Check if Spotlight is indexing. Called at most once per scan via a
/// thread-local cache — avoids N×mdutil invocations for N apps.
fn spotlight_is_available() -> bool {
    use std::cell::Cell;
    thread_local! {
        static CACHE: Cell<Option<bool>> = Cell::new(None);
    }
    CACHE.with(|c| {
        if let Some(v) = c.get() { return v; }
        let available = Command::new("mdutil").args(["-s", "/"]).output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("enabled"))
            .unwrap_or(false);
        c.set(Some(available));
        available
    })
}

/// Get filesystem atime via `stat -f %a` (BSD stat on macOS).
fn atime(path: &Path) -> Option<SystemTime> {
    let out = Command::new("stat")
        .args(["-f", "%a", path.to_str()?])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let unix_secs: u64 = s.parse().ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(unix_secs))
}

// ── uninstall ─────────────────────────────────────────────────────────────────

fn uninstall_app(app: &AppInfo, cfg: &Config) {
    // Verify codesign before touching — skip if mid-update
    if !crate::guard::codesign_ok(&app.path) {
        warn!("uninstall: {} codesign invalid — skipping (may be mid-update)", app.name);
        return;
    }

    if let Some(ref bid) = app.bundle_id {
        let _ = Command::new("osascript")
            .args(["-e", &format!("quit app id \"{bid}\"")])
            .output();
    }
    kill_by_name(&app.name);

    let _ = std::fs::remove_dir_all(&app.path);

    if let Some(ref bid) = app.bundle_id {
        for base in &cfg.paths.app_containers {
            let p = Config::expand(base).join(bid);
            if p.exists() { safe_remove(&p, false).log_errors(); }
        }
        for base in &cfg.paths.app_prefs {
            let p = Config::expand(base).join(format!("{bid}.plist"));
            if p.exists() { safe_remove(&p, false).log_errors(); }
        }
        for base in &cfg.paths.app_support {
            let p = Config::expand(base).join(&app.name);
            if p.exists() { safe_remove(&p, false).log_errors(); }
        }
        let cache = home_join(format!("Library/Caches/{bid}"));
        if cache.exists() { safe_remove(&cache, false).log_errors(); }

        let state = home_join(format!("Library/Saved Application State/{bid}.savedState"));
        if state.exists() { safe_remove(&state, false).log_errors(); }

        remove_launch_items(bid);
        info!("uninstalled: {}", app.name);
    }
}

fn remove_launch_items(bid: &str) {
    let dirs = [
        home_join("Library/LaunchAgents"),
        PathBuf::from("/Library/LaunchAgents"),
        PathBuf::from("/Library/LaunchDaemons"),
    ];
    for dir in &dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains(bid) {
                let path = entry.path();
                let _ = Command::new("launchctl")
                    .args(["unload", path.to_str().unwrap_or("")]).output();
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

fn kill_by_name(name: &str) {
    let _ = Command::new("pkill").args(["-x", name]).output();
    std::thread::sleep(Duration::from_millis(500));
    let _ = Command::new("pkill").args(["-9", "-x", name]).output();
}

// ── date parsing ──────────────────────────────────────────────────────────────

fn parse_mdls_date(s: &str) -> Option<SystemTime> {
    // mdls date format: "2024-11-03 14:15:00 +0000"
    // Strip optional timezone suffix (e.g. " +0000") before splitting.
    let s = s.trim();
    let s = if let Some(idx) = s.rfind(" +").or_else(|| s.rfind(" -")) {
        s[..idx].trim()
    } else {
        s
    };

    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 2 { return None; }
    let dp: Vec<u64> = parts[0].split('-').filter_map(|x| x.parse().ok()).collect();
    let tp: Vec<u64> = parts[1].split(':').filter_map(|x| x.parse().ok()).collect();
    if dp.len() < 3 || tp.len() < 3 { return None; }
    let days = days_from_ymd(dp[0], dp[1], dp[2]);
    let secs = days * 86400 + tp[0] * 3600 + tp[1] * 60 + tp[2];
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

fn days_from_ymd(y: u64, m: u64, d: u64) -> u64 {
    let (y, m) = if m < 3 { (y - 1, m + 12) } else { (y, m) };
    365 * y + y / 4 - y / 100 + y / 400 + (306 * (m + 1)) / 10 + d - 719_591
}

fn home_join<P: AsRef<Path>>(p: P) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(p)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else { format!("{}…", &s[..max.saturating_sub(1)]) }
}
