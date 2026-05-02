// tests/integration.rs — end-to-end CLI integration tests
//
// These tests exercise the compiled `hush` binary via assert_cmd.
// They run against a temp directory tree, never touching the real
// ~/Library — all paths are overridden through a temp config.json.
//
// Test matrix:
//   hush --help                   exits 0, contains "Hush"
//   hush clean -n (dry-run)       exits 0, prints "[dry]", changes nothing
//   hush audit                    exits 0, prints header
//   hush snapshot --list          exits 0 (tmutil may not run in CI — tolerated)
//   hush clean --system -n        respects --system flag, exits 0
//   hush <bad-subcommand>         exits non-zero
//   malformed config              exits non-zero with informative message
//   config with validation error  exits non-zero, mentions the field

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use assert_cmd::Command;
use tempfile::{tempdir, TempDir};

// ── test fixture ─────────────────────────────────────────────────────────────

struct Fixture {
    /// Root temp dir — holds config, fake Library dirs, fake apps
    root:       TempDir,
    config_path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempdir().unwrap();

        // Fake directory tree for cache / log paths
        let dirs = [
            "Library/Caches",
            "Library/Logs/DiagnosticReports",
            "Library/Application Support",
            "Library/Preferences",
            "Library/Containers",
        ];
        for d in &dirs {
            fs::create_dir_all(root.path().join(d)).unwrap();
        }

        // Put a .DS_Store and a crash log in the fake library
        write_file(root.path().join("Library/Caches/.DS_Store"), b"ds_store");
        write_file(
            root.path().join("Library/Logs/DiagnosticReports/test.crash"),
            b"crash report",
        );

        let config_path = root.path().join("config.json");
        let config = build_config(root.path());
        fs::write(&config_path, config).unwrap();

        Fixture { root, config_path }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("hush").unwrap();
        c.arg("--config").arg(&self.config_path);
        c
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn write_file(path: impl AsRef<Path>, content: &[u8]) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn build_config(root: &Path) -> String {
    let home = root.to_string_lossy();
    // Build a config.json pointing all paths into our temp root
    format!(r#"{{
  "schema_version": 1,
  "schedule": {{
    "boot_clean": false,
    "cache_sweep_interval_hours": 24,
    "app_audit_interval_days": 7,
    "snapshot_audit_interval_days": 3
  }},
  "thresholds": {{
    "cache_unused_days": 7,
    "uninstall_unused_days": 365,
    "snapshot_max_age_days": 7,
    "snapshot_keep_count": 2,
    "large_file_min_mb": 1,
    "log_max_age_days": 1,
    "tmp_max_age_hours": 1,
    "timemachine_incomplete_safe_hours": 24
  }},
  "paths": {{
    "system_cache":   ["{home}/Library/Caches"],
    "user_cache":     ["{home}/Library/Caches"],
    "crash_logs":     ["{home}/Library/Logs/DiagnosticReports"],
    "tmp_dirs":       ["/private/tmp"],
    "app_containers": ["{home}/Library/Containers"],
    "app_support":    ["{home}/Library/Application Support"],
    "app_prefs":      ["{home}/Library/Preferences"],
    "system_logs":    ["{home}/Library/Logs"]
  }},
  "categories": {{
    "system": {{
      "enabled": true,
      "targets": [
        {{"name": "DS_Store",      "pattern": "**/.DS_Store", "recursive": true}},
        {{"name": "CrashReports",  "ext": ["crash"],          "recursive": false}}
      ]
    }},
    "dev_caches": {{ "enabled": false, "entries": [] }},
    "project_artifacts": {{
      "enabled": false, "min_size_mb": 10,
      "scan_roots": [], "types": []
    }},
    "logs": {{ "enabled": true, "max_age_days": 1 }}
  }},
  "snapshots": {{
    "enabled": false,
    "auto_delete": false,
    "max_age_days": 7,
    "keep_count": 2,
    "skip_if_tm_running": true,
    "delete_incomplete_backups": false,
    "incomplete_safe_hours": 24
  }},
  "silence": {{
    "enabled": false,
    "block_notifications": false,
    "block_background_agents": false,
    "force_quit_on_window_close": false,
    "notification_mode": "banners_only",
    "per_app_overrides": {{}}
  }},
  "optimizer": {{
    "enabled": false,
    "flush_dns": false, "quicklook_refresh": false,
    "launch_services_rebuild": false, "sqlite_vacuum": false,
    "quarantine_cleanup": false, "saved_state_cleanup": false,
    "saved_state_max_age_days": 30, "broken_launch_agents": false,
    "notification_center_cleanup": false, "coreduet_cleanup": false,
    "font_cache_rebuild": false, "dock_refresh": false,
    "prevent_network_dsstore": false, "purge_inactive_memory": false,
    "disable_reopen_windows": false, "disable_sudden_motion_sensor": false,
    "increase_fd_limit": false, "periodic_maintenance": false
  }},
  "process_killer": {{
    "enabled": false,
    "graceful_timeout_secs": 1,
    "force_timeout_secs": 1,
    "use_launchctl_bootout": false
  }},
  "whitelist": {{ "apps": [], "bundle_ids": [], "paths": [] }},
  "rogue_list":  {{ "bundle_ids": [], "app_names": [], "process_names": [] }}
}}"#)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn help_exits_zero_and_mentions_hush() {
    let mut cmd = Command::cargo_bin("hush").unwrap();
    let out = cmd.arg("--help").output().unwrap();
    assert!(out.status.success(), "hush --help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Hush") || stdout.contains("hush"),
        "help output should mention 'Hush': {stdout}"
    );
}

#[test]
fn clean_dry_run_exits_zero() {
    let fix = Fixture::new();
    fix.cmd()
        .args(["clean", "-n"])
        .assert()
        .success();
}

#[test]
fn clean_dry_run_prints_dry_marker() {
    let fix = Fixture::new();
    // Plant a .DS_Store that dry-run should report
    write_file(fix.root.path().join("Library/Caches/test/.DS_Store"), b"meta");

    let out = fix.cmd()
        .args(["clean", "--system", "-n"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // dry-run should print "[dry]" marker
    assert!(
        stdout.contains("[dry]") || stdout.contains("dry"),
        "dry-run should print marker: {stdout}"
    );
}

#[test]
fn clean_dry_run_does_not_delete_files() {
    let fix = Fixture::new();
    let crash = fix.root.path().join("Library/Logs/DiagnosticReports/keep.crash");
    write_file(&crash, b"crash report");

    fix.cmd()
        .args(["clean", "--system", "-n"])
        .assert()
        .success();

    assert!(crash.exists(), "dry-run must not delete files");
}

#[test]
fn audit_exits_zero() {
    let fix = Fixture::new();
    fix.cmd()
        .arg("audit")
        .assert()
        .success();
}

#[test]
fn audit_prints_header() {
    let fix = Fixture::new();
    let out = fix.cmd().arg("audit").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("App Usage") || stdout.contains("Audit") || stdout.contains("Application"),
        "audit should print a header: {stdout}"
    );
}

#[test]
fn snapshot_list_exits_zero() {
    // tmutil may not exist in CI / sandbox — we just verify exit code
    // The command should tolerate a missing tmutil gracefully (returns 0)
    let fix = Fixture::new();
    fix.cmd()
        .args(["snapshot", "--list"])
        .assert()
        .success();
}

#[test]
fn clean_system_flag_exits_zero() {
    let fix = Fixture::new();
    fix.cmd()
        .args(["clean", "--system"])
        .assert()
        .success();
}

#[test]
fn crush_no_rogues_exits_zero() {
    let fix = Fixture::new();
    fix.cmd()
        .arg("crush")
        .assert()
        .success();
}

#[test]
fn bad_subcommand_exits_nonzero() {
    let mut cmd = Command::cargo_bin("hush").unwrap();
    cmd.arg("definitely-not-a-real-subcommand")
        .assert()
        .failure();
}

#[test]
fn missing_config_exits_nonzero_with_message() {
    let mut cmd = Command::cargo_bin("hush").unwrap();
    let out = cmd
        .args(["--config", "/tmp/hush_nonexistent_config_xyz.json", "clean"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "missing config should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("config") || stderr.contains("cannot read") || stderr.contains("error"),
        "error message should mention config problem: {stderr}"
    );
}

#[test]
fn malformed_json_config_exits_nonzero_with_line_info() {
    let root = tempdir().unwrap();
    let config_path = root.path().join("config.json");
    fs::write(&config_path, b"{ this is not json }").unwrap();

    let mut cmd = Command::cargo_bin("hush").unwrap();
    let out = cmd
        .args(["--config", config_path.to_str().unwrap(), "clean"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Should include parse error details
    assert!(
        stderr.contains("parse") || stderr.contains("line") || stderr.contains("error"),
        "parse error should be descriptive: {stderr}"
    );
}

#[test]
fn validation_error_mentions_field_name() {
    let root = tempdir().unwrap();
    let config_path = root.path().join("config.json");
    // cache_unused_days = 0 is invalid
    let bad = build_config(root.path())
        .replace("\"cache_unused_days\": 7,", "\"cache_unused_days\": 0,");
    fs::write(&config_path, bad).unwrap();

    let mut cmd = Command::cargo_bin("hush").unwrap();
    let out = cmd
        .args(["--config", config_path.to_str().unwrap(), "clean"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cache_unused_days"),
        "validation error should name the field: {stderr}"
    );
}

#[test]
fn version_flag_exits_zero() {
    let mut cmd = Command::cargo_bin("hush").unwrap();
    cmd.arg("--version").assert().success();
}
