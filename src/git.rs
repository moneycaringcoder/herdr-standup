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
//! - runs with `GIT_OPTIONAL_LOCKS=0`;
//! - runs with `GIT_NO_LAZY_FETCH=1`, so a partial clone is never filled in
//!   from its promisor remote — see below;
//! - runs the two `diff --shortstat` invocations with `GIT_INDEX_FILE` pointed
//!   at a throwaway **copy** of the worktree's index, never at the real one.
//!
//! That last point is not belt and braces, and it took a live reproduction to
//! find. **`--no-optional-locks` does not cover `diff`.** `status` writes its
//! refreshed stat cache back *optionally*, and the flag suppresses it; `diff`
//! refreshes the index as part of doing its job, and neither
//! `--no-optional-locks` nor `GIT_OPTIONAL_LOCKS=0` stops it. Verified on git
//! 2.53.0 against a checkout with one tracked file rewritten with identical
//! bytes — an ordinary editor save is enough to trigger it:
//!
//! ```text
//! baseline index                                             791c22ab…
//! GIT_OPTIONAL_LOCKS=0 git --no-optional-locks status …      791c22ab…  unchanged
//! GIT_OPTIONAL_LOCKS=0 git --no-optional-locks diff --shortstat
//!                                                            cb788797…  REWRITTEN
//! the same diff with GIT_INDEX_FILE on a copy                791c22ab…  unchanged
//! ```
//!
//! It also takes `index.lock`, so without the copy this plugin would contend
//! with an agent running `git add` in the same checkout — the exact concurrency
//! it advertises as safe. See [`ScratchIndex`].
//!
//! `tests/read_only.rs` fingerprints the index, working tree, refs and loose
//! object count of a fixture repository before and after a full run and fails on
//! any difference. That test is the contract; this module is its implementation.
//! It only has teeth against a **stale** stat cache, because a fresh one gives
//! git nothing to write back — which is exactly how this bug survived the first
//! version of that test.
//!
//! # The partial clone
//!
//! A `--filter=blob:none` clone holds the commits and the trees and almost none
//! of the blobs. A diff needs blobs, and git's answer to a missing one is to
//! fetch it from the promisor remote and write it into `.git/objects` — a write
//! to the user's repository and a network call, from the two invocations that
//! have always wanted a diff: `log --numstat` for the churn, and `log -p` for
//! the landing probes. Measured on git 2.53.0 against a real blobless clone,
//! one `log --numstat` over the history took the object store from **8 files to
//! 24**, silently.
//!
//! `GIT_NO_LAZY_FETCH=1` refuses the fetch. git then exits 128 and names the
//! object it would have had to fetch, which is reported rather than papered
//! over. Measured on the same clone, with the same invocations: zero objects
//! written.
//!
//! The cost is real and is the point. On a partial clone the line counts and
//! the merge status are **unavailable**, where before they were available and
//! paid for with a write:
//!
//! - the churn: [`Git::commits`] reads the log again without `--numstat`, so
//!   the commits themselves survive with a problem beside them naming the
//!   object. A missing blob must not cost a reader the commit list.
//! - the squash and rebase probes: `log -p` over the trunk range fails, and
//!   [`Landed::Unknown`] carries git's own words. "I could not find out" is a
//!   different message from "this did not land", and this is exactly the case
//!   that must not be allowed to collapse into the second one.
//!
//! The rest of the report survives: `status`, both `diff --shortstat`
//! invocations, `merge-base` and `rev-list` ask only about the checkout's own
//! commit and index, and were measured writing nothing. `git cherry` hashes
//! patches and so can want a blob that is not there; when it does it is a probe
//! failure like any other, and lands as [`Landed::Unknown`] rather than as an
//! answer.
//!
//! ## Below git 2.37 the variable is not enough
//!
//! `GIT_NO_LAZY_FETCH` arrived in git 2.37 — the same release as
//! `--since-as-filter`. An older git ignores it outright: measured on 2.36.6
//! against the same blobless clone, one `log --numstat` with the variable set
//! wrote **nine object files**. A promise that depends on the reader's git
//! being new enough is not a promise, so on such a git the diff is not asked
//! for at all. [`Git::unrefusable_promisor`] answers whether that applies —
//! partial clone *and* a git that cannot be told no — and [`Git::commits`] then
//! omits `--numstat`, while [`Git::landing`] returns [`Landed::Unknown`]
//! without running a probe. The reader is told which remote was declined and
//! why, rather than being given a number that cost them a fetch.
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

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::cache::Cache;
use crate::clock;
use crate::config::{Ignored, DEFAULT_BRANCH_CANDIDATES};
use crate::model::{
    CheckoutReport, Churn, Commit, Dirty, Equivalence, Head, Landed, RepoKey, Stamp, Tracking,
    Unpushed, Window,
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
/// `today` is here because of a git version difference rather than in spite of
/// one. Through git 2.54 it resolves to **now**, not to midnight, so rejecting
/// it would refuse a spec git accepts; git 2.55 changed it to mean the local
/// midnight, at which point it stops landing on now and never reaches the check
/// this list guards. Measured on 2.53.0 and 2.54.0 (now) and on 2.55.0
/// (midnight). Listing it is correct on both, and it must stay listed for as
/// long as anyone runs a git older than 2.55.
///
/// `midnight` is the spelling that means the start of the local day on every
/// version, and is this plugin's default window.
const SPECS_MEANING_NOW: &[&str] = &["now", "today"];

/// How close to the current instant a resolved date has to be before it is
/// treated as git's silent "I could not parse that" answer.
const NOW_SLACK_SECONDS: i64 = 2;

/// The diff options both sides of a patch-id comparison must be produced with.
///
/// Not a matter of taste. `git patch-id` hashes the diff it is handed, so two
/// diffs of the same change hash differently when they were produced with
/// different options — and the two commands compared here do not read the same
/// configuration: `diff-tree` is plumbing and takes git's *basic* diff config,
/// while `log` is porcelain and takes the *UI* config on top of it.
///
/// Measured on git 2.53.0, replaying these invocations against a three-commit
/// branch squash-merged onto a moved-on trunk. With an empty global config the
/// squash is found; with any one of `diff.noprefix = true`, `diff.context = 5`,
/// or a custom `diff.srcPrefix`/`diff.dstPrefix` in the **reader's own**
/// `~/.gitconfig`, only the `log` side moved and every squash merge on that
/// machine went back to reading as "not merged" — the exact bug, restored
/// silently by a setting that has nothing to do with merging.
///
/// So each is pinned rather than inherited:
///
/// - `-U3`, `--src-prefix`, `--dst-prefix` against `diff.context`,
///   `diff.noprefix`, `diff.srcPrefix`, `diff.dstPrefix` and
///   `diff.mnemonicPrefix`, which only the porcelain side honours.
/// - `--no-renames`, because rename detection rewrites the headers the id is
///   built from, and `diff.renames` is on by default in porcelain. It also
///   keeps these diffs consistent with the `log --numstat` above.
/// - `--no-textconv`, for symmetry, and so nothing named in a `.gitattributes`
///   is executed on the way past.
const PATCH_ID_DIFF_OPTIONS: &[&str] = &[
    "-U3",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "--no-renames",
    "--no-textconv",
];

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

/// The repository's default branch: the name a digest reports and the commit it
/// currently points at.
///
/// The two travel together because the answer needs the name and the cache key
/// needs the sha — a trunk that moved must not be served yesterday's verdict,
/// and the sha is the only thing that says it moved.
struct Trunk {
    name: String,
    oid: String,
}

/// A git runner. Holds the resolved binary, the per-invocation timeout — so one
/// wedged repository cannot stall the whole digest — the paths whose lines are
/// not counted, and the cache for the landing probes.
///
/// The cache is in-memory unless a caller asks for the persistent one, which
/// keeps every test hermetic: a suite that read the developer's real state
/// directory would pass or fail depending on what they ran yesterday.
pub struct Git {
    program: PathBuf,
    timeout: Duration,
    ignored: Ignored,
    cache: Cache,
    /// Whether this git honours `GIT_NO_LAZY_FETCH`, resolved on first use and
    /// only in a partial clone. See [`Git::refuses_lazy_fetch`].
    refuses_lazy_fetch: OnceLock<bool>,
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

    /// Stderr as a single line.
    ///
    /// Everything this feeds is rendered as one clause — a problem note, or the
    /// reason attached to a merge status — and git's stderr is routinely several
    /// lines, a `warning:` followed by a `fatal:`. Left as-is, the newline
    /// escapes into the middle of a digest and breaks the layout of both
    /// renderers.
    fn stderr_text(&self) -> String {
        let text = String::from_utf8_lossy(&self.stderr);
        let mut words = text.split_whitespace();
        let mut line = words.next().unwrap_or_default().to_string();
        for word in words {
            line.push(' ');
            line.push_str(word);
        }
        line
    }

    /// Stderr as a reader sees it: [`GitOut::stderr_text`] with git's
    /// `lazy fetching disabled` preamble dropped when git went on to say
    /// something specific.
    ///
    /// Every refused promisor fetch carries that warning, so a reason built out
    /// of the raw text reads "warning: lazy fetching disabled; some objects may
    /// not be available fatal: could not fetch <oid> …" — the mechanism twice
    /// over, in front of the one clause that names the object. Detection still
    /// runs on the raw text, because a stderr that is *only* the warning has
    /// nothing else to identify it by.
    fn stderr_reported(&self) -> String {
        let text = self.stderr_text();
        refused_promisor_fetch(&text).unwrap_or(text)
    }
}

impl Git {
    /// herdr runs plugin commands with a minimal `PATH` and no shell, so the
    /// binary is resolved explicitly rather than assumed.
    pub fn new(timeout: Duration) -> Self {
        Self {
            program: resolve_program(),
            timeout,
            ignored: Ignored::default(),
            cache: Cache::in_memory(),
            refuses_lazy_fetch: OnceLock::new(),
        }
    }

    /// Replaces the in-memory cache with a persistent one. `standup.rs` passes
    /// the real one; tests pass a cache in a temp directory or none at all.
    pub fn caching(mut self, cache: Cache) -> Self {
        self.cache = cache;
        self
    }

    /// Writes the cache back. Best-effort and silent, and called once the digest
    /// is collected rather than per checkout, so twenty worktrees cost one write.
    pub fn save_cache(&self) {
        self.cache.save();
    }

    /// Replaces the paths whose lines are not counted. The default is
    /// [`crate::config::DEFAULT_IGNORED_PATHS`]; `standup.rs` overrides it with
    /// whatever the reader configured, including nothing at all.
    pub fn ignoring(mut self, ignored: Ignored) -> Self {
        self.ignored = ignored;
        self
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

        // Asked once per checkout, and only ever answered `Some` on a git too
        // old to be told not to fetch. Everything downstream that would produce
        // a diff then declines to run rather than pulling blobs into someone's
        // repository; see the module note on the partial clone.
        let promisor = self.unrefusable_promisor(&path, &mut problems);
        let promisor = promisor.as_deref();

        let head = self.head(&path, git_dir.as_deref(), &mut problems);
        let (commits, churn) = match head {
            // An unborn branch has no history to walk, and `git log` refuses
            // outright. That is the expected state, not a problem.
            Head::Unborn { .. } => (Vec::new(), Churn::default()),
            _ => self.commits(&path, window, promisor, &mut problems),
        };
        let dirty = self.dirty(&path, git_dir.as_deref(), &mut problems);
        let tracking = self.tracking(&path, &head, &mut problems);
        let landed = self.landed(&path, &head, promisor, &mut problems);
        let unpushed = self.unpushed(&path, &head, &mut problems);

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
            unpushed,
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
    ///
    /// # Why there is no "refuse if this is inside a repository" guard
    ///
    /// An earlier version refused when the parent directory was inside a git
    /// repository, reasoning that the plugin should never create anything in a
    /// user's checkout. That guard was wrong, and it broke the plugin outright
    /// on a machine where it matters most: a home directory kept in git —
    /// ordinary dotfiles — puts the default state directory
    /// (`~/.local/state/herdr/plugins/<id>/`) inside a repository, so every run
    /// with no usable checkout to anchor date parsing died with
    /// `refusing to create the date-reference repository`. Confirmed on the
    /// development machine, whose `$HOME` is a checkout.
    ///
    /// The guard also protected very little. `git init --bare` writes only
    /// inside the directory it creates; it stages nothing, touches no index and
    /// moves no ref in the enclosing repository. What actually matters is the
    /// precision below: an enclosing repository must never be *mistaken* for
    /// this one, which is why the idempotence check compares the resolved git
    /// directory with the target path instead of merely asking "is this a
    /// repository".
    pub fn ensure_date_ref_repo(&self, path: &Path) -> Result<()> {
        // Idempotent, but only for a repository that really is *at* this path.
        // A bare `rev-parse --git-dir` succeeds from anywhere inside an
        // enclosing checkout, and treating that as "already created" would
        // silently resolve every window inside the user's own repository.
        if let Ok(out) = self.run(path, &["rev-parse", "--path-format=absolute", "--git-dir"]) {
            if out.ok() && canonical(Path::new(&out.stdout_text())) == canonical(path) {
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
                out.stderr_reported()
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
    ///
    /// `promisor` names the remote a partial clone would be filled in from when
    /// this git cannot be told not to; the `--numstat` is then not asked for at
    /// all, since asking is what triggers the fetch.
    fn commits(
        &self,
        path: &Path,
        window: &Window,
        promisor: Option<&str>,
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
        if let Some(promisor) = promisor {
            problems.push(format!(
                "could not count the lines changed in {}: this is a partial clone of {promisor}, \
                 and {} predates GIT_NO_LAZY_FETCH (git 2.37), so it cannot be told to refuse the \
                 fetch the diff would need. The diff is therefore not run at all, rather than \
                 filling in your repository from its remote. The commits are reported in full; \
                 their line counts read as zero.",
                path.display(),
                self.program.display()
            ));
        } else {
            args.push("--numstat");
        }
        args.extend_from_slice(&["-z", "--no-renames", LOG_FORMAT]);

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

        // A partial clone has none of the blobs a `--numstat` needs, and
        // refusing to fetch them (see the module note) fails the *whole* log
        // rather than the numstat half of it. So read it again without
        // `--numstat`: commits and their subjects need no blobs, and a missing
        // line count must not be allowed to render a busy day as an empty one.
        let refusal = (!out.ok())
            .then(|| refused_promisor_fetch(&out.stderr_text()))
            .flatten();
        if let Some(refusal) = refusal {
            problems.push(format!(
                "could not count the lines changed in {}: {refusal}. This is a partial clone, so \
                 the blobs the diff needs are not in it, and fetching them would write to the \
                 repository and reach the network — which this plugin never does. The commits are \
                 reported in full; their line counts read as zero.",
                path.display()
            ));
            args.retain(|arg| *arg != "--numstat");
            let Some(without_numstat) = self.capture(path, &args, problems) else {
                return (Vec::new(), Churn::default());
            };
            out = without_numstat;
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
                "could not read the log of {}: {}",
                path.display(),
                out.stderr_reported()
            ));
            return (Vec::new(), Churn::default());
        }

        parse_log(&out.stdout, &self.ignored)
    }

    /// Uncommitted work. Counts come from `status --porcelain=v2`, line volume
    /// from `diff --shortstat`, because `status` does not count lines and
    /// `diff` does not see untracked files.
    fn dirty(&self, path: &Path, git_dir: Option<&Path>, problems: &mut Vec<String>) -> Dirty {
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
                    out.stderr_reported()
                ));
            }
        }

        // The line volume, and the one place in this module that has to protect
        // the index by hand. `diff` refreshes the index and writes it back, and
        // that refresh is **not** the optional kind: neither
        // `--no-optional-locks` nor `GIT_OPTIONAL_LOCKS=0` suppresses it. Both
        // diffs therefore run against a throwaway copy of the worktree's index,
        // which is also what keeps them from contending for `index.lock` with an
        // agent running `git add` in the same checkout.
        match ScratchIndex::of(git_dir) {
            // No index at all: a repository where nothing has ever been staged.
            // Both diffs are empty by construction, and running them against a
            // GIT_INDEX_FILE that does not exist would be worse than not running
            // them — an empty index makes `diff --cached` report every tracked
            // file as deleted.
            Ok(None) => {}
            Ok(Some(scratch)) => {
                let index_file = [("GIT_INDEX_FILE", scratch.path())];
                for args in [
                    ["diff", "--shortstat"].as_slice(),
                    ["diff", "--cached", "--shortstat"].as_slice(),
                ] {
                    if let Some(out) = self.capture_env(path, args, &index_file, problems) {
                        if out.ok() {
                            let (insertions, deletions) = parse_shortstat(&out.stdout_text());
                            dirty.insertions += insertions;
                            dirty.deletions += deletions;
                        } else {
                            problems.push(format!(
                                "could not measure uncommitted changes in {}: {}",
                                path.display(),
                                out.stderr_reported()
                            ));
                        }
                    }
                }
            }
            // Deliberately not a fallback to the real index. Measuring the line
            // volume is worth less than the promise that this plugin never
            // writes to a repository, so the counts are left at zero and the
            // reason is rendered.
            Err(err) => problems.push(format!(
                "could not measure uncommitted line counts in {}: {err}. The file counts above \
                 are still accurate; the line counts are not, because measuring them safely \
                 needs a scratch copy of the index and one could not be made.",
                path.display()
            )),
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
                    out.stderr_reported()
                ));
                Tracking::UpstreamMissing { name: upstream }
            }
            None => Tracking::UpstreamMissing { name: upstream },
        }
    }

    /// The commits that exist in this checkout and **nowhere else**.
    ///
    /// ```sh
    /// git -C <wt> --no-optional-locks remote
    /// git -C <wt> --no-optional-locks rev-list --count HEAD --not --remotes
    /// ```
    ///
    /// Reachable from HEAD, reachable from no remote-tracking ref. That is the
    /// question, rather than the `ahead` count [`Git::tracking`] already
    /// reports, for two reasons: `ahead` is measured against the **one**
    /// configured upstream, and it answers nothing at all when there is no
    /// upstream — which is precisely the branch whose every commit is only here.
    /// `--remotes` spans every remote, so work pushed to a fork or to a second
    /// remote is correctly not counted as at risk.
    ///
    /// The `remote` call first is not a formality. Without it, a repository with
    /// no remote configured counts its entire history as unpushed, which is
    /// literally true and useless: there was never anywhere to push it, and a
    /// digest that files every local-only scratch repository under "work at
    /// risk" buries the case this exists to surface. That state gets its own
    /// name instead.
    fn unpushed(&self, path: &Path, head: &Head, problems: &mut Vec<String>) -> Unpushed {
        // An unborn branch has no commits, so nothing can be unpushed, and
        // `rev-list` would fail on the missing HEAD. Not a fault worth naming.
        if head.oid().filter(|oid| !oid.is_empty()).is_none() {
            return Unpushed::Commits { count: 0 };
        }

        // A failure here is recorded as a problem as well as a state, which is
        // what `tracking` does with a dangling upstream and for the same reason:
        // a checkout whose at-risk count could not be read must not be filed as
        // quiet and summarised down to its repository name. `problems` is the
        // one field every renderer is obliged to show.

        match self.capture(path, &["remote"], problems) {
            Some(out) if out.ok() => {
                if out.stdout_text().is_empty() {
                    return Unpushed::NoRemote;
                }
            }
            Some(out) => {
                return unpushed_unknown(
                    format!(
                        "git remote exited {}: {}",
                        exit_label(out.code),
                        out.stderr_reported()
                    ),
                    problems,
                )
            }
            None => return unpushed_unknown("git remote could not be run".to_string(), problems),
        }

        let Some(out) = self.capture(
            path,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
            problems,
        ) else {
            return unpushed_unknown("git rev-list could not be run".to_string(), problems);
        };
        if !out.ok() {
            return unpushed_unknown(
                format!(
                    "git rev-list exited {}: {}",
                    exit_label(out.code),
                    out.stderr_reported()
                ),
                problems,
            );
        }
        let text = out.stdout_text();
        match text.parse() {
            Ok(count) => Unpushed::Commits { count },
            Err(err) => unpushed_unknown(
                format!("git rev-list --count answered {text:?}: {err}"),
                problems,
            ),
        }
    }

    /// Whether the work has landed on the default branch.
    ///
    /// Two questions, asked in that order. `merge-base --is-ancestor` answers
    /// the exact one — is this commit *on* the trunk — and is the whole answer
    /// for a fast-forward or a merge commit. It is the wrong question for a
    /// squash merge or a rebase merge, which rewrite the commit, so the sha
    /// that shipped is not the sha this checkout holds; see
    /// [`Git::equivalent_patch`] for the second question and why a bare
    /// `NotMerged` there reported shipped work as unlanded.
    ///
    /// Never a bare `NotMerged` when the question could not be asked: "we could
    /// not find a default branch" and "this did not land" are opposite messages
    /// to the person reading the digest.
    fn landed(
        &self,
        path: &Path,
        head: &Head,
        promisor: Option<&str>,
        problems: &mut Vec<String>,
    ) -> Landed {
        let Some(trunk) = self.default_branch(path, problems) else {
            return Landed::Unknown {
                reason: "no default branch found: no refs/remotes/origin/HEAD, and none of the \
                         usual names resolve"
                    .to_string(),
            };
        };
        let default = trunk.name.as_str();

        if let Some(branch) = head.branch_name() {
            // `origin/main` and the local `main` are the same trunk for this
            // question; a checkout of it has not "merged into" anything.
            let same = branch == default
                || default
                    .strip_prefix("origin/")
                    .map(|short| short == branch)
                    .unwrap_or(false);
            if same {
                return Landed::IsDefault {
                    name: default.to_string(),
                };
            }
        }

        let Some(oid) = head.oid().filter(|oid| !oid.is_empty()) else {
            return Landed::Unknown {
                reason: format!("HEAD has no commit, so nothing can have landed on {default}"),
            };
        };

        // Everything below is a pure function of these three, so this is where
        // the cache sits: one key, one answer, and a checkout that moved cannot
        // hit it. The `IsDefault` answer above is deliberately outside it —
        // it turns on the *branch name*, so a detached HEAD at the trunk's own
        // commit must not be served the answer computed for the branch.
        let key = Cache::answer_key(oid, default, &trunk.oid);
        if let Some(landed) = self.cache.answer(&key) {
            return landed;
        }
        let landed = self.landing(path, oid, trunk, promisor, problems);
        // A failure is not an answer and is never stored: git could not be run
        // this time, and caching that would make one bad minute permanent.
        if !matches!(landed, Landed::Unknown { .. }) {
            self.cache.remember_answer(key, landed.clone());
        }
        landed
    }

    /// The two questions behind [`Landed`], for a checkout that is not the trunk
    /// and has a commit. Split out so the cache above has exactly one thing to
    /// wrap, and so the expensive path is one call away from being read.
    fn landing(
        &self,
        path: &Path,
        oid: &str,
        trunk: Trunk,
        promisor: Option<&str>,
        problems: &mut Vec<String>,
    ) -> Landed {
        let Trunk {
            name: default,
            oid: trunk_oid,
        } = trunk;
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
            // Exit 1 is "not contained", which is not the same as "did not
            // land"; see `equivalent_patch`. A probe that could not be run is
            // not an answer either, and must not arrive as one.
            //
            // On a partial clone this git would fill in, the probes are not run
            // at all: every one of them wants a diff, and the objects would
            // arrive from the remote as the price of asking.
            Some(1) => match promisor {
                Some(promisor) => Landed::Unknown {
                    reason: format!(
                        "HEAD is not on {default} by sha, and every remaining probe needs a diff \
                         that would be paid for with a fetch: this is a partial clone of \
                         {promisor}, and {} predates GIT_NO_LAZY_FETCH (git 2.37), so the fetch \
                         cannot be refused, and a squash or rebase merge cannot be ruled out",
                        self.program.display()
                    ),
                },
                None => match self.equivalent_patch(path, oid, &default, &trunk_oid, problems) {
                    Ok(Some(how)) => Landed::Equivalent { into: default, how },
                    Ok(None) => Landed::NotMerged { into: default },
                    Err(reason) => Landed::Unknown {
                        reason: format!(
                            "HEAD is not on {default} by sha, and {reason}, so a squash or rebase \
                             merge cannot be ruled out"
                        ),
                    },
                },
            },
            other => Landed::Unknown {
                reason: format!(
                    "git merge-base --is-ancestor exited {} against {default}: {}",
                    exit_label(other),
                    out.stderr_reported()
                ),
            },
        }
    }

    /// Looks for this branch's work on the trunk under a different sha.
    ///
    /// Squash merging is the default on a great many forges, and a squash or a
    /// rebase merge rewrites the commit — so the sha this checkout holds never
    /// appears on the default branch and containment, which is exact, says the
    /// work did not land. It shipped.
    ///
    /// Two probes, because neither alone covers both shapes. Measured on git
    /// 2.53.0 against a three-commit branch squash-merged onto a trunk that had
    /// moved on since the fork point:
    ///
    /// ```text
    /// merge-base --is-ancestor            exit 1        (correct, and useless)
    /// git cherry main topic               + + +         no individual patch survived
    /// combined diff-tree | patch-id       450e3211…  ->  6df5ff43… on main
    /// ```
    ///
    /// A rebase merge is the mirror image: every commit keeps its own patch, so
    /// `git cherry` marks them all `-`, while the combined id matches nothing
    /// because the trunk range carries other people's commits too.
    ///
    /// Neither result is proof. Two commits with the same diff have the same
    /// patch id, so this is reported as [`Landed::Equivalent`] and never folded
    /// into [`Landed::Merged`].
    fn equivalent_patch(
        &self,
        path: &Path,
        oid: &str,
        default: &str,
        trunk_oid: &str,
        problems: &mut Vec<String>,
    ) -> std::result::Result<Option<Equivalence>, String> {
        // Both probes are relative to the fork point. Unrelated histories have
        // none, and `merge-base` says so with exit 1 — that is an answer, not a
        // failure: there is nothing there for the work to have landed into.
        let base = match self.capture(path, &["merge-base", oid, default], problems) {
            Some(out) if out.ok() => out.stdout_text(),
            Some(out) if out.code == Some(1) => return Ok(None),
            Some(out) => {
                return Err(format!(
                    "git merge-base exited {}: {}",
                    exit_label(out.code),
                    out.stderr_reported()
                ))
            }
            None => return Err("git merge-base could not be run".to_string()),
        };
        if base.is_empty() {
            return Ok(None);
        }

        // `git cherry <upstream> <head>` prints one line per commit on the
        // branch: `-` when an equivalent patch is already upstream, `+` when it
        // is not. Nothing but `-` means every commit arrived by another route.
        // Cheapest first: one process, and it is the command a reader re-runs.
        let cherry = self.probe(path, &["cherry", default, oid], problems)?;
        let mut commits = 0u64;
        let mut all_upstream = true;
        for line in cherry.lines().filter(|line| !line.trim().is_empty()) {
            commits += 1;
            all_upstream &= line.starts_with('-');
        }
        if commits > 0 && all_upstream {
            return Ok(Some(Equivalence::EveryCommit { commits }));
        }

        // A squash merge collapses the branch into one commit, so no individual
        // patch id survives it. What does survive is the id of the branch's
        // *combined* diff against the fork point, which is exactly the diff the
        // squash commit carries.
        let Some((wanted, _)) = self
            .patch_ids(path, &diff_args(&["diff-tree", &base, oid]), problems)?
            .into_iter()
            .next()
        else {
            // An empty diff between the fork point and HEAD cannot be the thing
            // a squash commit carries, and `cherry` has already spoken for the
            // individual patches.
            return Ok(None);
        };
        let range = format!("{base}..{default}");
        // The expensive one, and the reason this cache exists: `log -p` over the
        // trunk range is the whole cost of the probe, and its key holds for every
        // worktree branched from the same commit of a trunk that has not moved.
        let range_key = Cache::range_key(&base, trunk_oid);
        let trunk_ids = match self.cache.range(&range_key) {
            Some(ids) => ids,
            None => {
                let ids = self.patch_ids(
                    path,
                    &diff_args(&[
                        "log",
                        // A merge commit prints a header and no diff, which would
                        // hand the next commit's patch to the wrong sha.
                        "--no-merges",
                        // `patch-id` reads the sha off the `commit <sha>` line
                        // that git's default format happens to print, so a host
                        // `format.pretty` would otherwise take it away and leave
                        // every id attributed to nothing.
                        "--format=commit %H",
                        &range,
                    ]),
                    problems,
                )?;
                self.cache.remember_range(range_key, ids.clone());
                ids
            }
        };
        Ok(trunk_ids
            .into_iter()
            .find(|(id, _)| *id == wanted)
            .map(|(_, oid)| Equivalence::Squashed { oid }))
    }

    /// A probe whose exit status is not an answer: anything but success means
    /// the question could not be asked, which the caller must keep apart from
    /// "the patch is not there".
    fn probe(
        &self,
        path: &Path,
        args: &[&str],
        problems: &mut Vec<String>,
    ) -> std::result::Result<String, String> {
        let name = args.first().copied().unwrap_or("?");
        match self.capture(path, args, problems) {
            Some(out) if out.ok() => Ok(out.stdout_text()),
            Some(out) => Err(format!(
                "git {name} exited {}: {}",
                exit_label(out.code),
                out.stderr_reported()
            )),
            None => Err(format!("git {name} could not be run")),
        }
    }

    /// `<args> | git patch-id --stable`, as `(patch id, commit)` pairs.
    ///
    /// `--stable` rather than the default, because it hashes each file
    /// independently: the id then does not depend on the order git happened to
    /// emit the files in, and `diff-tree` and `log` need not agree on that.
    ///
    /// The diff is buffered and handed to `patch-id` on stdin rather than joined
    /// to it by a pipe, and the reason is the exit status. Buffered, the source's
    /// status is checked *before* the sink ever runs; piped, a source that died
    /// partway would leave `patch-id` exiting 0 over a truncated stream, and
    /// "the ids I found do not include yours" must never be mistaken for "the
    /// patch is not on the trunk" — the one distinction
    /// [`Git::equivalent_patch`] exists to keep. It also keeps the deadline and
    /// the pipe draining in [`Git::run_env`] instead of duplicating both. The
    /// buffer is moved, not copied: it is the source's own captured stdout.
    ///
    /// The cost of that buffer on a stale branch of a busy trunk is real and is
    /// filed separately; it is a question about speed, not about the answer.
    fn patch_ids(
        &self,
        dir: &Path,
        args: &[&str],
        problems: &mut Vec<String>,
    ) -> std::result::Result<Vec<(String, String)>, String> {
        let name = args.first().copied().unwrap_or("?");
        let diff = match self.capture(dir, args, problems) {
            Some(out) if out.ok() => out.stdout,
            Some(out) => {
                return Err(format!(
                    "git {name} exited {}: {}",
                    exit_label(out.code),
                    out.stderr_reported()
                ))
            }
            None => return Err(format!("git {name} could not be run")),
        };
        if diff.is_empty() {
            return Ok(Vec::new());
        }
        let ids = match self.capture_stdin(dir, &["patch-id", "--stable"], diff, problems) {
            Some(out) if out.ok() => out.stdout_text(),
            Some(out) => {
                return Err(format!(
                    "git patch-id exited {}: {}",
                    exit_label(out.code),
                    out.stderr_reported()
                ))
            }
            None => return Err("git patch-id could not be run".to_string()),
        };
        Ok(ids
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                // Both fields or neither. The sha is what makes the verdict
                // checkable by hand, and `patch-id` always prints one, so a
                // line without it is not a shape to accommodate.
                Some((fields.next()?.to_string(), fields.next()?.to_string()))
            })
            .collect())
    }

    /// The repository's default branch, as a resolvable ref name.
    fn default_branch(&self, path: &Path, problems: &mut Vec<String>) -> Option<Trunk> {
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
            if let Some(oid) = self.resolves(path, &name, problems) {
                return Some(Trunk { name, oid });
            }
        }
        for candidate in DEFAULT_BRANCH_CANDIDATES.iter().copied() {
            if let Some(oid) = self.resolves(path, candidate, problems) {
                return Some(Trunk {
                    name: candidate.to_string(),
                    oid,
                });
            }
        }
        None
    }

    /// The commit a ref names, or `None` when it does not resolve.
    ///
    /// The sha is the point: it is what makes the trunk's position part of the
    /// cache key, and `rev-parse --verify` was already being run for the yes/no
    /// answer, so nothing extra is spawned to learn it.
    fn resolves(&self, path: &Path, reference: &str, problems: &mut Vec<String>) -> Option<String> {
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
        .filter(GitOut::ok)
        .map(|out| out.stdout_text())
        .filter(|oid| !oid.is_empty())
    }

    /// The promisor remote this checkout would be filled in from behind our
    /// back, if it has one this git cannot be stopped from using.
    ///
    /// `None` in the two ordinary cases — the repository is not a partial clone,
    /// or the git in front of us honours `GIT_NO_LAZY_FETCH` — and it is
    /// deliberately the second question first, so a modern git pays nothing at
    /// all: no extra invocation per checkout, and no `config` read.
    ///
    /// Both spellings are asked for at once, because git records the fact twice
    /// and a clone need not have both: `extensions.partialclone` is the
    /// repository-format extension, and `remote.<name>.promisor` is the per-remote
    /// flag. Measured on git 2.53.0, a `clone --filter=blob:none` writes the
    /// second and not the first. `--get-regexp` exits 1 when nothing matches,
    /// which is the answer rather than a fault.
    fn unrefusable_promisor(&self, path: &Path, problems: &mut Vec<String>) -> Option<String> {
        if self.refuses_lazy_fetch(path) {
            return None;
        }
        let out = self
            .capture(
                path,
                &[
                    "config",
                    "--get-regexp",
                    r"^(extensions\.partialclone|remote\..*\.promisor)$",
                ],
                problems,
            )
            .filter(GitOut::ok)?;
        out.stdout_text().lines().find_map(promisor_name)
    }

    /// Whether this git honours `GIT_NO_LAZY_FETCH`, which arrived in **git
    /// 2.37** — the same release as `--since-as-filter`.
    ///
    /// Measured against one blobless clone: on 2.53.0 the variable refuses the
    /// fetch and one `log --numstat` writes nothing; on 2.36.6 it is ignored
    /// outright and the same command wrote nine object files. So the variable
    /// alone is not the guarantee — below 2.37 the diff has to not be asked for.
    ///
    /// Resolved once per run — one `git version` for a whole session, cached
    /// here — and a version this cannot parse counts as **old**, because the two
    /// ways of being wrong are not symmetrical: one costs a line count, the
    /// other writes to somebody's repository.
    fn refuses_lazy_fetch(&self, path: &Path) -> bool {
        *self.refuses_lazy_fetch.get_or_init(|| {
            self.run(path, &["version"])
                .ok()
                .filter(GitOut::ok)
                .and_then(|out| parse_version(&out.stdout_text()))
                .map(|version| version >= (2, 37))
                .unwrap_or(false)
        })
    }

    // -----------------------------------------------------------------------
    // Invocation
    // -----------------------------------------------------------------------

    /// Runs git for [`Git::report`], turning the two outcomes that are never
    /// ordinary data — the binary could not be spawned, or it hung — into
    /// problems on the report. A non-zero exit is handed back for the caller to
    /// interpret, because for most of these commands it is an answer.
    fn capture(&self, dir: &Path, args: &[&str], problems: &mut Vec<String>) -> Option<GitOut> {
        self.capture_full(dir, args, &[], None, problems)
    }

    /// [`Git::capture`] with extra environment, used only to point
    /// `GIT_INDEX_FILE` at a scratch copy for the two `diff` invocations.
    fn capture_env(
        &self,
        dir: &Path,
        args: &[&str],
        extra_env: &[(&str, &Path)],
        problems: &mut Vec<String>,
    ) -> Option<GitOut> {
        self.capture_full(dir, args, extra_env, None, problems)
    }

    /// [`Git::capture`] with a diff on stdin, used only to feed
    /// [`Git::patch_ids`].
    fn capture_stdin(
        &self,
        dir: &Path,
        args: &[&str],
        stdin: Vec<u8>,
        problems: &mut Vec<String>,
    ) -> Option<GitOut> {
        self.capture_full(dir, args, &[], Some(stdin), problems)
    }

    fn capture_full(
        &self,
        dir: &Path,
        args: &[&str],
        extra_env: &[(&str, &Path)],
        stdin: Option<Vec<u8>>,
        problems: &mut Vec<String>,
    ) -> Option<GitOut> {
        match self.run_full(dir, args, extra_env, stdin) {
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
                out.stderr_reported()
            )
            .into());
        }
        Ok(out)
    }

    /// One invocation, with a hard deadline.
    ///
    /// A hung git — a stuck credential helper, a stalled network filesystem, an
    /// fsmonitor that never answers — must not hang the digest, so the child is
    /// polled and its dedicated process group is killed on expiry. The pipes are
    /// drained on their own threads: a child that fills the 64 KiB pipe buffer
    /// blocks forever otherwise, and `log --numstat` over a busy day comfortably
    /// exceeds that.
    fn run(&self, dir: &Path, args: &[&str]) -> Result<GitOut> {
        self.run_full(dir, args, &[], None)
    }

    /// [`Git::run`], with environment this module sets on purpose applied
    /// *after* the inherited environment is scrubbed, and an optional payload
    /// on the child's stdin.
    fn run_full(
        &self,
        dir: &Path,
        args: &[&str],
        extra_env: &[(&str, &Path)],
        stdin: Option<Vec<u8>>,
    ) -> Result<GitOut> {
        let mut command = Command::new(&self.program);
        command.arg("-C").arg(dir);
        // Global, before the subcommand: plain `status` takes `index.lock` to
        // write back its stat cache, and this plugin promises never to write.
        command.arg("--no-optional-locks");
        command.args(args);
        // Nothing here reads a terminal, so the default is a closed stdin; the
        // one command that takes a payload is `patch-id`.
        command.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        // A timed-out git can have helpers or wrapper descendants holding these
        // pipes open. Put the invocation in its own group before exec so expiry
        // can close the whole process tree rather than strand the reader joins.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

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
        // A partial clone's missing blobs are fetched from its promisor remote
        // on demand, which writes objects into the repository and reaches the
        // network. Both are things this plugin promises never to do, so the
        // fetch is refused and the invocation fails naming the object instead;
        // see the module note on the partial clone.
        command.env("GIT_NO_LAZY_FETCH", "1");
        // A plugin has no terminal to prompt on; without this a repository with
        // an http remote can block forever asking for a password.
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env("GIT_PAGER", "cat");
        // Everything parsed here is machine output, and a localised git would
        // translate the words this module keys off.
        command.env("LC_ALL", "C");
        // Applied last, so a deliberate GIT_INDEX_FILE survives the scrub above.
        for (key, value) in extra_env {
            command.env(key, value);
        }

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

        // On its own thread, for the same reason the readers are on theirs: the
        // deadline below only has teeth if nothing above it can block forever,
        // and a child that stops reading its stdin would otherwise hang the
        // digest — the one thing this runner exists to prevent. Dropping the
        // pipe at the end of the closure closes it, which is what tells the
        // child the diff has ended.
        let writer = stdin.map(|payload| {
            let mut pipe = child.stdin.take().expect("stdin is piped");
            std::thread::spawn(move || pipe.write_all(&payload))
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
                        terminate_process_group(&mut child);
                        // Reap the direct child before joining any pipe or stdin
                        // threads. Killing only this child is insufficient:
                        // descendants can retain its file descriptors forever.
                        break child.wait().map_err(|err| err.to_string())?;
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(5));
                }
                Err(err) => return Err(format!("waiting for git: {err}").into()),
            }
        };

        // The child exiting for any reason breaks the pipe, so this join cannot
        // outlive the wait above. Any failed write is then reported, including a
        // broken pipe: `patch-id` reads its stdin to EOF, so it cannot have
        // stopped early and still be right, and a short write would leave it
        // hashing a truncated diff and answering confidently. A confident wrong
        // answer is the one outcome this module never permits. A killed child is
        // the exception, already spoken for by `timed_out`.
        if let Some(writer) = writer {
            let wrote = writer.join().map_err(|_| {
                format!(
                    "the thread feeding git {} in {} panicked",
                    args.first().copied().unwrap_or("?"),
                    dir.display()
                )
            })?;
            if let Err(err) = wrote {
                if !timed_out {
                    return Err(format!(
                        "could not feed git {} in {}: {err}",
                        args.first().copied().unwrap_or("?"),
                        dir.display()
                    )
                    .into());
                }
            }
        }

        Ok(GitOut {
            code: status.code(),
            stdout: out_reader.join().unwrap_or_default(),
            stderr: err_reader.join().unwrap_or_default(),
            timed_out,
        })
    }
}

/// Terminates a timed-out invocation and every descendant still in its group.
///
/// `process_group(0)` makes the direct child's PID the group ID. Signalling the
/// negative ID therefore reaches inherited helpers as well as git itself. The
/// direct kill is deliberately retained as a fallback for a failed group signal
/// (or an unrepresentable platform PID), so the child can always be reaped.
#[cfg(unix)]
fn terminate_process_group(child: &mut std::process::Child) {
    let killed_group = libc::pid_t::try_from(child.id())
        .ok()
        .filter(|group| *group > 0)
        .is_some_and(|group| {
            // SAFETY: `group` is the positive PID of the child whose dedicated
            // process group was established by `CommandExt::process_group`.
            unsafe { libc::kill(-group, libc::SIGKILL) == 0 }
        });
    if !killed_group {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
fn terminate_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

// ---------------------------------------------------------------------------
// The scratch index
// ---------------------------------------------------------------------------

/// A throwaway copy of a worktree's index, deleted when it goes out of scope.
///
/// It exists for one reason. `git diff` refreshes the index and writes the
/// refreshed stat data back whenever a tracked file's cached stat data is
/// stale — which an ordinary editor save of identical content is enough to
/// cause. That refresh is **not** the optional kind: verified on git 2.53.0,
/// `GIT_OPTIONAL_LOCKS=0 git --no-optional-locks diff --shortstat` still
/// rewrites the index and still takes `index.lock`. `status --porcelain=v2`
/// with the same flags does not.
///
/// Pointing `GIT_INDEX_FILE` at a copy makes the diff read the same content and
/// write its refresh into the copy instead. Verified: the real index stayed
/// byte-identical and only the copy moved.
///
/// The copy lives in the system temp directory, never beside the repository,
/// and is named for this process and an increasing counter so concurrent
/// reports of different checkouts cannot collide.
struct ScratchIndex {
    path: PathBuf,
}

impl ScratchIndex {
    /// `Ok(None)` means the worktree has no index to copy, which is a real
    /// state — a repository where nothing has ever been staged — and one where
    /// both `diff --shortstat` forms are empty anyway.
    fn of(git_dir: Option<&Path>) -> std::result::Result<Option<ScratchIndex>, std::io::Error> {
        let Some(git_dir) = git_dir else {
            return Err(std::io::Error::other(
                "the per-worktree git directory could not be read",
            ));
        };
        let source = git_dir.join("index");
        if !source.is_file() {
            return Ok(None);
        }

        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("standup-index-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // A byte copy rather than `read-tree`: copying keeps the stat cache, so
        // the diff does the same work it would have done against the real index
        // and reports the same numbers.
        std::fs::copy(&source, &path)?;
        Ok(Some(ScratchIndex { path }))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        // git writes `<index>.lock` and renames it into place; a killed
        // invocation can leave one behind, and it is ours to clean up.
        let mut lock = self.path.clone().into_os_string();
        lock.push(".lock");
        let _ = std::fs::remove_file(PathBuf::from(lock));
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
fn parse_log(bytes: &[u8], ignored: &Ignored) -> (Vec<Commit>, Churn) {
    let mut commits: Vec<Commit> = Vec::new();
    let mut files_seen: Vec<String> = Vec::new();
    let mut excluded_seen: Vec<String> = Vec::new();
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
        // A binary file prints `-` for both counts, and a generated or vendored
        // path is treated exactly the same way: it is still a file the commit
        // touched, so it counts toward the file total and adds nothing to the
        // lines. Lines are the number that means "effort", and a regenerated
        // lockfile is tens of thousands of them that nobody wrote.
        let counts_lines = !ignored.matches(&path);
        if counts_lines {
            commit.insertions += added.unwrap_or(0);
            commit.deletions += deleted.unwrap_or(0);
            churn.insertions += added.unwrap_or(0);
            churn.deletions += deleted.unwrap_or(0);
        } else if !excluded_seen.contains(&path) {
            excluded_seen.push(path.clone());
        }
        if !files_seen.contains(&path) {
            files_seen.push(path.clone());
        }
        commit.files.push(path);
    }

    // The union, not the sum: a file edited in five commits is one file changed.
    churn.files = files_seen.len();
    churn.excluded = excluded_seen.len();
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

/// How a child ended, in words: a process killed by a signal has no code, and
/// "exited none" is not something to show a reader.
fn exit_label(code: Option<i32>) -> String {
    code.map(|code| code.to_string())
        .unwrap_or_else(|| "on a signal".to_string())
}

/// git's own account of a fetch it refused because `GIT_NO_LAZY_FETCH=1` is
/// set, or `None` when the failure was something else entirely.
///
/// Two things arrive on a refusal, measured on git 2.53.0 against a blobless
/// clone and flattened into one line by [`GitOut::stderr_text`]:
///
/// ```text
/// warning: lazy fetching disabled; some objects may not be available
/// fatal: could not fetch e3169e47… from promisor remote
/// ```
///
/// The warning is the mechanism, identical on every refusal; the fatal is the
/// news, and it names the object, which is the whole reason to report this
/// rather than a shrug. So the warning is dropped and everything from the first
/// `fatal:` or `error:` onward is kept.
///
/// Both markers are matched because either alone identifies the case: git could
/// reword one of them, and a refused fetch reported as an unexplained log
/// failure would send the reader looking for a corrupt repository.
fn refused_promisor_fetch(stderr: &str) -> Option<String> {
    if !stderr.contains("promisor") && !stderr.contains("lazy fetching disabled") {
        return None;
    }
    let news = ["fatal:", "error:"]
        .iter()
        .filter_map(|marker| stderr.find(marker))
        .min()
        .map_or(stderr, |at| &stderr[at..]);
    Some(news.trim().to_string())
}

/// `(major, minor)` out of `git version 2.36.6`, or `None` when the line is not
/// one this can be sure of.
///
/// Only two numbers, because every capability this module gates on landed in a
/// feature release. The trailing junk is real: Apple's build prints
/// `git version 2.39.5 (Apple Git-154)`, and a Windows build appends
/// `.windows.1`, so the version is taken as the first whitespace-delimited word
/// that starts with a digit and read up to its second dot.
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let field = text
        .split_whitespace()
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut numbers = field.split('.');
    let major = numbers.next()?.parse().ok()?;
    let minor = numbers.next()?.parse().ok()?;
    Some((major, minor))
}

/// The promisor remote named by one `git config --get-regexp` line, if it names
/// one.
///
/// `extensions.partialclone <remote>` carries the name as its value;
/// `remote.<name>.promisor true` carries it in the key, and the value has to be
/// read because the flag can be set to `false`. A boolean written with no value
/// at all prints as the bare key and means true, which is git's own reading.
fn promisor_name(line: &str) -> Option<String> {
    let (key, value) = match line.split_once(char::is_whitespace) {
        Some((key, value)) => (key, value.trim()),
        None => (line.trim(), ""),
    };
    if key == "extensions.partialclone" {
        return (!value.is_empty()).then(|| value.to_string());
    }
    let name = key.strip_prefix("remote.")?.strip_suffix(".promisor")?;
    matches!(value, "" | "true" | "yes" | "on" | "1").then(|| name.to_string())
}

/// An unreadable at-risk count, recorded twice on purpose.
///
/// The state carries the reason for the JSON, and `problems` carries it for the
/// renderers — which is not belt and braces. A checkout whose count could not be
/// read has nothing to put on its `unpushed:` line, so without a problem beside
/// it the checkout is quiet, and a quiet repository is summarised to its name and
/// dropped altogether when quiet ones are excluded. The failure would be
/// invisible in exactly the digest that needed it.
fn unpushed_unknown(reason: String, problems: &mut Vec<String>) -> Unpushed {
    problems.push(format!(
        "could not count the commits that exist only here: {reason}"
    ));
    Unpushed::Unknown { reason }
}

/// Splices [`PATCH_ID_DIFF_OPTIONS`] into a diff-producing command.
///
/// `command` is the subcommand followed by its own arguments, and the pinned
/// options land between them, so a caller cannot compare two diffs that were
/// produced differently — which is the whole failure this guards.
fn diff_args<'a>(command: &[&'a str]) -> Vec<&'a str> {
    let (subcommand, rest) = command.split_first().expect("a subcommand to run");
    let mut args = Vec::with_capacity(command.len() + PATCH_ID_DIFF_OPTIONS.len() + 1);
    args.push(*subcommand);
    args.push("-p");
    args.extend_from_slice(PATCH_ID_DIFF_OPTIONS);
    args.extend_from_slice(rest);
    args
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

        let (commits, churn) = parse_log(&stream, &Ignored::new(Vec::new()));
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
        let (commits, churn) = parse_log(&stream, &Ignored::new(Vec::new()));
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

    /// A direct-child kill is not enough when a helper inherits the output
    /// pipes: the readers remain blocked after git is reaped. The wrapper's
    /// sleeping child deliberately retains both pipes, and the channel keeps a
    /// broken implementation from hanging the test process indefinitely.
    #[cfg(unix)]
    #[test]
    fn a_timeout_kills_descendants_before_joining_pipe_readers() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::mpsc;

        static SEQ: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "standup-git-timeout-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create timeout fixture");
        struct RemoveDir(PathBuf);
        impl Drop for RemoveDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _remove_root = RemoveDir(root.clone());

        let wrapper = root.join("git");
        std::fs::write(
            &wrapper,
            r#"#!/bin/sh
sleep 300 &
descendant=$!
printf '%s\n' "$descendant" > "$2/descendant.pid"
wait "$descendant"
"#,
        )
        .expect("write fake git");
        let mut permissions = std::fs::metadata(&wrapper)
            .expect("stat fake git")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&wrapper, permissions).expect("make fake git executable");

        let invocation_timeout = Duration::from_secs(1);
        let return_limit = Duration::from_secs(3);
        let thread_root = root.clone();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut git = Git::new(invocation_timeout);
            // Do not mutate `GIT`: unit tests run concurrently, and the runner
            // already owns the resolved program path explicitly.
            git.program = wrapper;
            let started = Instant::now();
            let mut problems = Vec::new();
            let out = git.capture(&thread_root, &["status"], &mut problems);
            let _ = finished_tx.send((out, problems, started.elapsed()));
        });

        let pid_path = root.join("descendant.pid");
        let ready_deadline = Instant::now() + return_limit;
        let descendant = loop {
            match std::fs::read_to_string(&pid_path) {
                Ok(raw) => match raw.trim().parse::<libc::pid_t>() {
                    Ok(pid) => break pid,
                    Err(_) if Instant::now() < ready_deadline => std::thread::yield_now(),
                    Err(err) => panic!("fake git published an invalid descendant pid: {err}"),
                },
                Err(err)
                    if err.kind() == std::io::ErrorKind::NotFound
                        && Instant::now() < ready_deadline =>
                {
                    std::thread::yield_now();
                }
                Err(err) => panic!("fake git did not publish its descendant pid: {err}"),
            }
        };

        let (out, problems, elapsed) = match finished_rx.recv_timeout(return_limit) {
            Ok(result) => result,
            Err(err) => {
                // Before the fix, killing the orphan closes its inherited pipes
                // and lets the detached worker unwind instead of leaking it.
                // SAFETY: this PID was published by this test's own wrapper.
                unsafe {
                    libc::kill(descendant, libc::SIGKILL);
                }
                drop(worker);
                panic!("timed-out git did not return within {return_limit:?}: {err}");
            }
        };
        worker.join().expect("git runner thread");

        assert!(out.is_none(), "a timed-out capture has no output");
        assert_eq!(
            problems,
            vec![format!(
                "git status timed out after {invocation_timeout:?} in {}",
                root.display()
            )]
        );
        assert!(
            elapsed <= return_limit,
            "timeout returned after {elapsed:?}, limit was {return_limit:?}"
        );

        let gone_deadline = Instant::now() + return_limit;
        let descendant_exists = || {
            // SAFETY: signal zero only queries the PID published by the wrapper.
            let result = unsafe { libc::kill(descendant, 0) };
            result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        };
        while descendant_exists() && Instant::now() < gone_deadline {
            std::thread::yield_now();
        }
        if descendant_exists() {
            // Do not leave the regression fixture behind if this assertion is
            // what exposes a future process-group regression.
            // SAFETY: this PID was published by this test's own wrapper.
            unsafe {
                libc::kill(descendant, libc::SIGKILL);
            }
            panic!("git descendant {descendant} survived the timeout");
        }
    }

    /// The three shapes `git --version` is known to print. The Apple and Windows
    /// builds are why this reads a prefix rather than the whole field.
    #[test]
    fn a_git_version_is_read_as_major_and_minor() {
        assert_eq!(parse_version("git version 2.36.6"), Some((2, 36)));
        assert_eq!(
            parse_version("git version 2.39.5 (Apple Git-154)"),
            Some((2, 39))
        );
        assert_eq!(parse_version("git version 2.51.0.windows.1"), Some((2, 51)));
    }

    /// An unreadable version must not be mistaken for a new one: the caller
    /// treats `None` as a git that cannot refuse a promisor fetch, and the
    /// alternative is writing to someone's repository.
    #[test]
    fn an_unreadable_version_is_no_answer_at_all() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("git version UNKNOWN"), None);
        assert_eq!(parse_version("git version 2"), None);
    }

    /// The refusal is reported with the clause that names the object, not with
    /// the warning every refusal carries — and anything else git says is left
    /// alone.
    #[test]
    fn a_refused_fetch_is_reported_by_the_object_it_could_not_read() {
        let refusal = refused_promisor_fetch(
            "warning: lazy fetching disabled; some objects may not be available fatal: could not \
             fetch e3169e47 from promisor remote",
        );
        assert_eq!(
            refusal.as_deref(),
            Some("fatal: could not fetch e3169e47 from promisor remote")
        );
        assert_eq!(refused_promisor_fetch("fatal: bad object HEAD"), None);
    }
}
