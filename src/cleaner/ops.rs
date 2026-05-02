// cleaner/ops.rs — safe, classified file operations

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use log::debug;
use crate::error::{HushError, RemoveResult, SkipReason};
use crate::guard;

const LSOF_THRESHOLD: u64 = 512 * 1_048_576;

// ── primary API ───────────────────────────────────────────────────────────────

pub fn safe_remove(path: &Path, dry_run: bool) -> RemoveResult {
    let mut result = RemoveResult::default();

    if guard::is_hard_blocked(path) {
        result.add_skip(path.to_owned(), SkipReason::HardBlocked);
        return result;
    }

    let meta = match fs::symlink_metadata(path) {
        Ok(m)  => m,
        Err(e) => { result.add_error(HushError::from_io(e, path)); return result; }
    };

    let size = if meta.file_type().is_symlink() {
        0
    } else if meta.is_dir() {
        dir_size_safe(path)
    } else {
        meta.len()
    };

    if meta.is_dir() && size > LSOF_THRESHOLD && guard::is_process_using(path) {
        result.add_skip(path.to_owned(), SkipReason::ProcessInUse);
        return result;
    }

    if dry_run {
        if size > 0 {
            println!("  [dry]  {:<60} {}", path.display(), crate::fmt_bytes(size));
        }
        result.add_skip(path.to_owned(), SkipReason::DryRun);
        result.freed_bytes += size;
        return result;
    }

    let del = if meta.file_type().is_symlink() || meta.is_file() {
        fs::remove_file(path)
    } else {
        fs::remove_dir_all(path)
    };

    match del {
        Ok(()) => {
            debug!("removed {} ({})", path.display(), crate::fmt_bytes(size));
            result.freed_bytes += size;
        }
        Err(e) => result.add_error(HushError::from_io(e, path)),
    }

    result
}

pub fn safe_remove_old_files(
    root: &Path, max_age: Duration, exts: Option<&[&str]>,
    recursive: bool, dry_run: bool,
) -> RemoveResult {
    let mut result = RemoveResult::default();

    if guard::is_hard_blocked(root) {
        result.add_skip(root.to_owned(), SkipReason::HardBlocked);
        return result;
    }
    if !root.is_dir() { return result; }

    let cutoff = SystemTime::now().checked_sub(max_age).unwrap_or(SystemTime::UNIX_EPOCH);

    let rd = match fs::read_dir(root) {
        Ok(r)  => r,
        Err(e) => { result.add_error(HushError::from_io(e, root)); return result; }
    };

    for entry in rd.flatten() {
        let path = entry.path();
        if guard::is_hard_blocked(&path) {
            result.add_skip(path, SkipReason::HardBlocked);
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m)  => m,
            Err(e) => { result.add_error(HushError::from_io(e, &path)); continue; }
        };
        if meta.is_dir() {
            if recursive {
                result.merge(safe_remove_old_files(&path, max_age, exts, true, dry_run));
            }
            continue;
        }
        match meta.modified() {
            Ok(mtime) if mtime >= cutoff => continue,
            Err(e) => { result.add_error(HushError::from_io(e, &path)); continue; }
            _ => {}
        }
        if let Some(exts) = exts {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !exts.contains(&ext) { continue; }
        }
        result.merge(safe_remove(&path, dry_run));
    }
    result
}

pub fn walk_delete_if<F>(root: &Path, recursive: bool, dry_run: bool, keep: F) -> RemoveResult
where F: Fn(&Path) -> bool
{
    let mut result = RemoveResult::default();
    if guard::is_hard_blocked(root) {
        result.add_skip(root.to_owned(), SkipReason::HardBlocked);
        return result;
    }
    let rd = match fs::read_dir(root) {
        Ok(r)  => r,
        Err(e) => { result.add_error(HushError::from_io(e, root)); return result; }
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m)  => m,
            Err(e) => { result.add_error(HushError::from_io(e, &path)); continue; }
        };
        if meta.is_dir() {
            if recursive { result.merge(walk_delete_if(&path, true, dry_run, &keep)); }
        } else if !keep(&path) {
            result.merge(safe_remove(&path, dry_run));
        }
    }
    result
}

// ── size — cycle-safe ─────────────────────────────────────────────────────────

pub fn dir_size_safe(root: &Path) -> u64 {
    let mut visited = HashSet::new();
    dir_size_inner(root, &mut visited)
}

fn dir_size_inner(path: &Path, visited: &mut HashSet<u64>) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(m)  => m,
        Err(_) => return 0,
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let inode = meta.ino();
        if !visited.insert(inode) {
            debug!("dir_size: cycle at {} (inode {})", path.display(), inode);
            return 0;
        }
    }

    if meta.file_type().is_symlink() { return meta.len(); }
    if !meta.is_dir()               { return meta.len(); }

    let rd = match fs::read_dir(path) {
        Ok(r)  => r,
        Err(_) => return 0,
    };
    rd.flatten().fold(0u64, |acc, e| acc + dir_size_inner(&e.path(), visited))
}

pub fn file_size_safe(path: &Path) -> u64 {
    fs::symlink_metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
