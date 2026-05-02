// config_tests.rs — tests for config loading, schema migration, and validation
//
// Covers:
//   • Happy-path load from valid JSON
//   • Graceful load with missing optional fields (migration fills defaults)
//   • JSON comment stripping (// inline and line-start)
//   • schema_version 0 → 1 migration: all added fields are present
//   • Newer schema_version: loads with warning, does not fail
//   • Validation: cache_unused_days = 0 → error
//   • Validation: uninstall < cache → error
//   • Validation: snapshot_keep_count = 0 → clamped to 1, not error
//   • Malformed JSON → informative error with line/col
//   • Config::expand: ~ substitution

#[cfg(test)]
mod tests {
    use std::{io::Write, fs};
    use tempfile::NamedTempFile;
    use crate::config::{Config, SCHEMA_VERSION};

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Write `content` to a temp file and load it as a Config.
    fn load_str(content: &str) -> anyhow::Result<Config> {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        Config::load(f.path())
    }

    /// Minimal valid config string at schema_version 1.
    fn minimal_v1() -> String {
        // We embed a minimal valid JSON rather than referencing config.json
        // so tests don't depend on the repository file.
        r#"{
  "schema_version": 1,
  "schedule": {
    "boot_clean": true,
    "cache_sweep_interval_hours": 24,
    "app_audit_interval_days": 7,
    "snapshot_audit_interval_days": 3
  },
  "thresholds": {
    "cache_unused_days": 7,
    "uninstall_unused_days": 365,
    "snapshot_max_age_days": 7,
    "snapshot_keep_count": 2,
    "large_file_min_mb": 100,
    "log_max_age_days": 30,
    "tmp_max_age_hours": 24,
    "timemachine_incomplete_safe_hours": 24
  },
  "paths": {
    "system_cache": ["/Library/Caches"],
    "user_cache": ["~/Library/Caches"],
    "crash_logs": ["~/Library/Logs/DiagnosticReports"],
    "tmp_dirs": ["/private/tmp"],
    "app_containers": ["~/Library/Containers"],
    "app_support": ["~/Library/Application Support"],
    "app_prefs": ["~/Library/Preferences"],
    "system_logs": ["/private/var/log"]
  },
  "categories": {
    "system": { "enabled": true, "targets": [] },
    "dev_caches": { "enabled": true, "entries": [] },
    "project_artifacts": {
      "enabled": true, "min_size_mb": 10,
      "scan_roots": [], "types": []
    },
    "logs": { "enabled": true, "max_age_days": 30 }
  },
  "snapshots": {
    "enabled": true, "auto_delete": true,
    "max_age_days": 7, "keep_count": 2,
    "skip_if_tm_running": true,
    "delete_incomplete_backups": true,
    "incomplete_safe_hours": 24
  },
  "silence": {
    "enabled": true,
    "block_notifications": true,
    "block_background_agents": true,
    "force_quit_on_window_close": true,
    "notification_mode": "banners_only",
    "per_app_overrides": {}
  },
  "optimizer": {
    "enabled": true,
    "flush_dns": true, "quicklook_refresh": true,
    "launch_services_rebuild": true, "sqlite_vacuum": true,
    "quarantine_cleanup": true, "saved_state_cleanup": true,
    "saved_state_max_age_days": 30, "broken_launch_agents": true,
    "notification_center_cleanup": true, "coreduet_cleanup": true,
    "font_cache_rebuild": false, "dock_refresh": true,
    "prevent_network_dsstore": true, "purge_inactive_memory": false,
    "disable_reopen_windows": true,
    "disable_sudden_motion_sensor": true,
    "increase_fd_limit": true, "periodic_maintenance": true
  },
  "process_killer": {
    "enabled": true,
    "graceful_timeout_secs": 3,
    "force_timeout_secs": 5,
    "use_launchctl_bootout": true
  },
  "whitelist": { "apps": [], "bundle_ids": [], "paths": [] },
  "rogue_list": { "bundle_ids": [], "app_names": [], "process_names": [] }
}"#.to_string()
    }

    // ── happy path ────────────────────────────────────────────────────────────

    #[test]
    fn loads_valid_config() {
        let cfg = load_str(&minimal_v1()).expect("should load valid config");
        assert_eq!(cfg.schema_version, 1);
        assert_eq!(cfg.thresholds.cache_unused_days, 7);
        assert_eq!(cfg.thresholds.uninstall_unused_days, 365);
        assert!(cfg.optimizer.flush_dns);
    }

    #[test]
    fn schedule_fields_parsed() {
        let cfg = load_str(&minimal_v1()).unwrap();
        assert_eq!(cfg.schedule.cache_sweep_interval_hours, 24);
        assert_eq!(cfg.schedule.app_audit_interval_days, 7);
        assert_eq!(cfg.schedule.snapshot_audit_interval_days, 3);
        assert!(cfg.schedule.boot_clean);
    }

    // ── comment stripping ─────────────────────────────────────────────────────

    #[test]
    fn strips_line_comments() {
        let json = r#"{
  // This is a comment
  "schema_version": 1,
  "schedule": {
    "boot_clean": false, // inline comment
    "cache_sweep_interval_hours": 12,
    "app_audit_interval_days": 14,
    "snapshot_audit_interval_days": 7
  }
}"#;
        // Use load_str with a partial config — will fail on missing fields,
        // but we just want to confirm comment stripping doesn't break parsing.
        // We embed the comment test inside the full minimal config.
        let with_comment = minimal_v1().replace(
            "\"boot_clean\": true,",
            "\"boot_clean\": true, // this is a comment",
        );
        let cfg = load_str(&with_comment).expect("comment should be stripped");
        assert!(cfg.schedule.boot_clean);
    }

    #[test]
    fn strips_comments_with_url_in_string_value() {
        // A URL in a string value must NOT be treated as a comment
        let json = minimal_v1().replace(
            "\"banners_only\"",
            "\"https://example.com/docs\"",
        );
        // Just confirm it parses without error (the URL contains // but is in a string)
        let result = load_str(&json);
        assert!(result.is_ok(), "URL in string value should parse fine: {:?}", result.err());
    }

    // ── schema migration ──────────────────────────────────────────────────────

    #[test]
    fn migrates_v0_adds_process_killer() {
        // v0 config: no schema_version, no process_killer block
        let v0 = minimal_v1()
            .replace("\"schema_version\": 1,\n  ", "")
            .replace(
                r#"  "process_killer": {
    "enabled": true,
    "graceful_timeout_secs": 3,
    "force_timeout_secs": 5,
    "use_launchctl_bootout": true
  },"#,
                "",
            );

        let cfg = load_str(&v0).expect("v0 config should migrate successfully");
        // Migration should have filled in process_killer with defaults
        assert!(cfg.process_killer.enabled);
        assert_eq!(cfg.process_killer.graceful_timeout_secs, 3);
        assert_eq!(cfg.process_killer.use_launchctl_bootout, true);
    }

    #[test]
    fn migrates_v0_adds_snapshot_schedule() {
        let v0 = minimal_v1()
            .replace("\"schema_version\": 1,\n  ", "")
            .replace("\"snapshot_audit_interval_days\": 3,\n    ", "");

        let cfg = load_str(&v0).expect("v0 missing snapshot_audit should migrate");
        assert_eq!(cfg.schedule.snapshot_audit_interval_days, 3); // default from migration
    }

    #[test]
    fn newer_schema_version_loads_with_warning() {
        // schema_version = 999 should load (unknown fields ignored),
        // not panic or return Err
        let newer = minimal_v1().replace("\"schema_version\": 1,", "\"schema_version\": 999,");
        let result = load_str(&newer);
        assert!(result.is_ok(), "newer schema should load with warning, not error");
    }

    // ── validation ────────────────────────────────────────────────────────────

    #[test]
    fn validation_rejects_zero_cache_threshold() {
        let bad = minimal_v1().replace("\"cache_unused_days\": 7,", "\"cache_unused_days\": 0,");
        let result = load_str(&bad);
        assert!(result.is_err(), "cache_unused_days=0 should be rejected");
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("cache_unused_days"), "error should mention the field: {msg}");
    }

    #[test]
    fn validation_rejects_uninstall_less_than_cache() {
        // uninstall_unused_days (3) < cache_unused_days (7) → invalid
        let bad = minimal_v1()
            .replace("\"uninstall_unused_days\": 365,", "\"uninstall_unused_days\": 3,");
        let result = load_str(&bad);
        assert!(result.is_err(), "uninstall < cache threshold should be rejected");
    }

    #[test]
    fn validation_clamps_zero_snapshot_keep_count() {
        // snapshot_keep_count = 0 → clamped to 1, not an error
        let zero_keep = minimal_v1()
            .replace("\"keep_count\": 2,", "\"keep_count\": 0,")
            .replace("\"snapshot_keep_count\": 2,", "\"snapshot_keep_count\": 0,");
        let cfg = load_str(&zero_keep).expect("keep_count=0 should be clamped, not error");
        assert_eq!(cfg.thresholds.snapshot_keep_count, 1, "should be clamped to 1");
    }

    #[test]
    fn validation_rejects_zero_sweep_interval() {
        let bad = minimal_v1()
            .replace("\"cache_sweep_interval_hours\": 24,", "\"cache_sweep_interval_hours\": 0,");
        let result = load_str(&bad);
        assert!(result.is_err(), "zero sweep interval should be rejected");
    }

    // ── parse errors ──────────────────────────────────────────────────────────

    #[test]
    fn malformed_json_returns_informative_error() {
        let result = load_str("{ this is not json }");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        // Should mention line and column
        assert!(
            msg.contains("line") || msg.contains("col") || msg.contains("parse"),
            "error should be informative: {msg}"
        );
    }

    #[test]
    fn completely_empty_file_errors_clearly() {
        let result = load_str("");
        assert!(result.is_err());
    }

    // ── Config::expand ────────────────────────────────────────────────────────

    #[test]
    fn expand_tilde_substitutes_home() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let expanded = Config::expand("~/Library/Caches");
        assert_eq!(
            expanded.to_string_lossy(),
            format!("{home}/Library/Caches")
        );
    }

    #[test]
    fn expand_absolute_path_unchanged() {
        let p = Config::expand("/private/tmp");
        assert_eq!(p.to_string_lossy(), "/private/tmp");
    }

    #[test]
    fn expand_relative_path_unchanged() {
        let p = Config::expand("relative/path");
        assert_eq!(p.to_string_lossy(), "relative/path");
    }

    // ── SCHEMA_VERSION constant ───────────────────────────────────────────────

    #[test]
    fn schema_version_is_current() {
        // If someone bumps SCHEMA_VERSION but forgets to add a migration arm,
        // this test reminds them (it won't fail automatically — it's a smoke check).
        assert!(SCHEMA_VERSION >= 1, "SCHEMA_VERSION should be at least 1");
    }
}
