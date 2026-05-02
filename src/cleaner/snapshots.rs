// cleaner/snapshots.rs — APFS local snapshot management
//
// Strategy:
//   1. Parse `tmutil listlocalsnapshots /` (name → date)
//   2. Sort by date (oldest first)
//   3. Keep the N most-recent snapshots (cfg.snapshots.keep_count)
//   4. Delete everything older than max_age_days (using `tmutil deletelocalsnapshots`)
//   5. Also scan backup volumes for stale `.inProgress` bundles
//
// Deletion decision matrix:
//   • skip if Time Machine backup is currently running
//   • skip if snapshot is newer than max_age_days AND within keep_count
//   • delete otherwise
//
// Requires: tmutil (ships with macOS), no sudo needed for local snapshots.

use std::{
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use log::{debug, info, warn};
use crate::config::Config;
use super::ops::dir_size_safe;

#[derive(Debug)]
pub struct Snapshot {
    /// e.g. "com.apple.TimeMachine.2024-11-03-141500"
    pub name: String,
    /// Parsed Unix timestamp
    pub ts:   u64,
}

impl Snapshot {
    pub fn age_days(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.ts) / 86400
    }

    pub fn date_str(&self) -> &str {
        // "com.apple.TimeMachine.2024-11-03-141500" → "2024-11-03"
        self.name
            .rfind('.')
            .map(|i| &self.name[i+1..])
            .and_then(|s| s.get(..10))
            .unwrap_or(&self.name)
    }
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn list() -> anyhow::Result<()> {
    let snaps = fetch_snapshots();

    if snaps.is_empty() {
        println!("  snapshots  no local APFS snapshots found");
        return Ok(());
    }

    println!("\n  Local APFS Snapshots");
    println!("  {:<48} {:>8}  {:>10}", "Name", "Age", "Date");
    println!("  {}", "─".repeat(72));

    for s in &snaps {
        println!(
            "  {:<48} {:>6}d  {:>10}",
            truncate(&s.name, 48),
            s.age_days(),
            s.date_str(),
        );
    }

    println!();
    Ok(())
}

/// Delete stale snapshots according to config policy.
/// Returns approximate bytes freed (estimated from snapshot count × avg 500 MB).
pub fn delete_stale(cfg: &Config, dry_run: bool) -> anyhow::Result<u64> {
    let scfg = &cfg.snapshots;

    if !scfg.enabled {
        return Ok(0);
    }

    if Command::new("which").arg("tmutil").output()
        .map(|o| !o.status.success()).unwrap_or(true)
    {
        debug!("snapshots: tmutil not available");
        return Ok(0);
    }

    // Skip if TM is actively backing up
    if scfg.skip_if_tm_running && tm_is_running() {
        info!("snapshots: Time Machine is running — skipping");
        return Ok(0);
    }

    let mut snaps = fetch_snapshots();
    snaps.sort_by_key(|s| s.ts);

    let total = snaps.len();
    let keep_count = scfg.keep_count as usize;
    let max_age   = scfg.max_age_days;

    // Determine which to delete:
    // Always keep the newest `keep_count`.
    // Of the remainder, delete those older than max_age_days.
    let mut to_delete: Vec<&Snapshot> = Vec::new();

    if total > keep_count {
        let candidates = &snaps[..total - keep_count]; // oldest batch
        for s in candidates {
            if s.age_days() >= max_age {
                to_delete.push(s);
            }
        }
    }

    // Also delete any within keep_count that are very old (> 2× threshold)
    for s in snaps.iter().skip(total.saturating_sub(keep_count)) {
        if s.age_days() >= max_age * 2 {
            to_delete.push(s);
        }
    }

    if to_delete.is_empty() {
        info!("snapshots: nothing to delete ({total} snapshots, all within policy)");
        return Ok(0);
    }

    let mut freed_estimate = 0u64;

    for snap in &to_delete {
        if dry_run {
            println!(
                "  [dry] would delete snapshot {} ({}d old)",
                snap.date_str(), snap.age_days()
            );
            freed_estimate += 500 * 1_048_576; // ~500 MB estimate
            continue;
        }

        let ok = delete_snapshot(&snap.name);
        if ok {
            info!("snapshots: deleted {}", snap.name);
            freed_estimate += 500 * 1_048_576;
        } else {
            warn!("snapshots: failed to delete {}", snap.name);
        }
    }

    // Incomplete Time Machine backups
    if scfg.delete_incomplete_backups {
        freed_estimate += delete_incomplete_backups(
            scfg.incomplete_safe_hours, dry_run
        );
    }

    info!(
        "snapshots: deleted {} snapshot(s), freed ~{}",
        to_delete.len(),
        crate::fmt_bytes(freed_estimate)
    );

    Ok(freed_estimate)
}

// ── tmutil wrappers ───────────────────────────────────────────────────────────

fn fetch_snapshots() -> Vec<Snapshot> {
    let out = Command::new("tmutil")
        .args(["listlocalsnapshots", "/"])
        .output();

    let Ok(o) = out else { return vec![] };
    let text = String::from_utf8_lossy(&o.stdout);

    text.lines()
        .filter(|l| l.contains("com.apple.TimeMachine."))
        .filter_map(|line| {
            let name = line.trim().to_string();
            let ts = parse_snapshot_ts(&name)?;
            Some(Snapshot { name, ts })
        })
        .collect()
}

/// Parse "com.apple.TimeMachine.YYYY-MM-DD-HHMMSS" → Unix timestamp (approx).
fn parse_snapshot_ts(name: &str) -> Option<u64> {
    let date_part = name.rfind('.').map(|i| &name[i+1..])?;
    // Expected format: "2024-11-03-141500"
    if date_part.len() < 15 { return None; }

    let year:  u64 = date_part[0..4].parse().ok()?;
    let month: u64 = date_part[5..7].parse().ok()?;
    let day:   u64 = date_part[8..10].parse().ok()?;
    let hour:  u64 = date_part[11..13].parse().ok()?;
    let min:   u64 = date_part[13..15].parse().ok()?;
    let sec:   u64 = if date_part.len() >= 17 { date_part[15..17].parse().ok()? } else { 0 };

    // Approximate: ignores leap seconds, close enough for day-level decisions
    let days_since_epoch = days_from_ymd(year, month, day);
    Some(days_since_epoch * 86400 + hour * 3600 + min * 60 + sec)
}

fn days_from_ymd(y: u64, m: u64, d: u64) -> u64 {
    let (y, m) = if m < 3 { (y - 1, m + 12) } else { (y, m) };
    365 * y + y / 4 - y / 100 + y / 400 + (306 * (m + 1)) / 10 + d
        - 719_591 // offset to 1970-01-01
}

fn delete_snapshot(name: &str) -> bool {
    let date = name.rfind('.').map(|i| &name[i+1..]).unwrap_or(name);

    Command::new("tmutil")
        .args(["deletelocalsnapshots", date])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn tm_is_running() -> bool {
    let out = Command::new("tmutil").arg("status").output();
    let Ok(o) = out else { return false };
    let text = String::from_utf8_lossy(&o.stdout);
    // Look for "Running = 1"
    text.lines().any(|l| {
        let l = l.trim();
        (l.contains("\"Running\"") || l.starts_with("Running"))
            && l.contains('=')
            && l.contains('1')
    })
}

/// Scan all mounted backup volumes for stale `.inProgress` bundles.
fn delete_incomplete_backups(safe_hours: u64, dry_run: bool) -> u64 {
    let mut freed = 0u64;
    let vols = std::path::Path::new("/Volumes");
    let Ok(rd) = std::fs::read_dir(vols) else { return 0 };

    let safe_secs = safe_hours * 3600;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    for vol_entry in rd.flatten() {
        let vol = vol_entry.path();
        if !vol.is_dir() || vol.is_symlink() { continue; }

        let backupdb = vol.join("Backups.backupdb");
        if !backupdb.exists() { continue; }

        let Ok(mrd) = std::fs::read_dir(&backupdb) else { continue };
        for machine in mrd.flatten() {
            let mpath = machine.path();
            let Ok(brd) = std::fs::read_dir(&mpath) else { continue };
            for backup in brd.flatten() {
                let bpath = backup.path();
                let name = bpath.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                if !name.ends_with(".inProgress") && !name.ends_with(".inprogress") {
                    continue;
                }

                let mtime = bpath.metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(now);

                if now.saturating_sub(mtime) < safe_secs {
                    debug!("snapshots: skipping recent inProgress {name}");
                    continue;
                }

                let size = dir_size_safe(&bpath);
                if dry_run {
                    println!("  [dry] would delete incomplete backup {name} ({})", crate::fmt_bytes(size));
                    freed += size;
                } else {
                    let ok = Command::new("tmutil")
                        .args(["delete", bpath.to_str().unwrap_or("")])
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false);

                    if ok {
                        freed += size;
                        info!("snapshots: deleted incomplete backup {name}");
                    } else {
                        warn!("snapshots: could not delete {name} — try manually with sudo");
                    }
                }
            }
        }
    }

    freed
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else { format!("{}…", &s[..max.saturating_sub(1)]) }
}
