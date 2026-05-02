// error.rs — structured error types for Hush
//
// Design rationale:
//   The previous ops.rs pattern of `warn!(...); return 0` discards the *kind*
//   of failure, making it impossible for callers to distinguish between:
//     - ENOENT  (path vanished between scan and delete — harmless, skip)
//     - EACCES  (permission denied — log clearly, surface to user)
//     - EBUSY   (file locked by another process — retry or skip)
//     - ENOTSUP (filesystem doesn't support the operation — skip gracefully)
//     - Loop    (symlink cycle detected during traversal — abort branch)
//
//   `HushError` carries this classification so callers can react correctly.
//   `RemoveResult` bundles freed bytes with a classification log of skips,
//   which the CLI can surface in verbose mode.

use std::path::PathBuf;
use std::io;
#[cfg(unix)] use libc;

// ── primary error type ────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum HushError {
    /// Path no longer exists — safe to ignore in sweep loops
    NotFound(PathBuf),

    /// Permission denied — worth logging; never silently swallowed
    PermissionDenied { path: PathBuf, source: io::Error },

    /// File or directory is locked by a running process
    Busy { path: PathBuf, source: io::Error },

    /// Filesystem does not support the operation (e.g. read-only mount)
    Unsupported { path: PathBuf, source: io::Error },

    /// Symlink cycle detected during recursive traversal
    SymlinkLoop(PathBuf),

    /// A hard safety block fired (guard.rs) — path is unconditionally protected
    HardBlocked(PathBuf),

    /// A process occupancy check (lsof) confirmed the path is in active use
    InUse(PathBuf),

    /// Generic I/O error that doesn't fit the categories above
    Io { path: PathBuf, source: io::Error },

    /// External tool (mdls, tmutil, sqlite3 …) returned non-zero or bad output
    ExternalTool { tool: &'static str, detail: String },

    /// Config parsing / schema mismatch
    Config(String),
}

impl HushError {
    /// True if the error is harmless in a sweep context (just skip this path).
    pub fn is_skippable(&self) -> bool {
        matches!(self,
            HushError::NotFound(_)
            | HushError::HardBlocked(_)
            | HushError::InUse(_)
            | HushError::SymlinkLoop(_)
            | HushError::Unsupported { .. }
        )
    }

    /// Classify a raw io::Error at a path into a HushError.
    pub fn from_io(e: io::Error, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        match e.kind() {
            io::ErrorKind::NotFound          => HushError::NotFound(path),
            io::ErrorKind::PermissionDenied  => HushError::PermissionDenied { path, source: e },
            _ => {
                // EBUSY / ENOTSUP are not stable ErrorKind variants yet —
                // match raw OS codes via libc constants so the intent is clear.
                #[cfg(unix)]
                if e.raw_os_error() == Some(libc::EBUSY) {
                    return HushError::Busy { path, source: e };
                }
                #[cfg(unix)]
                if e.raw_os_error() == Some(libc::ENOTSUP) {
                    return HushError::Unsupported { path, source: e };
                }
                HushError::Io { path, source: e }
            }
        }
    }
}

impl std::fmt::Display for HushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HushError::NotFound(p)                        => write!(f, "not found: {}", p.display()),
            HushError::PermissionDenied { path, source }  => write!(f, "permission denied: {} ({source})", path.display()),
            HushError::Busy { path, source }              => write!(f, "in use (EBUSY): {} ({source})", path.display()),
            HushError::Unsupported { path, source }       => write!(f, "unsupported: {} ({source})", path.display()),
            HushError::SymlinkLoop(p)                     => write!(f, "symlink loop: {}", p.display()),
            HushError::HardBlocked(p)                     => write!(f, "hard-blocked: {}", p.display()),
            HushError::InUse(p)                           => write!(f, "process in use: {}", p.display()),
            HushError::Io { path, source }                => write!(f, "I/O error at {}: {source}", path.display()),
            HushError::ExternalTool { tool, detail }      => write!(f, "{tool}: {detail}"),
            HushError::Config(msg)                        => write!(f, "config error: {msg}"),
        }
    }
}

impl std::error::Error for HushError {}

// Allow `?` from anyhow contexts
impl From<HushError> for anyhow::Error {
    fn from(e: HushError) -> Self { anyhow::anyhow!("{e}") }
}

// ── removal result ────────────────────────────────────────────────────────────

/// Returned by safe_remove() and sweep functions.
/// Carries freed bytes AND a classified list of every path that was skipped,
/// so the caller (CLI verbose mode, test assertions) can inspect them.
#[derive(Debug, Default)]
pub struct RemoveResult {
    pub freed_bytes: u64,
    pub skipped:     Vec<Skip>,
    pub errors:      Vec<HushError>,
}

#[derive(Debug)]
pub struct Skip {
    pub path:   PathBuf,
    pub reason: SkipReason,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SkipReason {
    HardBlocked,
    ProcessInUse,
    SymlinkLoop,
    DryRun,
    NotFound,
    Unsupported,
}

impl RemoveResult {
    pub fn merge(&mut self, other: RemoveResult) {
        self.freed_bytes += other.freed_bytes;
        self.skipped.extend(other.skipped);
        self.errors.extend(other.errors);
    }

    pub fn add_skip(&mut self, path: PathBuf, reason: SkipReason) {
        self.skipped.push(Skip { path, reason });
    }

    pub fn add_error(&mut self, e: HushError) {
        // Permission denied and Io errors are real errors — preserve them.
        // Skippable errors are demoted to Skip entries.
        if e.is_skippable() {
            let (path, reason) = match e {
                HushError::NotFound(p)    => (p, SkipReason::NotFound),
                HushError::HardBlocked(p) => (p, SkipReason::HardBlocked),
                HushError::InUse(p)       => (p, SkipReason::ProcessInUse),
                HushError::SymlinkLoop(p) => (p, SkipReason::SymlinkLoop),
                HushError::Unsupported { path, .. } => (path, SkipReason::Unsupported),
                _ => unreachable!(),
            };
            self.skipped.push(Skip { path, reason });
        } else {
            self.errors.push(e);
        }
    }

    /// Log all non-skippable errors at WARN level.
    pub fn log_errors(&self) {
        for e in &self.errors {
            log::warn!("hush: {e}");
        }
    }

    /// True if any permission-denied errors occurred (surface to user).
    pub fn has_permission_errors(&self) -> bool {
        self.errors.iter().any(|e| matches!(e, HushError::PermissionDenied { .. }))
    }
}
