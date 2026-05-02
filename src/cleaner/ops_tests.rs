// cleaner/ops_tests.rs — unit tests for ops.rs
//
// Covers:
//   • safe_remove on a regular file
//   • safe_remove on a directory tree
//   • safe_remove dry-run: no bytes deleted, correct size reported
//   • safe_remove on a symlink: only the link is removed, target survives
//   • safe_remove hard-blocked path: nothing deleted, HardBlocked skip returned
//   • safe_remove missing path: NotFound skip (not an error)
//   • walk_delete_if: selective deletion by predicate
//   • dir_size_safe: correct traversal, no panic on empty dir
//   • dir_size_safe: symlink cycle — does not loop infinitely
//   • safe_remove_old_files: respects age threshold
//   • RemoveResult::merge accumulates freed_bytes

#[cfg(test)]
mod tests {
    use std::{
        fs, io::Write,
        os::unix::fs as unix_fs,
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    };
    use tempfile::{tempdir, TempDir};

    use crate::cleaner::ops::*;
    use crate::error::SkipReason;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn write_file(path: &Path, content: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = fs::File::create(path).unwrap();
        f.write_all(content).unwrap();
    }

    fn set_mtime_old(path: &Path, days_old: u64) {
        // Set mtime to `days_old` days ago via filetime crate alternative:
        // we use std::fs on top of libc futimes.
        let secs = (SystemTime::now()
            - Duration::from_secs(days_old * 86400))
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        unsafe {
            let path_cstr = std::ffi::CString::new(
                path.to_str().unwrap()
            ).unwrap();
            let times = [
                libc::timeval { tv_sec: secs, tv_usec: 0 },
                libc::timeval { tv_sec: secs, tv_usec: 0 },
            ];
            libc::utimes(path_cstr.as_ptr(), times.as_ptr());
        }
    }

    // ── safe_remove ───────────────────────────────────────────────────────────

    #[test]
    fn removes_regular_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.log");
        write_file(&file, b"hello world");
        let size_before = file.metadata().unwrap().len();

        let r = safe_remove(&file, false);
        r.log_errors();

        assert!(!file.exists(), "file should be deleted");
        assert_eq!(r.freed_bytes, size_before);
        assert!(r.errors.is_empty(), "no errors expected");
        assert!(r.skipped.is_empty(), "no skips expected");
    }

    #[test]
    fn removes_directory_tree() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        write_file(&sub.join("a.txt"), b"aaaa");
        write_file(&sub.join("b.txt"), b"bbbbbb");

        let expected = dir_size_safe(&sub);
        assert!(expected > 0);

        let r = safe_remove(&sub, false);
        r.log_errors();

        assert!(!sub.exists(), "directory should be deleted");
        assert_eq!(r.freed_bytes, expected);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn dry_run_does_not_delete() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("keep.txt");
        write_file(&file, b"do not delete me");
        let size_before = file.metadata().unwrap().len();

        let r = safe_remove(&file, true);

        assert!(file.exists(), "dry-run must not delete");
        assert_eq!(r.freed_bytes, size_before, "dry-run should count bytes");
        assert!(r.skipped.iter().any(|s| s.reason == SkipReason::DryRun));
    }

    #[test]
    fn removes_symlink_not_target() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("real.txt");
        let link   = dir.path().join("link.txt");
        write_file(&target, b"real content");
        unix_fs::symlink(&target, &link).unwrap();

        let r = safe_remove(&link, false);
        r.log_errors();

        assert!(!link.exists(),   "symlink should be gone");
        assert!(target.exists(),  "target must survive");
        assert!(r.errors.is_empty());
    }

    #[test]
    fn hard_blocked_path_skipped() {
        // /System is always hard-blocked
        let blocked = PathBuf::from("/System/Library/CoreServices");
        let r = safe_remove(&blocked, false);

        assert_eq!(r.freed_bytes, 0);
        assert!(r.skipped.iter().any(|s| s.reason == SkipReason::HardBlocked));
        assert!(r.errors.is_empty());
    }

    #[test]
    fn missing_path_is_not_found_skip() {
        let path = PathBuf::from("/tmp/hush_test_definitely_does_not_exist_xyz123");
        let r = safe_remove(&path, false);

        assert_eq!(r.freed_bytes, 0);
        // NotFound is demoted to a skip, not an error
        assert!(r.errors.is_empty(), "ENOENT should be a skip, not an error");
        assert!(r.skipped.iter().any(|s| s.reason == SkipReason::NotFound));
    }

    // ── walk_delete_if ────────────────────────────────────────────────────────

    #[test]
    fn walk_delete_if_selective() {
        let dir = tempdir().unwrap();
        write_file(&dir.path().join("keep.txt"),   b"keep");
        write_file(&dir.path().join("delete.log"), b"delete");

        let r = walk_delete_if(dir.path(), false, false, |p| {
            p.extension().and_then(|e| e.to_str()) != Some("log")
        });
        r.log_errors();

        assert!(dir.path().join("keep.txt").exists(),    "keep.txt must stay");
        assert!(!dir.path().join("delete.log").exists(), "delete.log must go");
        assert_eq!(r.freed_bytes, 6); // b"delete".len()
    }

    #[test]
    fn walk_delete_if_recursive() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        write_file(&sub.join("x.DS_Store"), b"meta");
        write_file(&sub.join("real.txt"),   b"content");

        let r = walk_delete_if(dir.path(), true, false, |p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".DS_Store"))
                .unwrap_or(false)
        });
        r.log_errors();

        assert!(!sub.join("x.DS_Store").exists());
        assert!(sub.join("real.txt").exists());
    }

    // ── dir_size_safe ─────────────────────────────────────────────────────────

    #[test]
    fn dir_size_empty_dir_is_zero() {
        let dir = tempdir().unwrap();
        // macOS may report non-zero for the directory entry itself;
        // we just ensure it doesn't panic and is reasonably small.
        let size = dir_size_safe(dir.path());
        assert!(size < 4096, "empty dir should be tiny, got {size}");
    }

    #[test]
    fn dir_size_counts_nested_files() {
        let dir = tempdir().unwrap();
        write_file(&dir.path().join("a.txt"), &[0u8; 1000]);
        write_file(&dir.path().join("sub/b.txt"), &[0u8; 2000]);

        let size = dir_size_safe(dir.path());
        assert!(size >= 3000, "expected >= 3000 bytes, got {size}");
    }

    #[test]
    fn dir_size_does_not_follow_symlink_cycle() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        // Create a symlink cycle: sub/loop → ../ (points back to root)
        let _ = unix_fs::symlink(dir.path(), sub.join("loop"));

        // Must complete without stack overflow or infinite loop
        let size = dir_size_safe(dir.path());
        // Just assert it returned (no panic) and is sane
        assert!(size < 10_000_000, "cycle guard failed, absurd size: {size}");
    }

    #[test]
    fn dir_size_does_not_follow_symlink_to_file() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("real.txt");
        write_file(&target, &[0u8; 500]);
        let link = dir.path().join("link.txt");
        unix_fs::symlink(&target, &link).unwrap();

        let size = dir_size_safe(dir.path());
        // Should count both the target file AND the symlink entry,
        // but NOT double-count via inode guard (hard link protection).
        // At minimum it counts 500 bytes.
        assert!(size >= 500);
    }

    // ── safe_remove_old_files ─────────────────────────────────────────────────

    #[test]
    fn removes_old_files_respects_age() {
        let dir = tempdir().unwrap();
        let old  = dir.path().join("old.crash");
        let new  = dir.path().join("new.crash");

        write_file(&old, b"old crash");
        write_file(&new, b"new crash");

        // Age the old file to 10 days
        set_mtime_old(&old, 10);
        // Leave new file with current mtime

        let max_age = Duration::from_secs(7 * 86400); // 7 days
        let r = safe_remove_old_files(dir.path(), max_age, Some(&["crash"]), false, false);
        r.log_errors();

        assert!(!old.exists(), "10-day-old file should be removed");
        assert!(new.exists(),  "new file should be kept");
        assert_eq!(r.freed_bytes, 9); // b"old crash".len()
    }

    #[test]
    fn extension_filter_skips_non_matching() {
        let dir = tempdir().unwrap();
        let crash = dir.path().join("report.crash");
        let log   = dir.path().join("report.log");
        write_file(&crash, b"crash");
        write_file(&log,   b"log");
        set_mtime_old(&crash, 30);
        set_mtime_old(&log, 30);

        let max_age = Duration::from_secs(1 * 86400);
        let r = safe_remove_old_files(dir.path(), max_age, Some(&["crash"]), false, false);

        assert!(!crash.exists(), "crash file should be removed");
        assert!(log.exists(),   "log file should be kept (wrong ext)");
    }

    // ── RemoveResult ──────────────────────────────────────────────────────────

    #[test]
    fn remove_result_merge_accumulates() {
        let mut a = crate::error::RemoveResult::default();
        a.freed_bytes = 100;

        let mut b = crate::error::RemoveResult::default();
        b.freed_bytes = 200;

        a.merge(b);
        assert_eq!(a.freed_bytes, 300);
    }
}
