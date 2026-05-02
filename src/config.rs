// config.rs — serde config structs + kqueue EVFILT_VNODE hot-reload
//
// Hot-reload design:
//   Config::watch(path) → spawns a 64 KB-stack thread that blocks on kqueue.
//   On NOTE_WRITE or NOTE_RENAME (atomic editor saves), it re-parses and
//   sends the new Config over an mpsc channel.
//   The daemon loop calls try_recv() — zero syscall cost when idle.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};
use log::warn;

// ── top-level ─────────────────────────────────────────────────────────────────

// Current config schema version. Increment when adding required fields.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Schema version — used for migration. Defaults to 0 (legacy) if absent.
    #[serde(default)]
    pub schema_version: u32,
    pub schedule:         Schedule,
    pub thresholds:       Thresholds,
    pub paths:            Paths,
    pub categories:       Categories,
    pub snapshots:        Snapshots,
    pub silence:          Silence,
    pub optimizer:        Optimizer,
    pub process_killer:   ProcessKiller,
    pub whitelist:        Whitelist,
    pub rogue_list:       RogueList,
}

// ── schedule ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Schedule {
    pub boot_clean:                  bool,
    pub cache_sweep_interval_hours:  u64,
    pub app_audit_interval_days:     u64,
    pub snapshot_audit_interval_days: u64,
}

// ── thresholds ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Thresholds {
    pub cache_unused_days:            u64,
    pub uninstall_unused_days:        u64,
    pub snapshot_max_age_days:        u64,
    pub snapshot_keep_count:          u64,
    pub large_file_min_mb:            u64,
    pub log_max_age_days:             u64,
    pub tmp_max_age_hours:            u64,
    pub timemachine_incomplete_safe_hours: u64,
}

// ── paths ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Paths {
    pub system_cache:  Vec<String>,
    pub user_cache:    Vec<String>,
    pub crash_logs:    Vec<String>,
    pub tmp_dirs:      Vec<String>,
    pub app_containers: Vec<String>,
    pub app_support:   Vec<String>,
    pub app_prefs:     Vec<String>,
    pub system_logs:   Vec<String>,
}

// ── categories ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Categories {
    pub system:            SystemCategory,
    pub dev_caches:        DevCacheCategory,
    pub project_artifacts: ProjectArtifactsCategory,
    pub logs:              LogsCategory,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemCategory {
    pub enabled: bool,
    pub targets: Vec<SystemTarget>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemTarget {
    pub name:        String,
    #[serde(default)] pub path:     Option<String>,
    #[serde(default)] pub pattern:  Option<String>,
    #[serde(default)] pub ext:      Option<Vec<String>>,
    #[serde(default)] pub recursive: Option<bool>,
    #[serde(default)] pub max_age_days: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DevCacheCategory {
    pub enabled: bool,
    pub entries: Vec<DevCacheEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DevCacheEntry {
    pub name:  String,
    pub path:  String,
    /// "safe" | "caution" | "risky"
    pub risk:  String,
    pub group: Option<String>,
}

impl DevCacheEntry {
    pub fn risk_emoji(&self) -> &'static str {
        match self.risk.as_str() {
            "safe"    => "🟢",
            "caution" => "🟡",
            "risky"   => "🔴",
            _         => "⚪",
        }
    }
    pub fn is_risky(&self) -> bool { self.risk == "risky" }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectArtifactsCategory {
    pub enabled:    bool,
    pub min_size_mb: u64,
    pub scan_roots: Vec<String>,
    pub types:      Vec<ProjectType>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectType {
    pub marker:   String,
    pub artifact: String,
    pub label:    String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogsCategory {
    pub enabled:     bool,
    pub max_age_days: u64,
}

// ── snapshots ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Snapshots {
    pub enabled:                   bool,
    pub auto_delete:               bool,
    pub max_age_days:              u64,
    pub keep_count:                u64,
    pub skip_if_tm_running:        bool,
    pub delete_incomplete_backups: bool,
    pub incomplete_safe_hours:     u64,
}

// ── silence ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Silence {
    pub enabled:                    bool,
    pub block_notifications:        bool,
    pub block_background_agents:    bool,
    pub force_quit_on_window_close: bool,
    pub notification_mode:          String,
    pub per_app_overrides:          HashMap<String, AppOverride>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppOverride {
    pub allow_notifications: bool,
    pub allow_background:    bool,
}

// ── optimizer ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Optimizer {
    pub enabled:                  bool,
    pub flush_dns:                bool,
    pub quicklook_refresh:        bool,
    pub launch_services_rebuild:  bool,
    pub sqlite_vacuum:            bool,
    pub quarantine_cleanup:       bool,
    pub saved_state_cleanup:      bool,
    pub saved_state_max_age_days: u64,
    pub broken_launch_agents:     bool,
    pub notification_center_cleanup: bool,
    pub coreduet_cleanup:         bool,
    pub font_cache_rebuild:       bool,
    pub dock_refresh:             bool,
    pub prevent_network_dsstore:  bool,
    pub purge_inactive_memory:    bool,
    pub disable_reopen_windows:   bool,
    pub disable_sudden_motion_sensor: bool,
    pub increase_fd_limit:        bool,
    pub periodic_maintenance:     bool,
}

// ── process killer ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProcessKiller {
    pub enabled:               bool,
    pub graceful_timeout_secs: u64,
    pub force_timeout_secs:    u64,
    pub use_launchctl_bootout: bool,
}

// ── lists ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Whitelist {
    pub apps:       Vec<String>,
    pub bundle_ids: Vec<String>,
    pub paths:      Vec<String>,
}

impl Whitelist {
    pub fn contains_bundle(&self, bid: &str) -> bool {
        self.bundle_ids.iter().any(|b| b == bid)
    }
    pub fn contains_app(&self, name: &str) -> bool {
        self.apps.iter().any(|a| a == name)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RogueList {
    pub bundle_ids:    Vec<String>,
    pub app_names:     Vec<String>,
    pub process_names: Vec<String>,
}

// ── Config impl ───────────────────────────────────────────────────────────────

impl Config {
    /// Load, migrate, and validate config.json.
    ///
    /// Migration strategy:
    ///   1. Parse into a `serde_json::Value` (tolerant — unknown fields ignored)
    ///   2. Run migration passes to bring schema up to current version
    ///   3. Deserialize into typed Config
    ///   4. Validate required invariants (thresholds > 0, keep_count >= 1, etc.)
    ///
    /// On parse failure: returns Err with the line/column from serde_json so
    /// the user can fix their config. Never silently falls back to defaults on
    /// a malformed file (that would hide mistakes).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let clean = strip_json_comments(&raw);

        // Step 1: tolerant parse into Value
        let mut value: serde_json::Value = serde_json::from_str(&clean)
            .map_err(|e| anyhow::anyhow!(
                "config.json parse error at line {}, col {}: {}",
                e.line(), e.column(), e
            ))?;

        // Step 2: migrate
        let file_version = value.get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        if file_version < SCHEMA_VERSION {
            migrate(&mut value, file_version);
        } else if file_version > SCHEMA_VERSION {
            log::warn!(
                "config schema_version {} is newer than this binary supports ({}). Unknown fields will be ignored.",
                file_version, SCHEMA_VERSION
            );
        }

        // Step 3: typed deserialize
        let mut cfg: Config = serde_json::from_value(value)
            .map_err(|e| anyhow::anyhow!("config type error: {e}"))?;

        // Step 4: validate + clamp
        cfg.validate()?;

        Ok(cfg)
    }

    fn validate(&mut self) -> anyhow::Result<()> {
        // Prevent division-by-zero and nonsensical schedules
        if self.thresholds.cache_unused_days == 0 {
            anyhow::bail!("thresholds.cache_unused_days must be > 0");
        }
        if self.thresholds.uninstall_unused_days < self.thresholds.cache_unused_days {
            anyhow::bail!(
                "thresholds.uninstall_unused_days ({}) must be >= cache_unused_days ({})",
                self.thresholds.uninstall_unused_days,
                self.thresholds.cache_unused_days,
            );
        }
        if self.thresholds.snapshot_keep_count == 0 {
            log::warn!("snapshot_keep_count = 0 — clamped to 1 (always keep at least one)");
            self.thresholds.snapshot_keep_count = 1;
        }
        if self.schedule.cache_sweep_interval_hours == 0 {
            anyhow::bail!("schedule.cache_sweep_interval_hours must be > 0");
        }
        Ok(())
    }

    /// Expand `~/` to $HOME.
    pub fn expand(s: &str) -> PathBuf {
        if let Some(rest) = s.strip_prefix("~/") {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest)
        } else {
            PathBuf::from(s)
        }
    }

    /// Spawn a watcher thread; returns a channel that delivers reloaded Configs.
    pub fn watch(path: PathBuf) -> Receiver<Config> {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("hush-cfg-watch".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                #[cfg(target_os = "macos")]
                kqueue_watch(path, tx);
                #[cfg(not(target_os = "macos"))]
                poll_watch(path, tx);
            })
            .expect("spawn cfg-watcher");
        rx
    }
}

// ── kqueue watcher (macOS) ────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn kqueue_watch(path: PathBuf, tx: mpsc::Sender<Config>) {
    use libc::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();

    // SAFETY: All libc kqueue/kevent calls follow the documented C API contract:
    //   • kqueue() returns a new kernel event queue fd; closed on exit.
    //   • open() with O_EVTONLY|O_CLOEXEC takes ownership of the fd.
    //   • EV_SET initialises a stack-allocated kevent — zeroed first.
    //   • kevent() is called with a correctly-sized changelist/eventlist pair.
    //   • All fds are closed before return, including the early-exit paths.
    //   No Rust references alias the raw fds at any point.
    unsafe {
        let kq = kqueue();
        if kq < 0 { return; }

        let open_fd = || open(cpath.as_ptr(), O_EVTONLY | O_CLOEXEC);
        let mut fd = open_fd();
        if fd < 0 { close(kq); return; }

        let mut changelist: [kevent; 1] = std::mem::zeroed();
        let timeout = timespec { tv_sec: 120, tv_nsec: 0 };
        let mut eventlist: [kevent; 1] = std::mem::zeroed();

        loop {
            EV_SET(
                &mut changelist[0],
                fd as usize,
                EVFILT_VNODE,
                (EV_ADD | EV_ENABLE | EV_CLEAR) as u16,
                (NOTE_WRITE | NOTE_RENAME | NOTE_DELETE) as u32,
                0,
                std::ptr::null_mut(),
            );

            let n = kevent(kq, changelist.as_ptr(), 1,
                           eventlist.as_mut_ptr(), 1, &timeout);

            if n > 0 {
                // Wait for editor to finish the atomic write
                thread::sleep(Duration::from_millis(80));

                match Config::load(&path) {
                    Ok(cfg) => { let _ = tx.send(cfg); }
                    Err(e)  => warn!("hot-reload parse error: {e}"),
                }

                // If file was replaced (atomic editor save), reopen fd
                if eventlist[0].fflags & (NOTE_DELETE | NOTE_RENAME) != 0 {
                    close(fd);
                    fd = open_fd();
                    if fd < 0 { break; }
                }
            }
        }

        close(fd);
        close(kq);
    }
}

#[cfg(not(target_os = "macos"))]
fn poll_watch(path: PathBuf, tx: mpsc::Sender<Config>) {
    use std::time::SystemTime;
    let mut last: Option<SystemTime> = None;
    loop {
        thread::sleep(Duration::from_secs(5));
        if let Ok(m) = std::fs::metadata(&path) {
            let mtime = m.modified().ok();
            if mtime != last {
                last = mtime;
                if let Ok(c) = Config::load(&path) { let _ = tx.send(c); }
            }
        }
    }
}


// ── schema migration ──────────────────────────────────────────────────────────

/// Bring a parsed JSON Value from `from_version` up to SCHEMA_VERSION.
/// Each migration arm adds missing fields with safe defaults.
/// Migrations are additive-only — no fields are removed (forward-compat).
fn migrate(value: &mut serde_json::Value, from_version: u32) {
    use serde_json::json;

    log::info!("config: migrating schema v{from_version} → v{SCHEMA_VERSION}");

    // v0 → v1: snapshot_audit_interval_days added to schedule;
    //           process_killer block added;
    //           optimizer.coreduet_cleanup added
    if from_version < 1 {
        // Add snapshot_audit_interval_days if absent
        if let Some(sched) = value.get_mut("schedule").and_then(|v| v.as_object_mut()) {
            sched.entry("snapshot_audit_interval_days").or_insert(json!(3));
        }

        // Add process_killer block if absent
        value.as_object_mut().map(|o| {
            o.entry("process_killer").or_insert(json!({
                "enabled": true,
                "graceful_timeout_secs": 3,
                "force_timeout_secs": 5,
                "use_launchctl_bootout": true
            }));
        });

        // Add optimizer.coreduet_cleanup if absent
        if let Some(opt) = value.get_mut("optimizer").and_then(|v| v.as_object_mut()) {
            opt.entry("coreduet_cleanup").or_insert(json!(true));
            opt.entry("saved_state_max_age_days").or_insert(json!(30));
            opt.entry("notification_center_cleanup").or_insert(json!(true));
            opt.entry("broken_launch_agents").or_insert(json!(true));
            opt.entry("periodic_maintenance").or_insert(json!(true));
            opt.entry("increase_fd_limit").or_insert(json!(true));
        }

        // Add rogue_list.process_names if absent
        if let Some(rl) = value.get_mut("rogue_list").and_then(|v| v.as_object_mut()) {
            rl.entry("process_names").or_insert(json!([]));
        }

        log::info!("config: migration v0→v1 applied");
    }

    if let Some(obj) = value.as_object_mut() {
        obj.insert("schema_version".into(), json!(SCHEMA_VERSION));
    }
}

// ── comment stripping ─────────────────────────────────────────────────────────

fn strip_json_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            out.push('\n');
            continue;
        }
        // Strip inline trailing // comments (outside of strings)
        if let Some(pos) = inline_comment_pos(line) {
            out.push_str(&line[..pos]);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn inline_comment_pos(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let mut in_str = false;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'"'                                  => in_str = !in_str,
            b'\\' if in_str                       => i += 1,
            b'/' if !in_str && i+1 < b.len() && b[i+1] == b'/' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}
