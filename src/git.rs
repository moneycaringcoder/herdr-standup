//! The read-only git layer.
//!
//! # The one rule
//!
//! This plugin must never write to a user's repository. Every invocation in this
//! module therefore:
//!
//! - passes `--no-optional-locks`, so `status` does not take `index.lock` to
//!   write back its stat cache;
//! - reads only — no `add`, no `write-tree`, no `merge-tree`, nothing that
//!   creates an object;
//! - never sets `GIT_INDEX_FILE` on a real index, because it never stages;
//! - runs with `GIT_OPTIONAL_LOCKS=0` and an object directory that would be a
//!   temporary one if anything ever did try to write.
//!
//! `tests/read_only.rs` fingerprints the index, working tree, refs and loose
//! object count of a fixture repository before and after a full run and fails on
//! any difference. That test is the contract; this module is its implementation.
//!
//! # The traps this module exists to avoid
//!
//! - `git rev-parse --since=<garbage>` exits **0** and answers *now*, so an
//!   unparseable window silently produces an empty digest. [`Git::resolve_date`]
//!   must detect that and fail loudly.
//! - `--since` prunes traversal on commit date, and with badly skewed committer
//!   timestamps it can stop early and miss commits.
//! - A merge commit carries no diffstat of its own, so it counts toward the
//!   commit total and contributes nothing to churn. Summing per-commit file
//!   counts double-counts a file edited twice; churn counts the *union*.
//! - An unborn branch and a branch deleted underneath a live checkout look
//!   identical to `symbolic-ref`. The discriminator is the worktree's own
//!   `logs/HEAD`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::model::{CheckoutReport, RepoKey, Window};
use crate::Result;

/// A directory that git has confirmed is a checkout, with its repository
/// identity resolved.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CheckoutId {
    /// `--show-toplevel`, absolute and canonical.
    pub path: PathBuf,
    /// `--git-common-dir`, absolute and canonical. Shared by every linked
    /// worktree of one repository.
    pub repo_key: RepoKey,
    /// The main checkout of the repository, when it can be determined.
    pub repo_root: PathBuf,
    pub is_linked_worktree: bool,
}

/// A git runner. Holds the resolved binary and the per-invocation timeout, so
/// one wedged repository cannot stall the whole digest.
#[derive(Debug, Clone)]
pub struct Git {
    program: PathBuf,
    timeout: Duration,
}

impl Git {
    /// herdr runs plugin commands with a minimal `PATH` and no shell, so the
    /// binary is resolved explicitly rather than assumed.
    pub fn new(timeout: Duration) -> Self {
        let _ = timeout;
        unimplemented!("git::Git::new — owned by the collector")
    }

    /// Path of the git binary in use. Rendered in error messages, because "git
    /// not found" from inside a plugin with a minimal PATH is otherwise a very
    /// confusing failure.
    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Resolves a candidate directory to a checkout.
    ///
    /// `Ok(None)` means "this is not a git checkout", which is ordinary data —
    /// most sessions have at least one workspace in a plain directory. `Err`
    /// means git itself could not be run, which is not.
    pub fn identify(&self, path: &Path) -> Result<Option<CheckoutId>> {
        let _ = path;
        unimplemented!("git::Git::identify — owned by the collector")
    }

    /// Every checkout of the same repository, including ones no workspace is
    /// sitting in. `Ok(vec![])` for a repository with no linked worktrees.
    pub fn worktrees(&self, id: &CheckoutId) -> Result<Vec<PathBuf>> {
        let _ = id;
        unimplemented!("git::Git::worktrees — owned by the collector")
    }

    /// Everything the digest knows about one checkout.
    ///
    /// Infallible by construction: anything that goes wrong lands in
    /// `report.problems` and is rendered, because a checkout dropped from the
    /// digest because git stuttered is indistinguishable from a quiet one.
    pub fn report(&self, id: &CheckoutId, window: &Window) -> CheckoutReport {
        let _ = (id, window);
        unimplemented!("git::Git::report — owned by the collector")
    }

    /// Resolves a `--since`-style approxidate spec to an absolute epoch second,
    /// using git's own parser so the plugin accepts exactly what git accepts.
    ///
    /// **Must reject unparseable input.** `git rev-parse --since=bogus` exits 0
    /// and returns the current time; passing that through would render a typo as
    /// a quiet day. Verified on git 2.53.0.
    pub fn resolve_date(&self, repo: &Path, spec: &str) -> Result<i64> {
        let _ = (repo, spec);
        unimplemented!("git::Git::resolve_date — owned by the collector")
    }

    /// Creates, if needed, an empty bare repository used only as a context for
    /// [`Git::resolve_date`] when the session has no checkouts of its own.
    /// `git rev-parse` refuses to run outside a repository.
    pub fn ensure_date_ref_repo(&self, path: &Path) -> Result<()> {
        let _ = path;
        unimplemented!("git::Git::ensure_date_ref_repo — owned by the collector")
    }
}
