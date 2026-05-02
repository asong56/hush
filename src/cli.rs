use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name    = "hush",
    about   = "Hush — silent, instant macOS cleaner",
    version,
    long_about = "\
Hush keeps your Mac lean and quiet with zero friction.
One command cleans everything; the daemon handles the rest silently.\n
Examples:
  hush clean             # full clean (safe defaults)
  hush clean -n          # dry-run — shows what would be freed
  hush snapshot          # delete stale APFS snapshots
  hush crush             # kill all rogue background processes
  hush optimize          # apply all system tweaks
  hush status            # show daemon state, config path, log size
  hush install           # register LaunchAgent (runs on login)"
)]
pub struct Cli {
    /// Path to config.json
    /// Default: ~/Library/Application Support/hush/config.json
    #[arg(short, long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Clean junk, caches, logs, snapshots (default: all safe passes)
    #[command(alias = "c")]
    Clean(CleanOpts),

    /// Silence rogue notifications and background agents
    #[command(alias = "s")]
    Silence(SilenceOpts),

    /// Apply system optimizations (DNS, UI, SQLite, LaunchServices…)
    #[command(alias = "o")]
    Optimize(OptimizeOpts),

    /// Manage APFS local snapshots
    #[command(alias = "snap")]
    Snapshot(SnapshotOpts),

    /// Kill rogue process(es) by name or rogue_list config
    Crush(CrushOpts),

    /// Audit: show app usage, snapshot list, large caches (read-only)
    #[command(alias = "a")]
    Audit,

    /// Show daemon status, config path, log file size
    #[command(alias = "st")]
    Status,

    /// Run as a background daemon (hot-reloads config)
    Daemon(DaemonOpts),

    /// Install LaunchAgent — auto-run on login
    Install,

    /// Uninstall LaunchAgent
    Uninstall,
}

// ── clean ────────────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct CleanOpts {
    /// Run all passes (default when no flag is specified)
    #[arg(short, long, default_value_t = true)]
    pub all: bool,

    /// Clean system junk: .DS_Store, AppleDouble, crash logs, tmp
    #[arg(long, overrides_with = "all")]
    pub system: bool,

    /// Clean developer caches (Xcode, npm, cargo, brew, …)
    #[arg(long, overrides_with = "all")]
    pub cache: bool,

    /// Clean stale project build artifacts (node_modules, target/, .build, …)
    #[arg(long, overrides_with = "all")]
    pub projects: bool,

    /// Run app lifecycle checks and cache sweeps
    #[arg(long, overrides_with = "all")]
    pub apps: bool,

    /// Delete stale APFS local snapshots
    #[arg(long, overrides_with = "all")]
    pub snapshots: bool,

    /// Also uninstall apps unused > threshold (requires --apps)
    #[arg(long)]
    pub uninstall: bool,

    /// Dry-run: print what would be removed, change nothing
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

// ── silence ───────────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct SilenceOpts {
    /// Apply all silence rules (default)
    #[arg(short, long, default_value_t = true)]
    pub all: bool,

    /// Restrict non-whitelisted notifications to banners-only
    #[arg(long, overrides_with = "all")]
    pub notifications: bool,

    /// Disable non-essential LaunchAgents
    #[arg(long, overrides_with = "all")]
    pub background: bool,

    /// Force apps to quit when their last window closes
    #[arg(long, overrides_with = "all")]
    pub dock: bool,
}

// ── optimize ─────────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct OptimizeOpts {
    /// Apply all optimizations (default)
    #[arg(short, long, default_value_t = true)]
    pub all: bool,

    /// Flush DNS cache and reset ARP/routing tables
    #[arg(long, overrides_with = "all")]
    pub network: bool,

    /// System tweaks: LaunchServices, SQLite vacuum, Quarantine DB, …
    #[arg(long, overrides_with = "all")]
    pub system: bool,

    /// UI tweaks: Dock, QuickLook, font cache, DS_Store prevention
    #[arg(long, overrides_with = "all")]
    pub ui: bool,
}

// ── snapshot ─────────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct SnapshotOpts {
    /// List all local snapshots without deleting
    #[arg(short, long)]
    pub list: bool,

    /// Dry-run: show what would be deleted
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

// ── crush ────────────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct CrushOpts {
    /// Target a specific process by name (otherwise uses rogue_list)
    #[arg(value_name = "PROCESS")]
    pub name: Option<String>,
}

// ── daemon ───────────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct DaemonOpts {
    /// Stay in foreground (don't daemonize)
    #[arg(long)]
    pub foreground: bool,
}
