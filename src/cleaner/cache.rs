// cleaner/cache.rs — developer cache + project artifact sweeper

use std::path::{Path, PathBuf};
use log::{debug, info, warn};
use crate::config::Config;
use super::ops::{safe_remove, dir_size_safe};

// ── dev-cache sweep ───────────────────────────────────────────────────────────

pub fn sweep_dev_caches(cfg: &Config, dry_run: bool) -> anyhow::Result<u64> {
    if !cfg.categories.dev_caches.enabled { return Ok(0); }
    let mut freed = 0u64;

    for entry in &cfg.categories.dev_caches.entries {
        if entry.is_risky() {
            debug!("cache: skipping risky '{}'", entry.name);
            continue;
        }
        let path = Config::expand(&entry.path);
        if !path.exists() { continue; }
        let size = dir_size_safe(&path);
        if size == 0 { continue; }
        if crate::guard::is_process_using(&path) {
            warn!("cache: skipping '{}' — in use", entry.name);
            continue;
        }
        let freed_now = clear_dir_contents(&path, dry_run);
        if freed_now > 0 {
            let verb = if dry_run { "would free" } else { "freed" };
            println!("  {} {:<42} {} {}",
                entry.risk_emoji(), entry.name, verb, crate::fmt_bytes(freed_now));
            freed += freed_now;
        }
    }

    info!("dev caches: freed {}", crate::fmt_bytes(freed));
    Ok(freed)
}

fn clear_dir_contents(dir: &Path, dry_run: bool) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut freed = 0u64;
    for entry in rd.flatten() {
        let path = entry.path();
        if crate::guard::is_hard_blocked(&path) { continue; }
        let r = safe_remove(&path, dry_run);
        r.log_errors();
        freed += r.freed_bytes;
    }
    freed
}

// ── project artifact sweep ────────────────────────────────────────────────────

pub fn sweep_project_artifacts(cfg: &Config, dry_run: bool) -> anyhow::Result<u64> {
    let cat = &cfg.categories.project_artifacts;
    if !cat.enabled { return Ok(0); }
    let min_bytes = cat.min_size_mb * 1_048_576;
    let mut artifacts: Vec<Artifact> = Vec::new();
    for root_str in &cat.scan_roots {
        let root = Config::expand(root_str);
        if root.exists() {
            find_artifacts(&root, &cat.types, min_bytes, 5, 0, &mut artifacts);
        }
    }
    artifacts.sort_by(|a, b| b.size.cmp(&a.size));

    let mut freed = 0u64;
    for art in &artifacts {
        let stale = art.days_old.map(|d| d > 30).unwrap_or(false);
        let age_str = art.days_old
            .map(|d| format!("{d}d old"))
            .unwrap_or_else(|| "age?".into());
        println!("  {}{:.<38} {} / {} / {}",
            if stale { "⚠ " } else { "  " },
            art.project_name,
            art.artifact_name, age_str,
            crate::fmt_bytes(art.size),
        );
        if dry_run {
            freed += art.size;
        } else {
            let r = safe_remove(&art.artifact_path, false);
            r.log_errors();
            freed += r.freed_bytes;
        }
    }

    info!("project artifacts: freed {}", crate::fmt_bytes(freed));
    Ok(freed)
}

struct Artifact {
    project_name:  String,
    artifact_path: PathBuf,
    artifact_name: String,
    size:          u64,
    days_old:      Option<u64>,
}

static SKIP_DIRS: &[&str] = &[
    "node_modules", ".git", "target", ".build", "build",
    "vendor", ".dart_tool", "Pods", "__pycache__",
    ".venv", "venv", ".terraform", ".gradle",
];

fn find_artifacts(
    dir: &Path, types: &[crate::config::ProjectType],
    min_bytes: u64, max_depth: usize, depth: usize,
    out: &mut Vec<Artifact>,
) {
    if depth >= max_depth { return; }
    let Ok(rd) = std::fs::read_dir(dir) else { return };

    let mut names: Vec<String> = Vec::new();
    for e in rd.flatten() { names.push(e.file_name().to_string_lossy().into_owned()); }

    for pt in types {
        if names.contains(&pt.marker) {
            let artifact_path = dir.join(&pt.artifact);
            if artifact_path.exists() {
                let size = dir_size_safe(&artifact_path);
                if size >= min_bytes {
                    let days_old = artifact_path.metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
                        .map(|d| d.as_secs() / 86400);
                    out.push(Artifact {
                        project_name: dir.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        artifact_path,
                        artifact_name: pt.artifact.clone(),
                        size, days_old,
                    });
                }
                return;
            }
        }
    }

    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".build" { continue; }
        if SKIP_DIRS.contains(&name.as_str()) { continue; }
        let path = e.path();
        if path.is_dir() {
            find_artifacts(&path, types, min_bytes, max_depth, depth + 1, out);
        }
    }
}
