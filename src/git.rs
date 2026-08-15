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
//!
//! # Verified behaviour this module encodes
//!
//! Measured against purpose-built fixtures on git 2.53.0, Linux. Each of these
//! is pinned by a test, so a future git that changes its mind fails CI rather
//! than quietly changing the digest.
//!
//! ## `--max-age` prunes, and the pruning loses commits
//!
//! `--max-age` (the plumbing spelling of `--since`) is a *traversal cutoff*, not
//! a filter: the walk stops at the first commit older than the cutoff. A history
//! whose committer timestamps are out of order — a rebase, a cherry-pick with
//! `--committer-date-is-author-date`, a machine with a skewed clock — therefore
//! hides everything behind the first old commit. A six-commit fixture with two
//! backdated commits reported **one** of the three commits that were genuinely
//! inside the window.
//!
//! `--since-as-filter=<date>` (git 2.37+) applies the same comparison without
//! pruning, and reported all three. This module uses it, and falls back to
//! `--max-age` on older git with a problem recorded on the report, because a
//! digest that quietly drops two thirds of the day is exactly the failure this
//! plugin exists to prevent. `--min-age` (`--until`) never pruned in any
//! arrangement tested, so it is passed as-is.
//!
//! ## The log record framing
//!
//! `--format` output cannot be split on a chosen separator alone: a commit
//! subject is arbitrary bytes, and a fixture subject containing a literal
//! `\x1e` really does break a naive `\x1e`-split. The framing that holds is
//! `-z`, which terminates each `--format` record and each `--numstat` path with
//! a NUL, plus `%x1e` at the **start** of the format so header fields can be
//! told from numstat fields. A separator inside a subject is then harmless: only
//! the first byte of a NUL-delimited field is ever consulted. `-z` also means
//! paths arrive as raw bytes rather than C-quoted, so a filename containing a
//! space, a newline or a non-UTF-8 byte survives.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::clock;
use crate::config::DEFAULT_BRANCH_CANDIDATES;
use crate::model::{
    CheckoutReport, Churn, Commit, Dirty, Head, Landed, RepoKey, Stamp, Tracking, Window,
};
use crate::Result;

/// Starts every `git log` header record. Chosen because it is the byte least
/// likely to be typed, but the parser does not depend on that: see the module
/// note on framing.
const RECORD: u8 = 0x1e;
/// Separates the fields inside one header record.
const UNIT: char = '\u{1f}';

/// The format handed to `git log`. `%ct` is the **commit** time, not `%at`:
/// `--since`/`--until` filter on commit date, so a rebased commit displayed by
/// its author date would appear in the digest under a day the window never
/// covered.
const LOG_FORMAT: &str = "--format=%x1e%H%x1f%P%x1f%an%x1f%ct%x1f%s";

/// Specs that legitimately resolve to the current instant, and so cannot be
/// distinguished from unparseable input by their result.
///
/// Verified on git 2.53.0: `today` resolves to **now**, not to midnight, so it
/// belongs here — rejecting it would refuse a spec git accepts. `midnight` is
/// the spelling that means the start of the local day, and is this plugin's
/// default window.
const SPECS_MEANING_NOW: &[&str] = &["now", "today"];

/// How close to the current instant a resolved date has to be before it is
/// treated as git's silent "I could not parse that" answer.
const NOW_SLACK_SECONDS: i64 = 2;

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

/// One finished invocation. `code` is `None` when the child was killed by a
/// signal, which includes the timeout kill.
#[derive(Debug)]
struct GitOut {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

impl GitOut {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }

    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

impl Git {
    /// herdr runs plugin commands with a minimal `PATH` and no shell, so the
    /// binary is resolved explicitly rather than assumed.
    pub fn new(timeout: Duration) -> Self {
        Self {
            program: resolve_program(),
            timeout,
        }
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
        let out = self.run(
            path,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-common-dir",
            ],
        )?;
        if out.timed_out {
            return Err(format!(
                "{} rev-parse timed out after {:?} in {}",
                self.program.display(),
                self.timeout,
                path.display()
            )
            .into());
        }
        // Anything git itself rejected — not a repository, a bare repository
        // with no work tree, a directory that has since been removed — is a
        // "no", not a failure. Only a git that could not run at all is an error,
        // and that is the spawn failure above.
        if !out.ok() {
            return Ok(None);
        }

        let text = out.stdout_text();
        let mut lines = text.lines();
        let (Some(toplevel), Some(common_dir)) = (lines.next(), lines.next()) else {
            return Ok(None);
        };
        if toplevel.is_empty() || common_dir.is_empty() {
            return Ok(None);
        }

        // Canonicalize before comparing: a symlinked or bind-mounted checkout
        // otherwise yields two identities for one repository, and the digest
        // would report it twice.
        let toplevel = canonical(Path::new(toplevel));
        let common_dir = canonical(Path::new(common_dir));

        // `<root>/.git` is the ordinary layout. Anything else — `--separate-git-dir`,
        // a bare repository with a work tree bolted on — has no derivable main
        // checkout, and the toplevel we are standing in is the best answer.
        let repo_root = match common_dir.file_name() {
            Some(name) if name == ".git" => common_dir
                .parent()
                .map(canonical)
                .unwrap_or_else(|| toplevel.clone()),
            _ => toplevel.clone(),
        };

        Ok(Some(CheckoutId {
            is_linked_worktree: toplevel != repo_root,
            path: toplevel,
            repo_key: RepoKey(common_dir.to_string_lossy().into_owned()),
            repo_root,
        }))
    }

    /// Every checkout of the same repository, including ones no workspace is
    /// sitting in. `Ok(vec![])` for a repository with no linked worktrees.
    pub fn worktrees(&self, id: &CheckoutId) -> Result<Vec<PathBuf>> {
        let out = self.run_ok(&id.path, &["worktree", "list", "--porcelain", "-z"])?;

        // Records are separated by an empty NUL field. `worktree <abs-path>` is
        // always first; nothing after it has a guaranteed order.
        let mut paths = Vec::new();
        let mut record: Vec<&[u8]> = Vec::new();
        let flush = |record: &mut Vec<&[u8]>, paths: &mut Vec<PathBuf>| {
            if record.is_empty() {
                return;
            }
            let skip = record
                .iter()
                .any(|field| *field == b"bare" || field.starts_with(b"prunable"));
            if !skip {
                if let Some(raw) = record[0].strip_prefix(b"worktree ") {
                    paths.push(canonical(Path::new(&bytes_to_path(raw))));
                }
            }
            record.clear();
        };
        for field in out.stdout.split(|byte| *byte == 0) {
            if field.is_empty() {
                flush(&mut record, &mut paths);
            } else {
                record.push(field);
            }
        }
        flush(&mut record, &mut paths);
        Ok(paths)
    }

    /// Everything the digest knows about one checkout.
    ///
    /// Infallible by construction: anything that goes wrong lands in
    /// `report.problems` and is rendered, because a checkout dropped from the
    /// digest because git stuttered is indistinguishable from a quiet one.
    pub fn report(&self, id: &CheckoutId, window: &Window) -> CheckoutReport {
        let mut problems: Vec<String> = Vec::new();
        let path = id.path.clone();

        let git_dir = self
            .capture(
                &path,
                &["rev-parse", "--path-format=absolute", "--git-dir"],
                &mut problems,
            )
            .filter(GitOut::ok)
            .map(|out| PathBuf::from(out.stdout_text()));

        let head = self.head(&path, git_dir.as_deref(), &mut problems);
        let (commits, churn) = match head {
            // An unborn branch has no history to walk, and `git log` refuses
            // outright. That is the expected state, not a problem.
            Head::Unborn { .. } => (Vec::new(), Churn::default()),
            _ => self.commits(&path, window, &mut problems),
        };
        let dirty = self.dirty(&path, &mut problems);
        let tracking = self.tracking(&path, &head, &mut problems);
        let landed = self.landed(&path, &head, &mut problems);

        CheckoutReport {
            path,
            repo_key: id.repo_key.clone(),
            repo_root: id.repo_root.clone(),
            is_linked_worktree: id.is_linked_worktree,
            head,
            commits,
            churn,
            dirty,
            tracking,
            landed,
            problems,
        }
    }

    /// Resolves a `--since`-style approxidate spec to an absolute epoch second,
    /// using git's own parser so the plugin accepts exactly what git accepts.
    ///
    /// **Must reject unparseable input.** `git rev-parse --since=bogus` exits 0
    /// and returns the current time; passing that through would render a typo as
    /// a quiet day. Verified on git 2.53.0.
    pub fn resolve_date(&self, repo: &Path, spec: &str) -> Result<i64> {
        if spec.trim().is_empty() {
            return Err("an empty window is not a date git can parse".into());
        }

        let out = self.run_ok(repo, &["rev-parse", &format!("--since={spec}")])?;
        let text = out.stdout_text();
        let epoch: i64 = text
            .strip_prefix("--max-age=")
            .ok_or_else(|| {
                format!("git rev-parse --since={spec} answered {text:?}, not --max-age")
            })?
            .trim()
            .parse()
            .map_err(|err| format!("git rev-parse --since={spec} answered {text:?}: {err}"))?;

        // The trap. git answers *now*, with exit status 0, for anything it
        // cannot parse — including the empty string. Only a handful of specs
        // legitimately mean now, so everything else that lands on now was a typo
        // the user needs to be told about, not a quiet day.
        let now = clock::now();
        let means_now = SPECS_MEANING_NOW.contains(&spec.trim().to_ascii_lowercase().as_str());
        if !means_now && (now - epoch).abs() <= NOW_SLACK_SECONDS {
            return Err(format!(
                "git could not parse the window {spec:?}: it resolved to the current time, \
                 which would render as an empty digest. Try a spec like `midnight`, \
                 `yesterday`, `2 hours ago` or `2026-08-01`."
            )
            .into());
        }
        Ok(epoch)
    }

    /// Creates, if needed, an empty bare repository used only as a context for
    /// [`Git::resolve_date`] when the session has no checkouts of its own.
    /// `git rev-parse` refuses to run outside a repository.
    pub fn ensure_date_ref_repo(&self, path: &Path) -> Result<()> {
        let parent = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;

        // The one place this module creates anything, so it is also the one
        // place that has to prove it is not standing in somebody's repository.
        // Checked before the idempotence probe below, because inside a user
        // repository that probe would answer "already a repository" about the
        // *enclosing* checkout and quietly do the wrong thing.
        if let Ok(out) = self.run(parent, &["rev-parse", "--git-dir"]) {
            if out.ok() {
                return Err(format!(
                    "refusing to create the date-reference repository at {}: {} is already inside \
                     a git repository",
                    path.display(),
                    parent.display()
                )
                .into());
            }
        }

        // Idempotent: an existing repository here is the normal case after the
        // first run.
        if let Ok(out) = self.run(path, &["rev-parse", "--git-dir"]) {
            if out.ok() {
                return Ok(());
            }
        }

        std::fs::create_dir_all(path)
            .map_err(|err| format!("could not create {}: {err}", path.display()))?;
        // Run *in* the directory rather than naming it on the command line, so a
        // state directory whose path is not valid UTF-8 still works.
        let out = self.run(path, &["init", "--bare", "-q"])?;
        if out.timed_out {
            return Err(format!(
                "{} init --bare timed out after {:?}",
                self.program.display(),
                self.timeout
            )
            .into());
        }
        if !out.ok() {
            return Err(format!(
                "could not create the date-reference repository at {}: {}",
                path.display(),
                out.stderr_text()
            )
            .into());
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Report parts
    // -----------------------------------------------------------------------

    /// All four states of [`Head`].
    ///
    /// The hard half is unborn versus branch-deleted. `symbolic-ref -q HEAD`
    /// exits 0 and prints the same name for both on git 2.53.0, so the
    /// discriminator is the worktree's own HEAD reflog: a checkout that ever had
    /// a commit has `<git-dir>/logs/HEAD`, a freshly `git init`-ed one does not.
    /// The git dir here must be the per-worktree one, never the common dir.
    fn head(&self, path: &Path, git_dir: Option<&Path>, problems: &mut Vec<String>) -> Head {
        let symbolic = self.capture(path, &["symbolic-ref", "-q", "HEAD"], problems);
        let Some(symbolic) = symbolic else {
            return Head::Detached { oid: String::new() };
        };

        if !symbolic.ok() {
            // Detached: `symbolic-ref` exits non-zero, and the raw object id is
            // the only name this checkout has.
            let oid = self
                .capture(
                    path,
                    &["rev-parse", "--verify", "-q", "HEAD^{commit}"],
                    problems,
                )
                .filter(GitOut::ok)
                .map(|out| out.stdout_text())
                .unwrap_or_default();
            if oid.is_empty() {
                problems.push(format!(
                    "HEAD in {} is neither a branch nor a commit",
                    path.display()
                ));
            }
            return Head::Detached { oid };
        }

        let reference = symbolic.stdout_text();
        let name = reference
            .strip_prefix("refs/heads/")
            .unwrap_or(&reference)
            .to_string();

        let resolved = self
            .capture(
                path,
                &[
                    "rev-parse",
                    "--verify",
                    "-q",
                    &format!("{reference}^{{commit}}"),
                ],
                problems,
            )
            .filter(GitOut::ok)
            .map(|out| out.stdout_text())
            .filter(|oid| !oid.is_empty());
        if let Some(oid) = resolved {
            return Head::Branch { name, oid };
        }

        match git_dir {
            // No problem is recorded here. `Head::BranchDeleted` already says
            // this, the renderers already print it loudly, and
            // `CheckoutReport::activity` already sorts it to the top — adding a
            // problem as well printed the same warning twice in the live output.
            Some(dir) if dir.join("logs/HEAD").exists() => Head::BranchDeleted { name },
            Some(_) => Head::Unborn { name },
            None => {
                // Without the per-worktree git dir the two states cannot be told
                // apart, and calling it unborn would hide a real problem.
                problems.push(format!(
                    "cannot tell whether {name} is unborn or was deleted: the per-worktree git \
                     directory of {} could not be read",
                    path.display()
                ));
                Head::Unborn { name }
            }
        }
    }

    /// The commits in the window, and the churn they add up to.
    fn commits(
        &self,
        path: &Path,
        window: &Window,
        problems: &mut Vec<String>,
    ) -> (Vec<Commit>, Churn) {
        let since = format!("--since-as-filter=@{}", window.since.epoch);
        let pruning_since = format!("--max-age={}", window.since.epoch);
        let until = window
            .until
            .as_ref()
            .map(|stamp: &Stamp| format!("--min-age={}", stamp.epoch));

        let mut args: Vec<&str> = vec!["log", &since];
        if let Some(until) = until.as_deref() {
            args.push(until);
        }
        args.extend_from_slice(&["--numstat", "-z", "--no-renames", LOG_FORMAT]);

        let Some(mut out) = self.capture(path, &args, problems) else {
            return (Vec::new(), Churn::default());
        };

        // `--since-as-filter` arrived in git 2.37. On anything older, fall back
        // to the pruning `--max-age` and say so: the numbers are then a lower
        // bound, because a skewed committer timestamp can stop the walk early.
        if !out.ok() && out.stderr_text().contains("since-as-filter") {
            problems.push(format!(
                "{} does not support --since-as-filter; falling back to --max-age, which stops \
                 walking at the first commit older than the window and can miss commits behind a \
                 skewed committer timestamp",
                self.program.display()
            ));
            args[1] = &pruning_since;
            let Some(fallback) = self.capture(path, &args, problems) else {
                return (Vec::new(), Churn::default());
            };
            out = fallback;
        }

        if !out.ok() {
            let stderr = out.stderr_text();
            // A branch with no commits yet. Reached when HEAD is a deleted
            // branch rather than an unborn one, which the caller cannot know in
            // advance.
            if stderr.contains("does not have any commits yet") {
                return (Vec::new(), Churn::default());
            }
            problems.push(format!(
                "could not read the log of {}: {stderr}",
                path.display()
            ));
            return (Vec::new(), Churn::default());
        }

        parse_log(&out.stdout)
    }

    /// Uncommitted work. Counts come from `status --porcelain=v2`, line volume
    /// from `diff --shortstat`, because `status` does not count lines and
    /// `diff` does not see untracked files.
    fn dirty(&self, path: &Path, problems: &mut Vec<String>) -> Dirty {
        let mut dirty = Dirty::default();

        if let Some(out) = self.capture(
            path,
            &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
            problems,
        ) {
            if out.ok() {
                let fields: Vec<&[u8]> = out.stdout.split(|byte| *byte == 0).collect();
                let mut index = 0;
                while index < fields.len() {
                    let field = fields[index];
                    index += 1;
                    let Some(kind) = field.first() else {
                        continue;
                    };
                    match kind {
                        b'1' => dirty.tracked_changed += 1,
                        // A rename/copy record consumes **two** NUL-terminated
                        // fields: the new path, then the original. Reading the
                        // second one as a record is the classic `-z` parsing bug
                        // — an original path beginning with `1` would be counted
                        // as another changed file.
                        b'2' => {
                            dirty.tracked_changed += 1;
                            index += 1;
                        }
                        b'u' => dirty.conflicted += 1,
                        b'?' => dirty.untracked += 1,
                        // `#` headers and `!` ignored entries are neither.
                        _ => {}
                    }
                }
            } else {
                problems.push(format!(
                    "could not read the status of {}: {}",
                    path.display(),
                    out.stderr_text()
                ));
            }
        }

        for args in [
            ["diff", "--shortstat"].as_slice(),
            ["diff", "--cached", "--shortstat"].as_slice(),
        ] {
            if let Some(out) = self.capture(path, args, problems) {
                if out.ok() {
                    let (insertions, deletions) = parse_shortstat(&out.stdout_text());
                    dirty.insertions += insertions;
                    dirty.deletions += deletions;
                } else {
                    problems.push(format!(
                        "could not measure uncommitted changes in {}: {}",
                        path.display(),
                        out.stderr_text()
                    ));
                }
            }
        }

        dirty
    }

    /// Upstream tracking for the checked-out branch.
    ///
    /// `%(upstream:track)` is deliberately not used: it is prose ("ahead 2,
    /// behind 1", or "gone"), localised and reformatted between versions.
    fn tracking(&self, path: &Path, head: &Head, problems: &mut Vec<String>) -> Tracking {
        let Head::Branch { name, .. } = head else {
            return Tracking::NotApplicable;
        };

        let upstream = self
            .capture(
                path,
                &[
                    "for-each-ref",
                    "--format=%(upstream:short)",
                    &format!("refs/heads/{name}"),
                ],
                problems,
            )
            .filter(GitOut::ok)
            .map(|out| out.stdout_text())
            .unwrap_or_default();
        if upstream.is_empty() {
            return Tracking::NoUpstream;
        }

        // `%(upstream:short)` keeps printing the configured name after the
        // remote-tracking ref is deleted — the usual state after a merged branch
        // is cleaned up on the forge. That is a different answer from "nothing
        // is watching this branch", so it gets its own variant.
        let resolves = self
            .capture(
                path,
                &[
                    "rev-parse",
                    "--verify",
                    "-q",
                    &format!("{upstream}^{{commit}}"),
                ],
                problems,
            )
            .map(|out| out.ok())
            .unwrap_or(false);
        if !resolves {
            return Tracking::UpstreamMissing { name: upstream };
        }

        let counts = self.capture(
            path,
            &[
                "rev-list",
                "--left-right",
                "--count",
                &format!("{upstream}...HEAD"),
            ],
            problems,
        );
        match counts {
            Some(out) if out.ok() => {
                // Left is the upstream side (behind), right is HEAD (ahead).
                let text = out.stdout_text();
                let mut parts = text.split_whitespace();
                let behind = parts.next().and_then(|n| n.parse().ok());
                let ahead = parts.next().and_then(|n| n.parse().ok());
                match (behind, ahead) {
                    (Some(behind), Some(ahead)) => Tracking::Upstream {
                        name: upstream,
                        ahead,
                        behind,
                    },
                    _ => {
                        problems.push(format!(
                            "could not read the ahead/behind counts against {upstream}: {text:?}"
                        ));
                        Tracking::UpstreamMissing { name: upstream }
                    }
                }
            }
            Some(out) => {
                problems.push(format!(
                    "could not compare {name} with {upstream}: {}",
                    out.stderr_text()
                ));
                Tracking::UpstreamMissing { name: upstream }
            }
            None => Tracking::UpstreamMissing { name: upstream },
        }
    }

    /// Whether the work has landed on the default branch.
    ///
    /// Never a bare `NotMerged` when the question could not be asked: "we could
    /// not find a default branch" and "this did not land" are opposite messages
    /// to the person reading the digest.
    fn landed(&self, path: &Path, head: &Head, problems: &mut Vec<String>) -> Landed {
        let Some(default) = self.default_branch(path, problems) else {
            return Landed::Unknown {
                reason: "no default branch found: no refs/remotes/origin/HEAD, and none of the \
                         usual names resolve"
                    .to_string(),
            };
        };

        if let Some(branch) = head.branch_name() {
            // `origin/main` and the local `main` are the same trunk for this
            // question; a checkout of it has not "merged into" anything.
            let same = branch == default
                || default
                    .strip_prefix("origin/")
                    .map(|short| short == branch)
                    .unwrap_or(false);
            if same {
                return Landed::IsDefault { name: default };
            }
        }

        let Some(oid) = head.oid().filter(|oid| !oid.is_empty()) else {
            return Landed::Unknown {
                reason: format!("HEAD has no commit, so nothing can have landed on {default}"),
            };
        };

        let Some(out) = self.capture(
            path,
            &["merge-base", "--is-ancestor", oid, default.as_str()],
            problems,
        ) else {
            return Landed::Unknown {
                reason: format!("could not compare HEAD with {default}"),
            };
        };
        match out.code {
            Some(0) => Landed::Merged { into: default },
            Some(1) => Landed::NotMerged { into: default },
            other => Landed::Unknown {
                reason: format!(
                    "git merge-base --is-ancestor exited {} against {default}: {}",
                    other
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "on a signal".into()),
                    out.stderr_text()
                ),
            },
        }
    }

    /// The repository's default branch, as a resolvable ref name.
    fn default_branch(&self, path: &Path, problems: &mut Vec<String>) -> Option<String> {
        let pointed = self
            .capture(
                path,
                &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
                problems,
            )
            .filter(GitOut::ok)
            .map(|out| out.stdout_text())
            .filter(|name| !name.is_empty());
        if let Some(name) = pointed {
            if self.resolves(path, &name, problems) {
                return Some(name);
            }
        }
        for candidate in DEFAULT_BRANCH_CANDIDATES.iter().copied() {
            if self.resolves(path, candidate, problems) {
                return Some(candidate.to_string());
            }
        }
        None
    }

    fn resolves(&self, path: &Path, reference: &str, problems: &mut Vec<String>) -> bool {
        self.capture(
            path,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("{reference}^{{commit}}"),
            ],
            problems,
        )
        .map(|out| out.ok())
        .unwrap_or(false)
    }

    // -----------------------------------------------------------------------
    // Invocation
    // -----------------------------------------------------------------------

    /// Runs git for [`Git::report`], turning the two outcomes that are never
    /// ordinary data — the binary could not be spawned, or it hung — into
    /// problems on the report. A non-zero exit is handed back for the caller to
    /// interpret, because for most of these commands it is an answer.
    fn capture(&self, dir: &Path, args: &[&str], problems: &mut Vec<String>) -> Option<GitOut> {
        match self.run(dir, args) {
            Ok(out) if out.timed_out => {
                problems.push(format!(
                    "git {} timed out after {:?} in {}",
                    args.first().copied().unwrap_or("?"),
                    self.timeout,
                    dir.display()
                ));
                None
            }
            Ok(out) => Some(out),
            Err(err) => {
                problems.push(err.to_string());
                None
            }
        }
    }

    /// Runs a command that is expected to succeed, naming it on any other
    /// outcome.
    fn run_ok(&self, dir: &Path, args: &[&str]) -> Result<GitOut> {
        let out = self.run(dir, args)?;
        if out.timed_out {
            return Err(format!(
                "git {} timed out after {:?} in {}",
                args.join(" "),
                self.timeout,
                dir.display()
            )
            .into());
        }
        if !out.ok() {
            return Err(format!(
                "git {} failed in {}: {}",
                args.join(" "),
                dir.display(),
                out.stderr_text()
            )
            .into());
        }
        Ok(out)
    }

    /// One invocation, with a hard deadline.
    ///
    /// A hung git — a stuck credential helper, a stalled network filesystem, an
    /// fsmonitor that never answers — must not hang the digest, so the child is
    /// polled and killed on expiry. The pipes are drained on their own threads:
    /// a child that fills the 64 KiB pipe buffer blocks forever otherwise, and
    /// `log --numstat` over a busy day comfortably exceeds that.
    fn run(&self, dir: &Path, args: &[&str]) -> Result<GitOut> {
        let mut command = Command::new(&self.program);
        command.arg("-C").arg(dir);
        // Global, before the subcommand: plain `status` takes `index.lock` to
        // write back its stat cache, and this plugin promises never to write.
        command.arg("--no-optional-locks");
        command.args(args);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        // Never inherit a caller's git environment. standup can be launched from
        // a hook or a herdr action, where GIT_DIR or GIT_INDEX_FILE would
        // silently retarget every command at the wrong repository — and
        // GIT_INDEX_FILE pointed at a real index is exactly what this module
        // must never touch.
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
            "GIT_LITERAL_PATHSPECS",
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
        ] {
            command.env_remove(key);
        }
        command.env("GIT_OPTIONAL_LOCKS", "0");
        // A plugin has no terminal to prompt on; without this a repository with
        // an http remote can block forever asking for a password.
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env("GIT_PAGER", "cat");
        // Everything parsed here is machine output, and a localised git would
        // translate the words this module keys off.
        command.env("LC_ALL", "C");

        let mut child = command.spawn().map_err(|err| {
            format!(
                "could not run {} in {}: {err}",
                self.program.display(),
                dir.display()
            )
        })?;

        let mut out_pipe = child.stdout.take().expect("stdout is piped");
        let mut err_pipe = child.stderr.take().expect("stderr is piped");
        let out_reader = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = out_pipe.read_to_end(&mut buffer);
            buffer
        });
        let err_reader = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = err_pipe.read_to_end(&mut buffer);
            buffer
        });

        let deadline = Instant::now() + self.timeout;
        let mut backoff = Duration::from_micros(200);
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        timed_out = true;
                        let _ = child.kill();
                        break child.wait().map_err(|err| err.to_string())?;
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(5));
                }
                Err(err) => return Err(format!("waiting for git: {err}").into()),
            }
        };

        Ok(GitOut {
            code: status.code(),
            stdout: out_reader.join().unwrap_or_default(),
            stderr: err_reader.join().unwrap_or_default(),
            timed_out,
        })
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Splits the `-z` log stream into commits, and sums the churn.
///
/// The stream is NUL-delimited fields. A field beginning with [`RECORD`] is a
/// commit header; every other non-empty field is one `--numstat` line, whose
/// first one carries the newline git puts between the header and the diff block.
/// Only the first byte of a field is ever consulted, so a subject containing a
/// record separator, a unit separator, a tab or a pipe cannot desynchronise it.
fn parse_log(bytes: &[u8]) -> (Vec<Commit>, Churn) {
    let mut commits: Vec<Commit> = Vec::new();
    let mut files_seen: Vec<String> = Vec::new();
    let mut churn = Churn::default();

    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            continue;
        }
        if field[0] == RECORD {
            if let Some(commit) = parse_header(&field[1..]) {
                commits.push(commit);
            }
            continue;
        }

        // The header record is followed by a newline before the first numstat
        // line. Paths can contain newlines too, which is why exactly one leading
        // byte is stripped rather than all whitespace.
        let line = match field.first() {
            Some(b'\n') => &field[1..],
            _ => field,
        };
        if line.is_empty() {
            continue;
        }
        let Some(commit) = commits.last_mut() else {
            continue;
        };
        let Some((added, deleted, path)) = parse_numstat(line) else {
            continue;
        };
        // A binary file prints `-` for both counts. It is still a file the
        // commit touched, so it counts toward the file total and adds nothing to
        // the lines.
        commit.insertions += added.unwrap_or(0);
        commit.deletions += deleted.unwrap_or(0);
        churn.insertions += added.unwrap_or(0);
        churn.deletions += deleted.unwrap_or(0);
        if !files_seen.contains(&path) {
            files_seen.push(path.clone());
        }
        commit.files.push(path);
    }

    // The union, not the sum: a file edited in five commits is one file changed.
    churn.files = files_seen.len();
    (commits, churn)
}

fn parse_header(field: &[u8]) -> Option<Commit> {
    let text = String::from_utf8_lossy(field);
    // Bounded so a subject containing a unit separator stays whole; the four
    // fields before it are machine-generated and contain none.
    let mut parts = text.splitn(5, UNIT);
    let oid = parts.next()?.to_string();
    let parents = parts.next()?;
    let author = parts.next()?.to_string();
    let committed: i64 = parts.next()?.trim().parse().ok()?;
    let subject = parts.next().unwrap_or_default().trim_end().to_string();

    Some(Commit {
        oid,
        author,
        committed: clock::stamp(committed),
        subject,
        // A merge carries no diffstat of its own, so it counts as a commit and
        // contributes nothing to churn. The parent count is the only reliable
        // signal; `--numstat` simply prints nothing for it.
        is_merge: parents.split_whitespace().count() > 1,
        insertions: 0,
        deletions: 0,
        files: Vec::new(),
    })
}

/// `<added>\t<deleted>\t<path>`, with `-` for both counts on a binary file.
/// `--no-renames` keeps the path column single-valued; without it a rename
/// prints a three-field form a naive parser mangles.
fn parse_numstat(line: &[u8]) -> Option<(Option<u64>, Option<u64>, String)> {
    let mut tabs = line.splitn(3, |byte| *byte == b'\t');
    let added = tabs.next()?;
    let deleted = tabs.next()?;
    let path = tabs.next()?;
    let count = |raw: &[u8]| -> Option<u64> { std::str::from_utf8(raw).ok()?.parse().ok() };
    Some((
        count(added),
        count(deleted),
        String::from_utf8_lossy(path).into_owned(),
    ))
}

/// ` 3 files changed, 12 insertions(+), 4 deletions(-)`, with either half
/// absent.
fn parse_shortstat(text: &str) -> (u64, u64) {
    let mut insertions = 0;
    let mut deletions = 0;
    let mut previous: Option<u64> = None;
    for token in text.split_whitespace() {
        if token.starts_with("insertion") {
            insertions = previous.unwrap_or(0);
        } else if token.starts_with("deletion") {
            deletions = previous.unwrap_or(0);
        }
        previous = token.trim_end_matches(',').parse().ok();
    }
    (insertions, deletions)
}

// ---------------------------------------------------------------------------
// Paths and the binary
// ---------------------------------------------------------------------------

/// Canonical form, falling back to the path itself. A path that cannot be
/// canonicalized — a worktree git has listed but that has since been removed —
/// is still worth reporting under the name git gave it.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(unix)]
fn bytes_to_path(raw: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(raw))
}

#[cfg(not(unix))]
fn bytes_to_path(raw: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(raw).into_owned())
}

/// Finds the git binary without a shell.
///
/// herdr runs plugin commands with a minimal `PATH`, so `Command::new("git")`
/// alone is a coin toss. `GIT` wins if it is set — it is how a user pins a
/// specific build — then `PATH`, then the two locations a Unix git is installed
/// in. The last resort is the bare name, so the failure is at least reported
/// with the same message shape as everything else.
fn resolve_program() -> PathBuf {
    if let Some(explicit) = std::env::var_os("GIT").filter(|value| !value.is_empty()) {
        return PathBuf::from(explicit);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join("git");
            if is_executable(&candidate) {
                return candidate;
            }
        }
    }
    for fallback in ["/usr/bin/git", "/usr/local/bin/git"] {
        let candidate = PathBuf::from(fallback);
        if is_executable(&candidate) {
            return candidate;
        }
    }
    PathBuf::from("git")
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_file_counts_as_a_file_and_no_lines() {
        let (added, deleted, path) = parse_numstat(b"-\t-\tblob.bin").unwrap();
        assert_eq!(added, None);
        assert_eq!(deleted, None);
        assert_eq!(path, "blob.bin");
    }

    #[test]
    fn a_numstat_path_may_contain_tabs_and_newlines() {
        let (added, deleted, path) = parse_numstat(b"3\t1\tweird\tname\nhere.txt").unwrap();
        assert_eq!(added, Some(3));
        assert_eq!(deleted, Some(1));
        assert_eq!(path, "weird\tname\nhere.txt");
    }

    #[test]
    fn shortstat_survives_either_half_being_absent() {
        assert_eq!(
            parse_shortstat(" 3 files changed, 12 insertions(+), 4 deletions(-)"),
            (12, 4)
        );
        assert_eq!(parse_shortstat(" 1 file changed, 1 insertion(+)"), (1, 0));
        assert_eq!(parse_shortstat(" 1 file changed, 2 deletions(-)"), (0, 2));
        assert_eq!(parse_shortstat(""), (0, 0));
    }

    /// The framing claim, in isolation: a subject containing the record and unit
    /// separators must not split a commit in two or swallow the next one.
    #[test]
    fn a_subject_containing_the_separators_does_not_desynchronise_the_parser() {
        let mut stream: Vec<u8> = Vec::new();
        stream.push(RECORD);
        stream.extend_from_slice(
            "aaaa\u{1f}\u{1f}agent\u{1f}1700000000\u{1f}subject with \u{1e} and \u{1f} in it"
                .as_bytes(),
        );
        stream.push(0);
        stream.extend_from_slice(b"\n2\t1\ta.txt");
        stream.push(0);
        stream.push(RECORD);
        stream.extend_from_slice("bbbb\u{1f}aaaa\u{1f}agent\u{1f}1700000100\u{1f}next".as_bytes());
        stream.push(0);

        let (commits, churn) = parse_log(&stream);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "subject with \u{1e} and \u{1f} in it");
        assert_eq!(commits[0].files, vec!["a.txt".to_string()]);
        assert_eq!(commits[1].subject, "next");
        assert_eq!(churn.files, 1);
        assert_eq!((churn.insertions, churn.deletions), (2, 1));
    }

    #[test]
    fn churn_counts_the_union_of_paths_not_the_sum_of_file_counts() {
        let mut stream: Vec<u8> = Vec::new();
        for (index, oid) in ["aaaa", "bbbb", "cccc"].iter().enumerate() {
            stream.push(RECORD);
            stream.extend_from_slice(
                format!("{oid}\u{1f}pppp\u{1f}agent\u{1f}17000000{index:02}\u{1f}edit").as_bytes(),
            );
            stream.push(0);
            stream.extend_from_slice(b"\n1\t1\tshared.txt");
            stream.push(0);
        }
        let (commits, churn) = parse_log(&stream);
        assert_eq!(commits.len(), 3);
        assert_eq!(churn.files, 1, "one file edited three times is one file");
        assert_eq!((churn.insertions, churn.deletions), (3, 3));
    }

    #[test]
    fn the_git_binary_is_resolved_to_something_runnable() {
        let git = Git::new(Duration::from_secs(5));
        assert!(
            git.program().is_absolute() || git.program() == Path::new("git"),
            "unexpected git path {}",
            git.program().display()
        );
    }
}
