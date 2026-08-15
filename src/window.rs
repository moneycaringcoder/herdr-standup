//! Resolving the reporting window, and the `--since-last` marker.
//!
//! Windows are resolved to **absolute instants before any `git log` runs**, for
//! one reason: `git rev-parse --since=<garbage>` exits 0 and answers *now*, so
//! an unparseable `--since` would otherwise produce a perfectly formatted empty
//! digest. Resolving eagerly turns that into a loud error, and as a bonus lets
//! the header state exactly which instant the window starts at instead of
//! echoing the user's fuzzy phrase back at them.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::git::Git;
use crate::model::{Note, Stamp, Window};
use crate::Result;

/// Resolves the window the digest covers.
///
/// `anchors` are candidate directories to run `git rev-parse` inside, in
/// preference order; git refuses to parse a date outside a repository. If none
/// of them work, the caller's `date_ref_repo` is created and used, so the window
/// is still validated in a session with no checkouts at all.
///
/// Returns the window plus any notes worth showing the user — for example, that
/// `--since-last` found no previous run and fell back to the default.
pub fn resolve(
    git: &Git,
    anchors: &[PathBuf],
    date_ref_repo: &Path,
    config: &Config,
) -> Result<(Window, Vec<Note>)> {
    let _ = (git, anchors, date_ref_repo, config);
    unimplemented!("window::resolve — owned by the surface builder")
}

/// The timestamp of the previous run a human read, if one was recorded.
///
/// A missing marker is the normal first-run case. A corrupt one is a warning
/// and a `None`, never a hard failure: a mangled state file must not stop the
/// digest from printing.
pub fn previous_run() -> Option<Stamp> {
    unimplemented!("window::previous_run — owned by the surface builder")
}

/// Records this run's timestamp for a later `--since-last`.
///
/// Best-effort: an unwritable state directory is reported to stderr and
/// otherwise ignored, because it must not fail the digest the user asked for.
/// Written **after** a successful render, so a run that blew up does not
/// advance the marker past work it never showed anybody.
pub fn record_run(at: &Stamp) -> Result<()> {
    let _ = at;
    unimplemented!("window::record_run — owned by the surface builder")
}
