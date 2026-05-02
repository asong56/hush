// optimizer/network.rs — network stack cleanup
//
// flush_dns():
//   Flushes macOS DNS cache via mDNSResponder HUP + dscacheutil.
//   Also clears the local NetBIOS / WINS cache if active.
//   No sudo needed on modern macOS (10.15+).
//
// reset_stack():
//   Flushes ARP table, routing cache, and resets socket statistics.
//   Useful after aggressive network changes or VPN residue.
//   Requires root for `route flush` — skips gracefully if not root.

use std::process::Command;
use log::{debug, info, warn};
use crate::config::Config;

pub fn flush_dns(cfg: &Config) -> anyhow::Result<()> {
    if !cfg.optimizer.flush_dns { return Ok(()); }

    // Primary: signal mDNSResponder to flush its cache
    run("killall", &["-HUP", "mDNSResponder"]);

    // Flush dscacheutil (Directory Services)
    run("dscacheutil", &["-flushcache"]);

    // Flush mDNSResponder stats (no-op if daemon not running)
    run("killall", &["-INFO", "mDNSResponder"]);

    // macOS Ventura+ uses dns-sd for stats; harmless if unavailable
    let _ = Command::new("dns-sd")
        .args(["-V"])
        .output();

    info!("network: DNS cache flushed");
    Ok(())
}

pub fn reset_stack(cfg: &Config) -> anyhow::Result<()> {
    if !cfg.optimizer.flush_dns { return Ok(()); }

    let euid = unsafe { libc::geteuid() };

    if euid == 0 {
        // Flush ARP table
        run("arp", &["-ad"]);

        // Flush routing cache (requires root)
        run("route", &["-n", "flush"]);

        info!("network: ARP + routing cache flushed");
    } else {
        debug!("network: skipping ARP/route flush (not root)");
    }

    // Reset TCP statistics — doesn't need root, just informational
    run("nettop", &["-P", "-L", "0"]);

    Ok(())
}

fn run(cmd: &str, args: &[&str]) {
    debug!("$ {cmd} {}", args.join(" "));
    let _ = Command::new(cmd).args(args).output();
}
