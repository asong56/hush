// daemon.rs — Hush background daemon
//
// Design goals:
//   • RSS < 512 KB at idle
//   • No tokio / async runtime — pure std::thread + kqueue
//   • Config hot-reload via kqueue EVFILT_VNODE (64 KB watcher thread)
//   • SIGTERM / SIGINT → clean shutdown via AtomicBool
//   • Timed passes via elapsed-since + sleep(60s) — no timer threads
//
// Daemon lifecycle:
//   1. Load config
//   2. Spawn kqueue config-watcher thread
//   3. Run boot_clean pass (if configured)
//   4. Loop:
//        try_recv() → hot-reload
//        check elapsed timers → run clean/sentinel passes
//        sleep(60s) — yields CPU entirely

use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use log::{error, info, warn};
use crate::{cli::DaemonOpts, config::Config};

// ── signal flag ───────────────────────────────────────────────────────────────

static RUNNING: AtomicBool = AtomicBool::new(true);

// ── public entry points ───────────────────────────────────────────────────────

pub fn run(cfg_path: &PathBuf, _opts: DaemonOpts) -> anyhow::Result<()> {
    setup_signals();

    let mut cfg = Config::load(cfg_path)?;
    let cfg_rx  = Config::watch(cfg_path.clone());

    info!(
        "hush daemon started  pid={} uid={}",
        std::process::id(),
        // SAFETY: getuid() has no preconditions; always safe to call.
        unsafe { libc::getuid() }
    );

    if cfg.schedule.boot_clean {
        run_boot_clean(&cfg);
    }

    // Timers — track last-run instants
    let mut last_cache    = Instant::now()
        .checked_sub(Duration::from_secs(cfg.schedule.cache_sweep_interval_hours * 3600))
        .unwrap_or_else(Instant::now); // trigger immediately on first loop

    let mut last_app      = Instant::now()
        .checked_sub(Duration::from_secs(cfg.schedule.app_audit_interval_days * 86400))
        .unwrap_or_else(Instant::now);

    let mut last_snapshot = Instant::now()
        .checked_sub(Duration::from_secs(cfg.schedule.snapshot_audit_interval_days * 86400))
        .unwrap_or_else(Instant::now);

    // ── main event loop ───────────────────────────────────────────────────────
    while RUNNING.load(Ordering::Relaxed) {

        // ① Hot-reload check (non-blocking)
        // On a parse error the daemon continues with the last valid config
        // rather than exiting — a malformed mid-edit file should not kill
        // a long-running background process.
        match cfg_rx.try_recv() {
            Ok(new_cfg) => {
                info!("daemon: config reloaded");
                cfg = new_cfg;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                warn!("daemon: config watcher thread exited — hot-reload disabled");
            }
        }

        let now = Instant::now();

        // ② Scheduled cache sweep
        let cache_interval = Duration::from_secs(
            cfg.schedule.cache_sweep_interval_hours * 3600
        );
        if now.duration_since(last_cache) >= cache_interval {
            run_cache_pass(&cfg);
            last_cache = Instant::now();
        }

        // ③ Scheduled app audit
        let app_interval = Duration::from_secs(
            cfg.schedule.app_audit_interval_days * 86400
        );
        if now.duration_since(last_app) >= app_interval {
            run_app_pass(&cfg);
            last_app = Instant::now();
        }

        // ④ Scheduled snapshot audit
        let snap_interval = Duration::from_secs(
            cfg.schedule.snapshot_audit_interval_days * 86400
        );
        if now.duration_since(last_snapshot) >= snap_interval {
            run_snapshot_pass(&cfg);
            last_snapshot = Instant::now();
        }

        // Sleep 60 s — zero CPU while idle
        // We check RUNNING in short 1-second increments so SIGTERM is
        // handled within 1 second, not after 60.
        for _ in 0..60 {
            if !RUNNING.load(Ordering::Relaxed) { break; }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    info!("hush daemon stopped");
    Ok(())
}

// ── passes called from the loop ───────────────────────────────────────────────

fn run_boot_clean(cfg: &Config) {
    info!("daemon: boot clean started");
    quietly(crate::cleaner::system::clean_ds_store(cfg, false));
    quietly(crate::cleaner::system::clean_apple_double(cfg, false));
    quietly(crate::cleaner::system::clean_crash_logs(cfg, false));
    quietly(crate::cleaner::system::clean_tmp(cfg, false));
    quietly(crate::cleaner::system::clean_system_logs(cfg, false));
    quietly(crate::sentinel::silence::block_agents(cfg));
    quietly(crate::sentinel::silence::force_quit_on_close(cfg));
    quietly(crate::sentinel::notifications::restrict(cfg));
    quietly(crate::optimizer::system::apply(cfg));
    quietly(crate::optimizer::ui::apply(cfg));
    quietly(crate::optimizer::network::flush_dns(cfg));
    info!("daemon: boot clean done");
}

fn run_cache_pass(cfg: &Config) {
    info!("daemon: cache sweep");
    quietly(crate::cleaner::cache::sweep_dev_caches(cfg, false));
    quietly(crate::cleaner::apps::clean_unused_cache(cfg, false));
}

fn run_app_pass(cfg: &Config) {
    info!("daemon: app audit");
    quietly(crate::cleaner::apps::remove_rogue(cfg, false));
    quietly(crate::sentinel::processes::crush_rogue(cfg, &None).map(|_| ()));
}

fn run_snapshot_pass(cfg: &Config) {
    info!("daemon: snapshot audit");
    quietly(crate::cleaner::snapshots::delete_stale(cfg, false).map(|_| ()));
}

fn quietly<T>(r: anyhow::Result<T>) {
    if let Err(e) = r { warn!("daemon: pass error: {e}"); }
}

// ── LaunchAgent install ───────────────────────────────────────────────────────

const LABEL: &str = "com.hush.daemon";

pub fn install(cfg_path: &PathBuf) -> anyhow::Result<()> {
    let home = std::env::var("HOME")?;
    let bin  = std::env::current_exe()?;

    let la_dir    = PathBuf::from(&home).join("Library/LaunchAgents");
    let plist_path = la_dir.join(format!("{LABEL}.plist"));

    std::fs::create_dir_all(&la_dir)?;

    let plist = format!(
r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>

    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>--config</string>
        <string>{cfg}</string>
        <string>daemon</string>
        <string>--foreground</string>
    </array>

    <!-- Start immediately and stay alive -->
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>

    <!-- Throttle rapid respawn (e.g. config error) -->
    <key>ThrottleInterval</key>
    <integer>30</integer>

    <!-- Low I/O priority — never competes with user work -->
    <key>LowPriorityIO</key>
    <true/>
    <key>Nice</key>
    <integer>5</integer>

    <key>StandardOutPath</key>
    <string>{home}/Library/Logs/hush.log</string>
    <key>StandardErrorPath</key>
    <string>{home}/Library/Logs/hush.log</string>

    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>warn</string>
    </dict>
</dict>
</plist>"#,
        bin  = bin.display(),
        cfg  = cfg_path.display(),
        home = home,
    );

    std::fs::write(&plist_path, &plist)?;

    // Install a newsyslog config so the log file is rotated weekly,
    // kept for 4 weeks, and compressed — prevents unbounded log growth.
    let newsyslog_dir  = PathBuf::from("/etc/newsyslog.d");
    let newsyslog_conf = newsyslog_dir.join("hush.conf");
    if newsyslog_dir.exists() {
        let conf = format!(
            "# hush log rotation — managed by `hush install`\n\
             {home}/Library/Logs/hush.log  644  4  *  $W0D0  JN\n",
            home = home,
        );
        // Best-effort: skip silently if /etc/newsyslog.d is not writable
        let _ = std::fs::write(&newsyslog_conf, conf);
    }

    let _ = std::process::Command::new("launchctl")
        .args(["load", "-w", plist_path.to_str().unwrap()])
        .output();

    println!("✓  LaunchAgent installed");
    println!("   {}", plist_path.display());
    println!("   daemon will auto-start on next login");
    println!("   to start now: launchctl start {LABEL}");
    println!("   logs: ~/Library/Logs/hush.log");

    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    let home = std::env::var("HOME")?;
    let plist_path = PathBuf::from(&home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"));

    let _ = std::process::Command::new("launchctl")
        .args(["unload", "-w", plist_path.to_str().unwrap()])
        .output();

    if plist_path.exists() {
        std::fs::remove_file(&plist_path)?;
        println!("✓  LaunchAgent removed");
    } else {
        println!("  nothing to uninstall (LaunchAgent not found)");
    }

    let newsyslog_conf = PathBuf::from("/etc/newsyslog.d/hush.conf");
    if newsyslog_conf.exists() {
        let _ = std::fs::remove_file(&newsyslog_conf);
    }

    Ok(())
}

// ── signal handling ───────────────────────────────────────────────────────────

fn setup_signals() {
    #[cfg(unix)]
    // SAFETY: signal() is called before any threads are spawned that could
    // observe the signal disposition. handle_signal() only writes to an
    // AtomicBool, which is signal-safe. SIG_IGN is a valid disposition value.
    unsafe {
        libc::signal(libc::SIGTERM, handle_signal as libc::sighandler_t);
        libc::signal(libc::SIGINT,  handle_signal as libc::sighandler_t);
        // Ignore SIGHUP — config reload is done via kqueue, not signals
        libc::signal(libc::SIGHUP,  libc::SIG_IGN);
    }
}

#[cfg(unix)]
extern "C" fn handle_signal(_: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}
