// guard_tests.rs — exhaustive tests for the safety layer
//
// These are the most critical tests in the codebase.
// Every hard-block rule, critical bundle check, and TCC guard has a test.
// A regression here could lead to data loss or codesign damage.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use crate::guard::*;

    // ── is_hard_blocked — absolute paths ─────────────────────────────────────

    #[test]
    fn blocks_filesystem_root() {
        assert!(is_hard_blocked(&PathBuf::from("/")));
    }

    #[test]
    fn blocks_system_dirs() {
        for path in &["/System", "/bin", "/sbin", "/usr", "/Library", "/private", "/etc"] {
            assert!(is_hard_blocked(&PathBuf::from(path)),
                "expected hard-block for {path}");
        }
    }

    #[test]
    fn blocks_system_subdirs() {
        assert!(is_hard_blocked(&PathBuf::from("/System/Library/CoreServices")));
        assert!(is_hard_blocked(&PathBuf::from("/usr/bin/python3")));
        assert!(is_hard_blocked(&PathBuf::from("/Library/Extensions/SomeKEXT.kext")));
    }

    // ── is_hard_blocked — codesign-critical paths ─────────────────────────────

    #[test]
    fn blocks_app_bundle_contents() {
        // Any path inside .app/Contents/ is blocked (codesign tree)
        let paths = [
            "/Applications/Safari.app/Contents/MacOS/Safari",
            "/Applications/MyApp.app/Contents/Info.plist",
            "/Applications/MyApp.app/Contents/Resources/en.lproj/MainMenu.nib",
        ];
        for p in &paths {
            assert!(is_hard_blocked(&PathBuf::from(p)),
                "expected hard-block for {p} (inside .app bundle)");
        }
    }

    #[test]
    fn does_not_block_the_app_itself() {
        // The .app directory itself (not Contents/) should NOT be blocked
        // — it's the uninstaller's job to remove it entirely
        let app = PathBuf::from("/Applications/MyApp.app");
        assert!(!is_hard_blocked(&app),
            ".app root should not be hard-blocked (uninstall removes the whole bundle)");
    }

    // ── is_hard_blocked — critical caches ────────────────────────────────────

    #[test]
    fn blocks_critical_system_prefs_caches() {
        let cases = [
            "/Users/user/Library/Caches/com.apple.systempreferences",
            "/Users/user/Library/Caches/com.apple.SystemSettings",
            "/Users/user/Library/Caches/com.apple.controlcenter",
        ];
        for p in &cases {
            assert!(is_hard_blocked(&PathBuf::from(p)), "must block {p}");
        }
    }

    #[test]
    fn blocks_audio_system_caches() {
        assert!(is_hard_blocked(&PathBuf::from(
            "/Users/user/Library/Caches/com.apple.coreaudio"
        )));
        assert!(is_hard_blocked(&PathBuf::from(
            "/Users/user/Library/Caches/com.apple.audio.HAL"
        )));
    }

    #[test]
    fn blocks_security_and_keychain() {
        assert!(is_hard_blocked(&PathBuf::from(
            "/Users/user/Library/Keychains/login.keychain-db"
        )));
        // com.apple.security.* segment
        assert!(is_hard_blocked(&PathBuf::from(
            "/Users/user/Library/Caches/com.apple.security.pboxd"
        )));
    }

    #[test]
    fn blocks_icloud_drive_sync() {
        assert!(is_hard_blocked(&PathBuf::from(
            "/Users/user/Library/Caches/com.apple.bird"
        )));
        assert!(is_hard_blocked(&PathBuf::from(
            "/Users/user/Library/CloudStorage/com.apple.CloudDocs"
        )));
    }

    #[test]
    fn blocks_cups_printer_config() {
        assert!(is_hard_blocked(&PathBuf::from(
            "/Users/user/Library/Preferences/org.cups.PrintingPrefs.plist"
        )));
    }

    #[test]
    fn blocks_user_trash() {
        assert!(is_hard_blocked(&PathBuf::from("/Users/user/.Trash/photo.jpg")));
    }

    // ── is_hard_blocked — /Library allow-list ────────────────────────────────

    #[test]
    fn allows_library_caches() {
        assert!(!is_hard_blocked(&PathBuf::from("/Library/Caches/com.example.app")));
    }

    #[test]
    fn allows_library_logs() {
        assert!(!is_hard_blocked(&PathBuf::from("/Library/Logs/some.log")));
    }

    #[test]
    fn allows_library_launch_agents() {
        assert!(!is_hard_blocked(&PathBuf::from(
            "/Library/LaunchAgents/com.example.agent.plist"
        )));
    }

    #[test]
    fn blocks_library_outside_allowlist() {
        // /Library/Saved Application State is NOT in the allow-list
        assert!(is_hard_blocked(&PathBuf::from(
            "/Library/Saved Application State/com.foo.bar.savedState"
        )));
    }

    // ── is_inside_app_bundle ──────────────────────────────────────────────────

    #[test]
    fn detects_inside_bundle() {
        assert!(is_inside_app_bundle(&PathBuf::from(
            "/Applications/Foo.app/Contents/MacOS/Foo"
        )));
        assert!(is_inside_app_bundle(&PathBuf::from(
            "/Applications/Foo.app/Contents/Frameworks/Foo.framework/Foo"
        )));
    }

    #[test]
    fn not_inside_bundle_for_bundle_root() {
        assert!(!is_inside_app_bundle(&PathBuf::from("/Applications/Foo.app")));
        assert!(!is_inside_app_bundle(&PathBuf::from("/Applications/Foo.app/Contents")));
    }

    // ── requires_fda ─────────────────────────────────────────────────────────

    #[test]
    fn fda_paths_detected() {
        let protected = [
            "/Users/user/Library/Mail/V10/MailData/Envelope Index",
            "/Users/user/Library/Messages/chat.db",
            "/Users/user/Library/Safari/History.db",
            "/private/var/db/diagnostics/system.logarchive",
            "/private/var/folders/xx/abc123/T/some.tmp",
        ];
        for p in &protected {
            assert!(requires_fda(&PathBuf::from(p)), "expected FDA required for {p}");
        }
    }

    #[test]
    fn non_fda_paths_clear() {
        let safe = [
            "/Users/user/Library/Caches/com.example.app",
            "/Users/user/Library/Logs/MyApp/app.log",
            "/tmp/scratch.txt",
        ];
        for p in &safe {
            assert!(!requires_fda(&PathBuf::from(p)), "expected NOT FDA for {p}");
        }
    }

    // ── is_critical_bundle ────────────────────────────────────────────────────

    #[test]
    fn com_apple_bundles_are_critical_by_default() {
        let critical = [
            "com.apple.finder",
            "com.apple.dock",
            "com.apple.systemuiserver",
            "com.apple.notificationcenterui",
            "com.apple.security.pboxd",
        ];
        for bid in &critical {
            assert!(is_critical_bundle(bid), "expected critical: {bid}");
        }
    }

    #[test]
    fn safe_apple_caches_are_not_critical() {
        let safe = [
            "com.apple.dt.Xcode",
            "com.apple.dt.XCBuild",
            "com.apple.CoreSimulator",
            "com.apple.iphonesimulator",
            "org.swift.swiftpm",
        ];
        for bid in &safe {
            assert!(!is_critical_bundle(bid), "expected NOT critical: {bid}");
        }
    }

    #[test]
    fn third_party_non_password_managers_are_not_critical() {
        // Random apps: not critical (Hush may manage their caches)
        assert!(!is_critical_bundle("com.spotify.client"));
        assert!(!is_critical_bundle("com.github.atom"));
        assert!(!is_critical_bundle("com.visualstudio.code"));
    }

    #[test]
    fn password_managers_are_critical() {
        assert!(is_critical_bundle("com.1password.1password"));
        assert!(is_critical_bundle("com.agilebits.onepassword7"));
        assert!(is_critical_bundle("com.bitwarden.desktop"));
    }
}
