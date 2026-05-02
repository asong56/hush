// error_tests.rs — tests for HushError classification and RemoveResult behaviour

#[cfg(test)]
mod tests {
    use std::{io, path::PathBuf};
    use crate::error::*;

    // ── HushError::from_io ────────────────────────────────────────────────────

    #[test]
    fn from_io_not_found_classified() {
        let e = io::Error::from(io::ErrorKind::NotFound);
        let he = HushError::from_io(e, "/tmp/gone");
        assert!(matches!(he, HushError::NotFound(_)));
        assert!(he.is_skippable());
    }

    #[test]
    fn from_io_permission_denied_classified() {
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        let he = HushError::from_io(e, "/etc/sudoers");
        assert!(matches!(he, HushError::PermissionDenied { .. }));
        assert!(!he.is_skippable(), "EACCES is a real error, not skippable");
    }

    #[test]
    fn from_io_ebusy_classified() {
        // EBUSY = raw OS error 16 on macOS
        let e = io::Error::from_raw_os_error(16);
        let he = HushError::from_io(e, "/dev/disk0");
        // On macOS this should classify as Busy; on others fallback to Io
        // Either way, it must not classify as NotFound or HardBlocked
        assert!(!matches!(he, HushError::NotFound(_)));
        assert!(!matches!(he, HushError::HardBlocked(_)));
    }

    // ── is_skippable ─────────────────────────────────────────────────────────

    #[test]
    fn not_found_is_skippable() {
        assert!(HushError::NotFound(PathBuf::from("/gone")).is_skippable());
    }

    #[test]
    fn hard_blocked_is_skippable() {
        assert!(HushError::HardBlocked(PathBuf::from("/System")).is_skippable());
    }

    #[test]
    fn in_use_is_skippable() {
        assert!(HushError::InUse(PathBuf::from("/tmp/app")).is_skippable());
    }

    #[test]
    fn symlink_loop_is_skippable() {
        assert!(HushError::SymlinkLoop(PathBuf::from("/tmp/loop")).is_skippable());
    }

    #[test]
    fn permission_denied_is_not_skippable() {
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(!HushError::PermissionDenied { path: PathBuf::from("/etc"), source: e }
            .is_skippable());
    }

    // ── RemoveResult ──────────────────────────────────────────────────────────

    #[test]
    fn add_error_skippable_becomes_skip() {
        let mut r = RemoveResult::default();
        r.add_error(HushError::NotFound(PathBuf::from("/gone")));

        assert!(r.errors.is_empty(), "skippable error should become a skip entry");
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(r.skipped[0].reason, SkipReason::NotFound);
    }

    #[test]
    fn add_error_permission_denied_stays_as_error() {
        let mut r = RemoveResult::default();
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        r.add_error(HushError::PermissionDenied { path: PathBuf::from("/etc"), source: e });

        assert_eq!(r.errors.len(), 1, "EACCES must remain in errors, not demoted to skip");
        assert!(r.skipped.is_empty());
    }

    #[test]
    fn has_permission_errors_true_when_present() {
        let mut r = RemoveResult::default();
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        r.add_error(HushError::PermissionDenied { path: PathBuf::from("/etc"), source: e });
        assert!(r.has_permission_errors());
    }

    #[test]
    fn has_permission_errors_false_when_absent() {
        let mut r = RemoveResult::default();
        r.add_error(HushError::NotFound(PathBuf::from("/gone")));
        assert!(!r.has_permission_errors());
    }

    #[test]
    fn merge_accumulates_all_fields() {
        let mut a = RemoveResult::default();
        a.freed_bytes = 100;
        a.add_skip(PathBuf::from("/a"), SkipReason::DryRun);
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        a.add_error(HushError::PermissionDenied { path: PathBuf::from("/b"), source: e });

        let mut b = RemoveResult::default();
        b.freed_bytes = 200;
        b.add_skip(PathBuf::from("/c"), SkipReason::HardBlocked);

        a.merge(b);

        assert_eq!(a.freed_bytes, 300);
        assert_eq!(a.skipped.len(), 2);
        assert_eq!(a.errors.len(), 1);
    }

    // ── Display ───────────────────────────────────────────────────────────────

    #[test]
    fn display_includes_path() {
        let e = HushError::NotFound(PathBuf::from("/some/path"));
        let s = format!("{e}");
        assert!(s.contains("/some/path"), "display should include path: {s}");
    }

    #[test]
    fn display_external_tool_includes_tool_name() {
        let e = HushError::ExternalTool { tool: "tmutil", detail: "exit 1".into() };
        let s = format!("{e}");
        assert!(s.contains("tmutil"), "display should include tool name: {s}");
    }
}
