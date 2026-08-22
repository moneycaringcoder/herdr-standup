//! The shared vocabulary of the plugin.
//!
//! Every module either builds part of a [`Digest`] or renders one. Nothing else
//! is shared between modules, so this file is the whole contract:
//!
//! ```text
//!   herdr.rs   session.snapshot  ->  Vec<WorkspaceRef>   (where agents sit)
//!   git.rs     read-only git     ->  CheckoutReport      (what happened there)
//!   window.rs  --since/--since-last -> Window            (over what period)
//!   standup.rs joins those       ->  Digest
//!   render.rs  Digest            ->  text | markdown | json
//! ```
//!
//! Two rules run through the types and are worth stating once:
//!
//! 1. **Absent and broken are different.** `Option::None` means "herdr or git
//!    did not report this", and never "zero". Anything that went wrong carries
//!    its own message in a `problems` list and is rendered, because a digest
//!    that silently drops a repository looks exactly like a quiet day.
//! 2. **Quiet is a state, not an omission.** A checkout with nothing in the
//!    window is still present in the digest, marked [`Activity::Quiet`].

use std::path::PathBuf;

use serde::Serialize;

/// Canonical identity of a repository: its absolute `--git-common-dir`, shared
/// by every linked worktree of the same repo. Never the directory name, never
/// `--git-dir` (which is per-worktree).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct RepoKey(pub String);

impl RepoKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// An instant, carried as both the machine value and the exact local rendering.
///
/// The roadmap calls an ambiguous timestamp in a daily digest a bug, so the two
/// halves travel together: `epoch` is what git was given, `local` is what the
/// user is shown, and `zone` names the zone `local` is expressed in. A renderer
/// can never accidentally print a UTC instant as if it were local, because it
/// has no way to format `epoch` itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stamp {
    /// Seconds since the Unix epoch.
    pub epoch: i64,
    /// `YYYY-MM-DD HH:MM` in the local zone.
    pub local: String,
    /// Zone abbreviation and offset, e.g. `CEST +0200`.
    pub zone: String,
}

impl Stamp {
    /// `2026-08-15 09:12 CEST +0200` — the unambiguous form, for headers.
    pub fn full(&self) -> String {
        format!("{} {}", self.local, self.zone)
    }
}

/// How the reporting window was chosen. Rendered, because "you asked for
/// today" and "this is the first run so I fell back to today" are different
/// answers to "why is this empty".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WindowSource {
    /// The built-in default: local midnight today.
    Default,
    /// An explicit `--since` the user typed.
    Explicit { spec: String },
    /// `--since-last`, with a previous run on record.
    SinceLast { previous_run: Stamp },
    /// `--since-last` with no previous run recorded — first use, or a wiped
    /// state directory. Falls back to the default window and says so.
    SinceLastFirstRun,
}

/// The reporting period, resolved to absolute instants before any git command
/// runs.
///
/// Resolution is deliberately eager: `git rev-parse --since=<spec>` answers
/// "now" for input it cannot parse, with exit status 0, so a typo'd window would
/// otherwise produce an empty digest indistinguishable from a quiet day. See
/// `window::resolve`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Window {
    pub since: Stamp,
    /// `None` means "up to now", which is the normal case.
    pub until: Option<Stamp>,
    pub source: WindowSource,
}

// ---------------------------------------------------------------------------
// herdr side
// ---------------------------------------------------------------------------

/// A herdr workspace that is sitting in some directory.
///
/// Discovery does **not** rely on `workspace.worktree`. Verified against a live
/// 0.8.0 session: that key is present only for workspaces herdr itself opened as
/// a repo or a worktree, and is absent for a workspace simply `cd`-ed into a
/// checkout. In a ten-workspace capture, **nine workspaces were sitting in git
/// checkouts and only three carried a `worktree` key** — reporting the tracked
/// ones alone would have omitted two thirds of the day's work. So the candidate
/// paths are the union of `workspace.worktree.checkout_path` and every distinct
/// pane `cwd`, and git decides which of them are checkouts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceRef {
    pub workspace_id: String,
    /// The workspace label the user sees in the sidebar.
    pub label: String,
    /// Workspace number in the sidebar, when herdr reports one.
    pub number: Option<u64>,
    /// Directories occupied by this workspace's panes, plus its tracked
    /// checkout path if it has one. Deduplicated, order stable.
    pub paths: Vec<PathBuf>,
    /// Agents herdr reports in this workspace.
    pub agents: Vec<AgentRef>,
    /// herdr's own view of the workspace: `idle`, `working`, `done`, ...
    pub agent_status: Option<String>,
}

/// An agent herdr reports occupying a pane.
///
/// Decision, recorded because it is a judgement call: the digest carries the
/// agent's **name and program** everywhere, and its **session id only in the
/// JSON output**. The name is what makes a digest readable ("shear-classifier
/// landed three commits"); the session id is a pointer for follow-up that has no
/// business being pasted into a team channel. Never any transcript content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRef {
    /// The user's label for the agent, e.g. `shear-classifier`.
    pub name: Option<String>,
    /// The program, e.g. `claude` or `opencode`.
    pub program: Option<String>,
    /// Opaque session id, JSON output only.
    pub session_id: Option<String>,
    pub pane_id: String,
    /// Per-pane status, e.g. `working`.
    pub status: Option<String>,
    /// The directory this agent was working in, when herdr says.
    ///
    /// This is what makes attribution a fact rather than a guess. Agents are
    /// reported per workspace, and a workspace's panes need not all sit in the
    /// same checkout, so a workspace-scoped agent list credits every agent to
    /// every checkout the workspace touches. The agent rows in a live capture
    /// each carry their own `cwd`; where one does not, the answer is unknown and
    /// says so rather than being spread across the candidates.
    pub cwd: Option<PathBuf>,
}

impl AgentRef {
    /// Best display name: the user's label, else the program, else nothing.
    ///
    /// Not an identity. `agent` is `claude` for all but one row of the live
    /// capture and `name` is absent on three, so two agents can share a display
    /// name and still be two agents — see [`CheckoutDigest::agents`], which
    /// deduplicates on the pane instead.
    pub fn display(&self) -> Option<&str> {
        self.name
            .as_deref()
            .or(self.program.as_deref())
            .filter(|s| !s.is_empty())
    }
}

// ---------------------------------------------------------------------------
// git side
// ---------------------------------------------------------------------------

/// What HEAD is, which decides how much of the rest is even meaningful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Head {
    /// On a branch, with a commit.
    Branch { name: String, oid: String },
    /// Detached at a commit.
    Detached { oid: String },
    /// A branch is pointed at but has no commits yet, and the worktree has no
    /// HEAD reflog — a freshly `git init`-ed checkout.
    Unborn { name: String },
    /// A branch is pointed at, it does not resolve, but the worktree *has* a
    /// HEAD reflog: the branch was deleted underneath a live checkout. Looks
    /// identical to unborn without the reflog check, and is a real problem
    /// worth naming rather than a normal empty state.
    BranchDeleted { name: String },
}

impl Head {
    pub fn branch_name(&self) -> Option<&str> {
        match self {
            Head::Branch { name, .. } | Head::Unborn { name } | Head::BranchDeleted { name } => {
                Some(name)
            }
            Head::Detached { .. } => None,
        }
    }

    pub fn oid(&self) -> Option<&str> {
        match self {
            Head::Branch { oid, .. } | Head::Detached { oid } => Some(oid),
            Head::Unborn { .. } | Head::BranchDeleted { .. } => None,
        }
    }
}

/// One commit inside the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Commit {
    /// Full 40-hex object id. Renderers abbreviate; the data keeps it whole.
    pub oid: String,
    pub author: String,
    /// Commit time, local. `--since`/`--until` filter on commit date, so this
    /// is the field the window is actually about.
    pub committed: Stamp,
    /// First line of the message, with no trailing whitespace.
    pub subject: String,
    /// True for a commit with more than one parent. Merges carry no diffstat of
    /// their own, so they count toward the commit total and not toward churn.
    pub is_merge: bool,
    pub insertions: u64,
    pub deletions: u64,
    /// Paths this commit touched. Empty for a merge.
    pub files: Vec<String>,
}

impl Commit {
    /// The abbreviation renderers show. Fixed at 8, because a digest is read as
    /// a column and a variable-width id ruins the alignment.
    pub fn short_oid(&self) -> &str {
        let end = self.oid.len().min(8);
        &self.oid[..end]
    }
}

/// Aggregate change volume. `files` is the size of the *union* of touched
/// paths, not the sum of per-commit file counts, so a file edited in five
/// commits counts once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Churn {
    pub files: usize,
    pub insertions: u64,
    pub deletions: u64,
}

impl Churn {
    pub fn is_zero(&self) -> bool {
        self.files == 0 && self.insertions == 0 && self.deletions == 0
    }

    pub fn add(&mut self, other: Churn) {
        self.files += other.files;
        self.insertions += other.insertions;
        self.deletions += other.deletions;
    }
}

/// Uncommitted work sitting in the checkout right now.
///
/// Not strictly "what happened in the window" — it has no timestamp — but it is
/// the difference between "the agent did nothing" and "the agent did a day of
/// work and never committed it", which is the single most useful thing a
/// standup digest can tell you. Reported as a present-tense fact, separately
/// from the window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Dirty {
    pub tracked_changed: usize,
    pub untracked: usize,
    pub conflicted: usize,
    pub insertions: u64,
    pub deletions: u64,
}

impl Dirty {
    pub fn is_clean(&self) -> bool {
        self.tracked_changed == 0 && self.untracked == 0 && self.conflicted == 0
    }
}

/// Upstream tracking state for the checked-out branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Tracking {
    /// No branch (detached/unborn), so tracking is not a question that applies.
    NotApplicable,
    /// A branch with no configured upstream: nothing is watching this work.
    NoUpstream,
    /// An upstream is configured but does not resolve — typically the remote
    /// branch was deleted after a merge. Distinct from `NoUpstream`, and worth
    /// saying so.
    UpstreamMissing { name: String },
    Upstream {
        name: String,
        ahead: u64,
        behind: u64,
    },
}

/// Whether the work has landed on the repository's default branch.
///
/// Decision, recorded: **"merged" means merged into the default branch**, not
/// into the upstream tracking branch, even when they differ. "Did it land?" is a
/// question about the trunk; a topic branch pushed to its own remote branch has
/// been *published*, not landed, and that is reported separately by
/// [`Tracking`]. When the default branch cannot be determined the answer is
/// [`Landed::Unknown`] with the reason attached — never a bare `false`, which
/// would read as "did not land".
///
/// Two states here both mean "the work is in", and they are kept apart on
/// purpose. [`Landed::Merged`] is *containment*, which
/// `git merge-base --is-ancestor` proves outright: the commit itself is on the
/// trunk. [`Landed::Equivalent`] is a *matching patch*, which is all that
/// survives a squash merge or a rebase merge — both rewrite the commit, so the
/// original sha never reaches the trunk and containment answers "no" for work
/// that shipped weeks ago. A matching patch is strong evidence and not proof,
/// since two commits with the same diff are indistinguishable by patch id, so
/// the digest says which of the two it holds rather than flattening them into
/// one word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Landed {
    /// This checkout *is* the default branch.
    IsDefault { name: String },
    /// HEAD is an ancestor of the default branch: the work is in, exactly.
    Merged { into: String },
    /// HEAD is not an ancestor of the default branch, but the same patch is on
    /// it under another sha — what a squash or a rebase merge leaves behind.
    Equivalent { into: String, how: Equivalence },
    /// HEAD is not an ancestor of the default branch and no equivalent patch
    /// was found on it either.
    NotMerged { into: String },
    /// No default branch could be identified, or HEAD has no commit.
    Unknown { reason: String },
}

/// Which probe found the equivalent patch, and what it found.
///
/// Both carry enough to re-run by hand, because the standard for every number
/// in this digest is that one git command reproduces it, and a verdict this
/// indirect owes the reader that command more than the exact ones do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Equivalence {
    /// Every commit on the branch has a patch-id twin on the default branch:
    /// `git cherry <default> HEAD` printed `-` lines and nothing else. This is
    /// the shape a rebase merge leaves, and the shape a squash of a single
    /// commit leaves.
    EveryCommit { commits: u64 },
    /// One commit on the default branch carries the patch id of this branch's
    /// whole diff against the merge base. This is the shape a squash merge of
    /// more than one commit leaves: no individual patch survives it, so
    /// `git cherry` finds nothing and the combined diff is the only thing left
    /// to match.
    Squashed { oid: String },
}

/// How much a checkout has to say. Ordering matters: renderers sort busiest
/// first, and `Broken` sorts to the top of everything so a failure is never
/// buried under a page of quiet workspaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    /// Nothing in the window and nothing uncommitted.
    Quiet,
    /// Uncommitted changes but no commits in the window.
    Uncommitted,
    /// Commits in the window.
    Active,
    /// git could not be read here. Rendered loudly.
    Broken,
}

/// Everything known about one checkout over the window. Built by `git.rs` from
/// a path; the herdr side is joined on afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckoutReport {
    /// The checkout root, as git resolves it (`--show-toplevel`), not the pane
    /// cwd that led us there.
    pub path: PathBuf,
    pub repo_key: RepoKey,
    pub repo_root: PathBuf,
    pub is_linked_worktree: bool,
    pub head: Head,
    pub commits: Vec<Commit>,
    pub churn: Churn,
    pub dirty: Dirty,
    pub tracking: Tracking,
    pub landed: Landed,
    /// Things that went wrong while reading this checkout. Non-empty means the
    /// numbers above are incomplete, and every renderer must show it.
    pub problems: Vec<String>,
}

impl CheckoutReport {
    pub fn activity(&self) -> Activity {
        // A branch deleted underneath a live checkout is not a reporting
        // failure — the numbers are right — but it is somebody's work about to
        // be lost, so it sorts to the top with the failures rather than being
        // filed under "quiet".
        if !self.problems.is_empty() || matches!(self.head, Head::BranchDeleted { .. }) {
            Activity::Broken
        } else if !self.commits.is_empty() {
            Activity::Active
        } else if !self.dirty.is_clean() {
            Activity::Uncommitted
        } else {
            Activity::Quiet
        }
    }

    /// Distinct commit authors in the window, first-seen order. Useful because
    /// "three agents, one author identity" is the normal shape here.
    pub fn authors(&self) -> Vec<&str> {
        let mut seen: Vec<&str> = Vec::new();
        for commit in &self.commits {
            let author = commit.author.as_str();
            if !author.is_empty() && !seen.contains(&author) {
                seen.push(author);
            }
        }
        seen
    }
}

// ---------------------------------------------------------------------------
// The digest
// ---------------------------------------------------------------------------

/// One checkout, with the herdr workspaces that were sitting in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckoutDigest {
    #[serde(flatten)]
    pub report: CheckoutReport,
    /// Workspaces whose panes were in this checkout. Usually one; zero when the
    /// checkout was found through a sibling worktree rather than a workspace.
    ///
    /// This is the workspace *roster*, not the attribution: a workspace listed
    /// here has at least one pane in this checkout, and its `agents` are every
    /// agent it holds wherever they were sitting. Who worked *here* is
    /// [`CheckoutDigest::agents`], the field below.
    pub workspaces: Vec<WorkspaceRef>,
    /// The agents herdr placed in this checkout, in pane order, one entry per
    /// pane.
    ///
    /// Decided in `standup.rs` rather than derived here, because deciding it
    /// needs git: an agent's `cwd` is a directory, and only git knows which
    /// checkout a directory belongs to. An agent herdr could not place is in
    /// neither this list nor another checkout's, and the digest says so in a
    /// note — crediting it to every candidate would be a guess, and a reader
    /// takes "this agent worked here" as a fact.
    ///
    /// One entry per pane, never per name. Two agents can share a display name
    /// and still be two agents: `agent` is `claude` for all but one row of the
    /// live capture, and `name` is absent on three of eighteen. Collapsing on
    /// the name reported two agents in one checkout as one, which is #19.
    pub agents: Vec<AgentRef>,
}

/// The rollup the roadmap asks for.
///
/// Decision, recorded: the digest groups **primarily by repository**, with
/// checkouts nested inside. Grouping by time would put two commits from one
/// branch on opposite sides of an unrelated repo's commit, and "what came out of
/// today" is answered per project. Time still orders everything inside a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoDigest {
    pub repo_key: RepoKey,
    /// Display name: the basename of the repo root.
    pub name: String,
    pub repo_root: PathBuf,
    pub checkouts: Vec<CheckoutDigest>,
    /// Sum over the checkouts. `commits` counts distinct commit ids, so a commit
    /// visible from two worktrees of one repo is counted once.
    pub commits: usize,
    pub churn: Churn,
}

impl RepoDigest {
    pub fn activity(&self) -> Activity {
        self.checkouts
            .iter()
            .map(|c| c.report.activity())
            .max()
            .unwrap_or(Activity::Quiet)
    }
}

/// A problem that is not attributable to a single checkout — a workspace path
/// that is not a repo, a snapshot field that did not parse, a git binary that
/// could not be run. Always rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
}

impl Note {
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            message: message.into(),
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
        }
    }
}

/// The whole report. Every renderer takes exactly this and adds nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Digest {
    /// Schema version for the JSON output, so a script can refuse a shape it
    /// does not know.
    pub schema: u32,
    pub generated_at: Stamp,
    pub window: Window,
    pub repos: Vec<RepoDigest>,
    pub notes: Vec<Note>,
}

/// Bumped whenever the JSON shape changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

impl Digest {
    pub fn total_commits(&self) -> usize {
        self.repos.iter().map(|r| r.commits).sum()
    }

    pub fn total_churn(&self) -> Churn {
        let mut churn = Churn::default();
        for repo in &self.repos {
            churn.add(repo.churn);
        }
        churn
    }

    /// Repos with nothing to report. Summarised as one line rather than
    /// dropped, so "no repos" and "ten quiet repos" never look the same.
    pub fn quiet_repos(&self) -> Vec<&RepoDigest> {
        self.repos
            .iter()
            .filter(|r| r.activity() == Activity::Quiet)
            .collect()
    }

    pub fn busy_repos(&self) -> Vec<&RepoDigest> {
        self.repos
            .iter()
            .filter(|r| r.activity() != Activity::Quiet)
            .collect()
    }
}
