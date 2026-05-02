# Hush

> Silent. Instant. macOS cleaner.

One binary. Zero noise. Runs on login, cleans everything, never asks twice.

```
hush clean       # full clean in one shot
hush crush       # kill all rogue background processes
hush snapshot    # delete stale APFS snapshots
hush optimize    # apply all system tweaks
hush status      # show daemon state, config path, log size
hush install     # register daemon — runs forever, silently
```

---

## Goals

| Metric | Target |
|--------|--------|
| Daemon RSS (idle) | **< 128 KB** |
| Binary size (release) | **< 1.5 MB** |
| Cold start to first pass | **< 100 ms** |
| External runtime dependencies | **none** |
| Configuration hot-reload latency | **< 100 ms** |

---

## Install

```bash
# Build (requires Rust stable, macOS 12+)
cargo build --release

# Put on PATH
sudo cp target/release/hush /usr/local/bin/hush

# Install LaunchAgent (auto-start on login)
hush install

# First run
hush clean
```

### Uninstall

```bash
hush uninstall
sudo rm /usr/local/bin/hush
rm -rf ~/Library/Application\ Support/hush
```

---

## Commands

### `hush clean` — full clean

```
FLAGS
  -n, --dry-run    Show what would be removed, change nothing
  --system         .DS_Store, AppleDouble, crash logs, tmp, system logs
  --cache          Developer caches (Xcode, npm, cargo, brew, pip, …)
  --projects       Project build artifacts (node_modules, target/, .build, …)
  --apps           App cache sweep (unused > 7d threshold)
  --snapshots      Delete stale APFS local snapshots
  --uninstall      Uninstall apps unused > 1 year (requires --apps)
  -a, --all        All passes (default)
```

#### What `hush clean` touches

**System junk**
| Target | Description |
|--------|-------------|
| `.DS_Store` | Finder metadata — recursively across home + /Volumes |
| `._*` (AppleDouble) | HFS+ resource fork remnants |
| `*.crash`, `*.spin`, `*.hang`, `*.ips` | Crash/diagnostic reports older than `log_max_age_days` |
| `/private/var/db/DiagnosticPipeline` | System diagnostic pipeline DB |
| `/private/var/db/powerlog` | Battery/power audit logs |
| `/private/tmp` | Temp files older than `tmp_max_age_hours` |
| System log files | `/private/var/log/**/*.log` older than threshold |

**Developer caches** (50+ entries, risk-rated)
| Risk | Meaning |
|------|---------|
| 🟢 safe | Rebuilt automatically by the tool |
| 🟡 caution | May require a large re-download |
| 🔴 risky | Contains irreplaceable data — **skipped by default** |

Risky entries (Docker volumes, Ollama models, etc.) are listed in `audit` but never auto-deleted.

**Project artifacts** — scans `~/Developer`, `~/Documents`, `~/Projects`, etc. for:
`node_modules`, `target/`, `.build/`, `build/`, `vendor/`, `.dart_tool/`, `.terraform/`, and more — only if ≥ 10 MB.

---

### `hush snapshot` — APFS snapshot management

Parses `tmutil listlocalsnapshots /`, then:

- Keeps the **N newest** snapshots (`thresholds.snapshot_keep_count`)
- Deletes everything older than `thresholds.snapshot_max_age_days`
- Skips deletion if Time Machine is currently backing up
- Also removes stale `.inProgress` backup bundles (after `incomplete_safe_hours`)

```bash
hush snapshot --list    # audit only
hush snapshot           # delete stale
hush snapshot -n        # dry-run
```

---

### `hush crush` — rogue process killer

```bash
hush crush              # kill everything in rogue_list.process_names
hush crush "AppName"    # kill a specific process by name
```

Kill sequence per target:
1. `pkill -x <name>` — SIGTERM (graceful, waits `graceful_timeout_secs`)
2. `pkill -9 -x <name>` — SIGKILL (force)
3. `launchctl bootout gui/<uid>/<pid>` — evict from launchd (prevents respawn)
4. `pgrep -x <name>` — verify dead

Also removes **broken LaunchAgents** — plists whose backing binary no longer exists on disk.

---

### `hush silence` — notification & background control

```bash
hush silence                    # all rules
hush silence --notifications    # restrict NC to banners-only
hush silence --background       # disable non-essential LaunchAgents
hush silence --dock             # force apps to quit on window close
```

**Notification restriction** writes to `com.apple.notificationcenterui.<bundleId>`:
- Rogue list apps → `alert-style = none` (fully silent)
- All others → `alert-style = banner`, badge off, sound off

**Force-quit-on-close** sets `NSQuitAlwaysKeepsWindows = false` globally and per-app. Apps no longer linger in the Dock or menu bar after their last window closes.

---

### `hush optimize` — system tuning

```bash
hush optimize              # all passes
hush optimize --network    # flush DNS + ARP
hush optimize --system     # LaunchServices, SQLite, quarantine DB, saved state…
hush optimize --ui         # Dock speed, QuickLook, Finder, font cache
```

**System passes**

| Pass | What it does |
|------|-------------|
| `launch_services_rebuild` | `lsregister -kill -r` — fixes "Open With" duplicates |
| `sqlite_vacuum` | `VACUUM` all `.db` files in ~/Library (< 500 MB, not in use) |
| `quarantine_cleanup` | Prunes `LSQuarantineEvent` rows older than threshold |
| `saved_state_cleanup` | Removes stale `.savedState` bundles (app gone or > 30d) |
| `broken_launch_agents` | Unloads + removes LaunchAgent plists with missing binaries |
| `notification_center_cleanup` | Prunes NC pref domains for uninstalled apps |
| `coreduet_cleanup` | Clears CoreDuet knowledge DB (Siri Suggestions data) |
| `periodic_maintenance` | Runs BSD `periodic daily/weekly/monthly` scripts |

**UI passes**

| Pass | What it does |
|------|-------------|
| `quicklook_refresh` | Kills `qlmanage`, clears thumbnail cache |
| `font_cache_rebuild` | Removes `com.apple.ATS` cache + restarts ATSServer |
| `prevent_network_dsstore` | `DSDontWriteNetworkStores` + `DSDontWriteUSBStores` |
| `dock_refresh` | Zero autohide delay, scale minimize, no recent apps |

---

### `hush audit` — read-only report

```bash
hush audit
```

Prints:
1. All apps sorted by days-since-last-use + size
2. All local APFS snapshots + age
3. (No changes are made)

---

### `hush install` / `hush uninstall` — daemon lifecycle

```bash
hush install      # writes ~/Library/LaunchAgents/com.hush.daemon.plist
hush uninstall    # unloads + removes
```

Daemon runs with `Nice = 5` and `LowPriorityIO = true` — it never competes with foreground work. Logs to `~/Library/Logs/hush.log`.

---

## `config.json`

Default location: `~/Library/Application Support/hush/config.json`

Custom: `hush --config /path/to/config.json <command>`

### Hot-reload

Edit and save `config.json`. The daemon picks it up **within 100 ms** via macOS `kqueue EVFILT_VNODE NOTE_WRITE`. No restart needed. Atomic editor saves (temp→rename) are also handled (`NOTE_RENAME`).

### Key fields

```jsonc
{
  "thresholds": {
    "cache_unused_days": 7,          // app cache sweep threshold
    "uninstall_unused_days": 365,    // stale app uninstall threshold
    "snapshot_max_age_days": 7,      // APFS snapshot max age
    "snapshot_keep_count": 2         // always keep N newest snapshots
  },

  "snapshots": {
    "auto_delete": true,
    "skip_if_tm_running": true,      // safe: never interfere with active backup
    "delete_incomplete_backups": true
  },

  "silence": {
    "force_quit_on_window_close": true,  // no menu-bar/Dock lingering
    "per_app_overrides": {
      "com.apple.Mail": { "allow_notifications": true, "allow_background": true }
    }
  },

  "whitelist": {
    "bundle_ids": [ "com.apple.finder", ... ],
    "apps":       [ "Finder", "Dock", ... ]
  },

  "rogue_list": {
    "bundle_ids":    [ "com.mackeeper.MacKeeper", ... ],
    "process_names": [ "MacKeeper Helper", ... ]
  }
}
```

---

## Safety guarantees

### 1. Signature & notarisation protection
- **Never** deletes files inside a live `.app/Contents/` tree
- Calls `codesign --verify` before any uninstall operation
- If the bundle is mid-update (codesign fails), the uninstall is skipped

### 2. File-lock / process occupancy
- Directories > 512 MB are checked via `lsof +D` before deletion
- Process stem-name check (`pgrep -f`) for every app cache being cleared
- Kill sequence uses SIGTERM → sleep → SIGKILL → `launchctl bootout` (prevents respawn)

### 3. Permission escalation prevention
- Never writes to TCC database directly
- All writes stay in `~/Library` (user-space)
- Paths requiring FDA (`/Library/Mail`, `/Library/Messages`, etc.) are in `guard.rs` blocklist
- No `sudo` prompts — operations that need root are skipped with a log message

### 4. Critical cache protection
`guard.rs` maintains a hard-blocked segment list including:
```
com.apple.systempreferences   com.apple.controlcenter
com.apple.coreaudio           com.apple.security.*
com.apple.trustd              com.apple.bird  (iCloud)
org.cups.*                    com.apple.dock.saved-state
```
Any path containing these segments is unconditionally skipped, regardless of config.

---

## Architecture

```
src/
├── main.rs                  CLI entry, subcommand dispatch
├── cli.rs                   clap argument definitions
├── config.rs                Typed config structs + kqueue hot-reload
├── guard.rs                 Safety: hard-blocks, lsof, codesign, TCC, critical bundles
├── daemon.rs                Background event loop + LaunchAgent install
│
├── cleaner/
│   ├── ops.rs               safe_remove(), walk_delete_if(), measure_size()
│   ├── system.rs            DS_Store, AppleDouble, crash logs, tmp, system logs
│   ├── cache.rs             Dev caches (50+ entries) + project artifact sweep
│   ├── apps.rs              mdls last-used, rogue removal, stale uninstall
│   └── snapshots.rs         APFS snapshot audit + deletion + incomplete backups
│
├── optimizer/
│   ├── network.rs           DNS flush, ARP/routing reset
│   ├── system.rs            LaunchServices, SQLite vacuum, quarantine DB,
│   │                        saved state, broken agents, NC cleanup, CoreDuet, periodic
│   └── ui.rs                QuickLook, font cache, Dock tuning, Finder tweaks
│
└── sentinel/
    ├── notifications.rs     NC restriction via defaults write (no TCC touch)
    ├── processes.rs         Rogue process crusher + broken agent killer
    └── silence.rs           LaunchAgent disabling + force-quit-on-close
```

### Memory budget

| Component | Budget |
|-----------|--------|
| Daemon main stack | 8 KB |
| Config watcher thread stack | 64 KB (explicit) |
| Parsed Config struct (heap) | ~4 KB |
| Serde / log buffers (transient) | ~16 KB |
| Total idle RSS | **< 100 KB** |

Achieved via: no tokio, no arc-swap, no allocator-heavy crates, `panic = "abort"`, `opt-level = "z"`, `strip = true`.

---

## Requires root (sudo)

These operations are skipped silently when not root:

| Operation | Reason |
|-----------|--------|
| `hush optimize --network` (ARP flush) | `route flush` needs root |
| `hush clean --system` (Spotlight reindex) | `mdutil` needs root |
| `purge_inactive_memory` | `purge` needs root |
| `/Library/LaunchDaemons` agent removal | System-level paths |

Run `sudo hush optimize` once at setup time to apply all root-level tweaks.

---

## Logs

```bash
tail -f ~/Library/Logs/hush.log           # daemon output
RUST_LOG=debug hush clean                 # verbose one-shot
RUST_LOG=info hush snapshot --list        # info-level output
```

---

## FAQ

**Will this break my apps?**
The `guard.rs` hard-block list and critical bundle checks prevent touching anything system-critical. `risky`-rated entries (Docker, Ollama models) are never auto-deleted.

**Can I undo a clean?**
Not automatically. Use `hush clean -n` (dry-run) first to see exactly what will be removed.

**Why does `hush crush` not kill `[process]`?**
It's either in `whitelist.apps`, starts with `com.apple.`, or its process name doesn't match `rogue_list.process_names`. Add it to the rogue list or run `hush crush "Exact Process Name"`.

**Does Hush need Full Disk Access?**
No. All paths are within user-space (`~/Library`). System-level paths requiring FDA are blocked by `guard::requires_fda()` and skipped with a log entry, not a permission prompt.
