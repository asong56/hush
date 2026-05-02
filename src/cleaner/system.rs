// cleaner/system.rs — boot-time system junk removal

use std::{ffi::OsStr, path::Path, process::Command, time::Duration};
use log::info;
use crate::config::Config;
use super::ops::{safe_remove, safe_remove_old_files, walk_delete_if};

pub fn clean_ds_store(cfg: &Config, dry_run: bool) -> anyhow::Result<u64> {
    let mut freed = 0u64;
    let roots: Vec<_> = std::iter::once(home_dir())
        .chain(cfg.paths.user_cache.iter().map(|p| Config::expand(p)))
        .collect();
    for root in &roots {
        if root.exists() {
            let r = walk_delete_if(root, true, dry_run, |p| {
                p.file_name() != Some(OsStr::new(".DS_Store"))
            });
            r.log_errors();
            freed += r.freed_bytes;
        }
    }
    let vols = Path::new("/Volumes");
    if vols.exists() {
        let r = walk_delete_if(vols, true, dry_run, |p| {
            p.file_name() != Some(OsStr::new(".DS_Store"))
        });
        r.log_errors();
        freed += r.freed_bytes;
    }
    info!("DS_Store: freed {}", crate::fmt_bytes(freed));
    Ok(freed)
}

pub fn clean_apple_double(cfg: &Config, dry_run: bool) -> anyhow::Result<u64> {
    let mut freed = 0u64;
    let roots: Vec<_> = std::iter::once(home_dir())
        .chain(cfg.paths.user_cache.iter().map(|p| Config::expand(p)))
        .collect();
    for root in &roots {
        if root.exists() {
            let r = walk_delete_if(root, true, dry_run, |p| {
                !p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("._"))
                    .unwrap_or(false)
            });
            r.log_errors();
            freed += r.freed_bytes;
        }
    }
    info!("AppleDouble: freed {}", crate::fmt_bytes(freed));
    Ok(freed)
}

pub fn clean_crash_logs(cfg: &Config, dry_run: bool) -> anyhow::Result<u64> {
    let max_age = Duration::from_secs(cfg.thresholds.log_max_age_days * 86400);
    let crash_exts = &["crash", "spin", "hang", "ips", "diag"][..];
    let mut freed = 0u64;
    for p in &cfg.paths.crash_logs {
        let dir = Config::expand(p);
        if !dir.exists() { continue; }
        let r = safe_remove_old_files(&dir, max_age, Some(crash_exts), false, dry_run);
        r.log_errors();
        freed += r.freed_bytes;
    }
    for target in &cfg.categories.system.targets {
        if let Some(path) = &target.path {
            let dir = Config::expand(path);
            if !dir.exists() { continue; }
            let age = Duration::from_secs(
                target.max_age_days.unwrap_or(cfg.thresholds.log_max_age_days) * 86400
            );
            let r = safe_remove_old_files(&dir, age, None, true, dry_run);
            r.log_errors();
            freed += r.freed_bytes;
        }
    }
    info!("Crash/diagnostic logs: freed {}", crate::fmt_bytes(freed));
    Ok(freed)
}

pub fn clean_tmp(cfg: &Config, dry_run: bool) -> anyhow::Result<u64> {
    let max_age = Duration::from_secs(cfg.thresholds.tmp_max_age_hours * 3600);
    let mut freed = 0u64;
    for p in &cfg.paths.tmp_dirs {
        let dir = Config::expand(p);
        if dir.exists() {
            let r = safe_remove_old_files(&dir, max_age, None, false, dry_run);
            r.log_errors();
            freed += r.freed_bytes;
        }
    }
    info!("Tmp: freed {}", crate::fmt_bytes(freed));
    Ok(freed)
}

pub fn clean_system_logs(cfg: &Config, dry_run: bool) -> anyhow::Result<u64> {
    let max_age = Duration::from_secs(cfg.thresholds.log_max_age_days * 86400);
    let mut freed = 0u64;
    for p in &cfg.paths.system_logs {
        let dir = Config::expand(p);
        if !dir.exists() { continue; }
        let r = safe_remove_old_files(
            &dir, max_age, Some(&["log", "gz", "asl", "tracev3"]), true, dry_run,
        );
        r.log_errors();
        freed += r.freed_bytes;
    }
    info!("System logs: freed {}", crate::fmt_bytes(freed));
    Ok(freed)
}

pub fn reindex_spotlight() -> anyhow::Result<()> {
    // SAFETY: geteuid() has no preconditions; always safe to call.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        log::warn!("reindex_spotlight: requires root — skipping");
        return Ok(());
    }
    let _ = Command::new("mdutil").args(["-i", "off", "/"]).output();
    let si = Path::new("/.Spotlight-V100");
    if si.exists() { let _ = std::fs::remove_dir_all(si); }
    let _ = Command::new("mdutil").args(["-i", "on", "/"]).output();
    info!("Spotlight: reindex started");
    Ok(())
}

fn home_dir() -> std::path::PathBuf {
    std::env::var("HOME").map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/"))
}
