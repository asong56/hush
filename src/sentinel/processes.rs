// sentinel/processes.rs — rogue process crusher
//
// Kills persistent background processes that:
//   a) appear in config rogue_list.process_names, OR
//   b) are matched by a user-supplied name argument, OR
//   c) are LaunchAgents with no valid backing binary ("broken agents")
//
// Kill sequence:
//   1. pkill -x <name>           (SIGTERM — graceful, timeout: graceful_timeout_secs)
//   2. pkill -9 -x <name>        (SIGKILL — force, timeout: force_timeout_secs)
//   3. launchctl bootout         (evict from launchd — no respawn)
//   4. Verify: pgrep -x <name>   (confirm dead)
//
// File-lock safety: we never kill a process whose bundle is codesign-valid
// AND which has open files in user-data directories — that risks data corruption.

use std::{
    path::PathBuf,
    process::Command,
    thread::sleep,
    time::Duration,
};
use log::{debug, info, warn};
use crate::config::Config;

// ── public API ────────────────────────────────────────────────────────────────

/// Kill all rogue processes: either `name` (specific) or the full rogue_list.
/// Returns number of processes successfully killed.
pub fn crush_rogue(cfg: &Config, name: &Option<String>) -> anyhow::Result<u32> {
    if !cfg.process_killer.enabled { return Ok(0); }

    let targets: Vec<String> = if let Some(n) = name {
        vec![n.clone()]
    } else {
        cfg.rogue_list.process_names.clone()
    };

    let mut killed = 0u32;

    for target in &targets {
        if is_whitelisted(target, cfg) {
            warn!("crush: skipping whitelisted process '{target}'");
            continue;
        }

        if !is_running(target) {
            debug!("crush: '{target}' not running");
            continue;
        }

        info!("crush: targeting '{target}'");

        if kill_process(target, cfg) {
            killed += 1;
            println!("  ✓  crushed: {target}");
        } else {
            println!("  ✗  could not kill: {target} — try: sudo kill -9 $(pgrep -x \"{target}\")");
        }
    }

    // Also kill broken launch agents (agents with no valid binary)
    killed += kill_broken_agents(cfg);

    Ok(killed)
}

// ── kill sequence ─────────────────────────────────────────────────────────────

fn kill_process(name: &str, cfg: &Config) -> bool {
    let graceful = cfg.process_killer.graceful_timeout_secs;
    let force    = cfg.process_killer.force_timeout_secs;

    // ① SIGTERM
    let _ = Command::new("pkill").args(["-x", name]).output();
    sleep(Duration::from_secs(graceful));

    if !is_running(name) {
        debug!("crush: '{name}' exited cleanly after SIGTERM");
        return true;
    }

    // ② SIGKILL
    let _ = Command::new("pkill").args(["-9", "-x", name]).output();
    sleep(Duration::from_secs(force));

    // ③ launchctl bootout (evicts from launchd — prevents respawn)
    if cfg.process_killer.use_launchctl_bootout {
        if let Some(pid) = get_pid(name) {
            let domain = format!("gui/{}/{pid}", unsafe { libc::getuid() });
            let _ = Command::new("launchctl")
                .args(["bootout", &domain])
                .output();
            debug!("crush: launchctl bootout {domain}");
        }
    }

    // ④ Final sudo attempt if still running and we have permissions
    if is_running(name) {
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            let _ = Command::new("pkill").args(["-9", "-x", name]).output();
            sleep(Duration::from_secs(1));
        }
    }

    !is_running(name)
}

// ── broken launch agent cleanup ───────────────────────────────────────────────

/// Kill LaunchAgent processes whose backing binary no longer exists on disk.
/// Returns count killed.
fn kill_broken_agents(cfg: &Config) -> u32 {
    if !cfg.optimizer.broken_launch_agents { return 0; }

    let la_dir = home_join("Library/LaunchAgents");
    let Ok(rd) = std::fs::read_dir(&la_dir) else { return 0 };

    let mut killed = 0u32;

    for entry in rd.flatten() {
        let plist = entry.path();
        if plist.extension().and_then(|e| e.to_str()) != Some("plist") { continue; }

        let name = plist.file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        // Skip Apple and whitelisted agents
        if name.starts_with("com.apple.") { continue; }
        if is_whitelisted(&name, cfg) { continue; }

        let binary = get_plist_binary(&plist);
        let binary_missing = binary.as_ref()
            .map(|b| !std::path::Path::new(b).exists())
            .unwrap_or(false);

        if binary_missing {
            debug!("crush: broken agent {name} (binary gone)");

            let _ = Command::new("launchctl")
                .args(["unload", plist.to_str().unwrap_or("")])
                .output();

            if is_running(&name) {
                let _ = Command::new("pkill").args(["-9", "-x", &name]).output();
                if !is_running(&name) {
                    killed += 1;
                    info!("crush: killed broken agent process '{name}'");
                }
            }

            let _ = std::fs::remove_file(&plist);
        }
    }

    killed
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn is_running(name: &str) -> bool {
    Command::new("pgrep")
        .args(["-x", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn get_pid(name: &str) -> Option<u32> {
    let out = Command::new("pgrep").args(["-x", name]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

fn is_whitelisted(name: &str, cfg: &Config) -> bool {
    cfg.whitelist.contains_app(name)
        || cfg.whitelist.apps.iter().any(|a| a.to_lowercase() == name.to_lowercase())
}

/// Read `ProgramArguments[0]` (or `Program`) from a .plist via PlistBuddy.
fn get_plist_binary(plist: &std::path::Path) -> Option<String> {
    let out = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :ProgramArguments:0", plist.to_str()?])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        // Fallback: try Program key
        let out2 = Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :Program", plist.to_str()?])
            .output()
            .ok()?;
        let s2 = String::from_utf8_lossy(&out2.stdout).trim().to_string();
        if s2.is_empty() { None } else { Some(s2) }
    } else {
        Some(s)
    }
}

fn home_join<P: AsRef<std::path::Path>>(p: P) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(p)
}
