// hush — silent, instant macOS cleaner
// Binary: hush <subcommand> [flags]

mod cli;
mod config;
mod daemon;
mod error;
mod guard;

pub mod cleaner {
    pub mod apps;
    pub mod cache;
    pub mod ops;
    pub mod snapshots;
    pub mod system;
}

pub mod optimizer {
    pub mod network;
    pub mod system;
    pub mod ui;
}

pub mod sentinel {
    pub mod notifications;
    pub mod processes;
    pub mod silence;
}

// ── test modules (compiled only in test builds) ───────────────────────────────
#[cfg(test)] mod guard_tests;
#[cfg(test)] mod config_tests;
#[cfg(test)] mod error_tests;

use std::process;
use clap::Parser;
use log::{error, info};

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn"),
    )
    .format_timestamp(None)
    .format_target(false)
    .init();

    let args = cli::Cli::parse();
    let cfg_path = args.config.clone().unwrap_or_else(default_cfg_path);

    let cfg = match config::Config::load(&cfg_path) {
        Ok(c)  => c,
        Err(e) => { error!("{e}"); process::exit(1); }
    };

    let result = match args.command {
        cli::Command::Clean(o)    => cmd_clean(&cfg, o),
        cli::Command::Silence(o)  => cmd_silence(&cfg, o),
        cli::Command::Optimize(o) => cmd_optimize(&cfg, o),
        cli::Command::Snapshot(o) => cmd_snapshot(&cfg, o),
        cli::Command::Crush(o)    => cmd_crush(&cfg, o),
        cli::Command::Audit       => cmd_audit(&cfg),
        cli::Command::Status      => cmd_status(&cfg_path),
        cli::Command::Daemon(o)   => daemon::run(&cfg_path, o),
        cli::Command::Install     => daemon::install(&cfg_path),
        cli::Command::Uninstall   => daemon::uninstall(),
    };

    if let Err(e) = result {
        error!("{e}");
        process::exit(1);
    }
}

fn cmd_clean(cfg: &config::Config, o: cli::CleanOpts) -> anyhow::Result<()> {
    let mut freed: u64 = 0;
    if o.all || o.system {
        freed += cleaner::system::clean_ds_store(cfg, o.dry_run)?;
        freed += cleaner::system::clean_apple_double(cfg, o.dry_run)?;
        freed += cleaner::system::clean_crash_logs(cfg, o.dry_run)?;
        freed += cleaner::system::clean_tmp(cfg, o.dry_run)?;
        freed += cleaner::system::clean_system_logs(cfg, o.dry_run)?;
    }
    if o.all || o.cache {
        freed += cleaner::cache::sweep_dev_caches(cfg, o.dry_run)?;
    }
    if o.all || o.projects {
        freed += cleaner::cache::sweep_project_artifacts(cfg, o.dry_run)?;
    }
    if o.all || o.apps {
        cleaner::apps::remove_rogue(cfg, o.dry_run)?;
        freed += cleaner::apps::clean_unused_cache(cfg, o.dry_run)?;
        if o.uninstall {
            cleaner::apps::uninstall_stale(cfg, o.dry_run)?;
        }
    }
    if o.all || o.snapshots {
        freed += cleaner::snapshots::delete_stale(cfg, o.dry_run)?;
    }
    let prefix = if o.dry_run { "[dry-run] would free" } else { "freed" };
    println!("✓  {prefix} {}", fmt_bytes(freed));
    Ok(())
}

fn cmd_silence(cfg: &config::Config, o: cli::SilenceOpts) -> anyhow::Result<()> {
    if o.all || o.notifications { sentinel::notifications::restrict(cfg)?; }
    if o.all || o.background    { sentinel::silence::block_agents(cfg)?; }
    if o.all || o.dock          { sentinel::silence::force_quit_on_close(cfg)?; }
    println!("✓  silence applied");
    Ok(())
}

fn cmd_optimize(cfg: &config::Config, o: cli::OptimizeOpts) -> anyhow::Result<()> {
    if o.all || o.network {
        optimizer::network::flush_dns(cfg)?;
        optimizer::network::reset_stack(cfg)?;
    }
    if o.all || o.system { optimizer::system::apply(cfg)?; }
    if o.all || o.ui     { optimizer::ui::apply(cfg)?; }
    println!("✓  optimizations applied");
    Ok(())
}

fn cmd_snapshot(cfg: &config::Config, o: cli::SnapshotOpts) -> anyhow::Result<()> {
    if o.list { return cleaner::snapshots::list(); }
    let freed = cleaner::snapshots::delete_stale(cfg, o.dry_run)?;
    let prefix = if o.dry_run { "[dry-run] would free" } else { "freed" };
    println!("✓  snapshots — {prefix} {}", fmt_bytes(freed));
    Ok(())
}

fn cmd_crush(cfg: &config::Config, o: cli::CrushOpts) -> anyhow::Result<()> {
    let killed = sentinel::processes::crush_rogue(cfg, &o.name)?;
    if killed == 0 { println!("✓  no rogue processes found"); }
    else           { println!("✓  crushed {killed} process(es)"); }
    Ok(())
}

fn cmd_status(cfg_path: &std::path::Path) -> anyhow::Result<()> {
    use std::process::Command;

    let label = "com.hush.daemon";
    let running = Command::new("launchctl")
        .args(["list", label])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let status_str = if running { "running ✓" } else { "stopped" };
    println!("daemon       : {status_str}");

    println!("config       : {}", cfg_path.display());

    let home = std::env::var("HOME").unwrap_or_default();
    let log_path = std::path::PathBuf::from(&home).join("Library/Logs/hush.log");
    if log_path.exists() {
        let size = log_path.metadata().map(|m| m.len()).unwrap_or(0);
        println!("log          : {} ({})", log_path.display(), fmt_bytes(size));
    } else {
        println!("log          : not yet created");
    }

    let plist = std::path::PathBuf::from(&home)
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    println!("launchagent  : {}", if plist.exists() { "installed" } else { "not installed" });

    Ok(())
}

fn cmd_audit(cfg: &config::Config) -> anyhow::Result<()> {
    cleaner::apps::audit_report(cfg)?;
    cleaner::snapshots::list()?;
    Ok(())
}

pub fn fmt_bytes(b: u64) -> String {
    if b >= 1_073_741_824      { format!("{:.1} GB", b as f64 / 1_073_741_824.0) }
    else if b >= 1_048_576     { format!("{:.0} MB", b as f64 / 1_048_576.0) }
    else if b >= 1024          { format!("{:.0} KB", b as f64 / 1024.0) }
    else                       { format!("{b} B") }
}

fn default_cfg_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home)
        .join("Library/Application Support/hush/config.json")
}
