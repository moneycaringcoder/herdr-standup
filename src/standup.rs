//! Orchestration: herdr's view of the session, joined to git's view of the
//! disk, grouped by repository.
//!
//! The pipeline, in order, with the reason each step exists:
//!
//! 1. **Ask herdr where the agents are.** One `session.snapshot`.
//! 2. **Collect candidate directories.** Pane cwds and tracked checkout paths,
//!    plus anything the user passed with `--path`. Not filtered yet — git is
//!    the authority on what is a checkout.
//! 3. **Identify.** Each candidate becomes a [`CheckoutId`] or a note saying it
//!    is not a repository. Two workspaces in one directory collapse to one
//!    checkout carrying both.
//! 4. **Expand to siblings.** Other worktrees of the same repository, which no
//!    workspace is sitting in. An agent that finished and had its workspace
//!    closed still produced commits today, and a strictly per-workspace view
//!    would lose exactly that work.
//! 5. **Resolve the window**, using a real repository as the parsing context,
//!    so an unparseable `--since` fails loudly instead of rendering as a quiet
//!    day.
//! 6. **Report** each checkout, and roll up by repository.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::cache::Cache;
use crate::clock;
use crate::config::{Config, Ignored};
use crate::git::{CheckoutId, Git};
use crate::herdr::Herdr;
use crate::model::{
    Activity, AgentRef, CheckoutDigest, Churn, Digest, Note, RepoDigest, RepoKey, WorkspaceRef,
    SCHEMA_VERSION,
};
use crate::render::quantity;
use crate::window;
use crate::Result;

/// Builds the digest. Everything that can degrade does, and says so in
/// `digest.notes`; only a failure that would make the whole report a lie is
/// returned as an error.
pub fn build(config: &Config) -> Result<Digest> {
    let ignored = Ignored::new(config.ignore.clone());
    // The persistent cache is asked for here and nowhere else. `Git::new` alone
    // caches in memory only, which is what keeps the test suites from reading
    // the state directory of whoever is running them.
    let git = Git::new(config.git_timeout)
        .ignoring(ignored.clone())
        .caching(Cache::load());
    let mut notes: Vec<Note> = Vec::new();

    let workspaces = collect_workspaces(config, &mut notes)?;

    // Candidate directories, in a stable order: herdr's first, so the digest
    // reads in sidebar order, then anything the user named explicitly.
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut owners: Vec<(PathBuf, WorkspaceRef)> = Vec::new();
    for workspace in &workspaces {
        for path in &workspace.paths {
            push_unique(&mut candidates, path.clone());
            owners.push((path.clone(), workspace.clone()));
        }
    }
    if !config.offline {
        if let Some(cwd) = config.repository_scope().invocation_cwd() {
            push_unique(&mut candidates, cwd.to_path_buf());
        }
    }
    for path in &config.extra_paths {
        push_unique(&mut candidates, path.clone());
    }

    // Identify. A directory that is not a repository is ordinary data — most
    // sessions have at least one — but it is still worth one line, because
    // "standup did not mention that workspace" should never be a mystery.
    let mut checkouts: Vec<(CheckoutId, Vec<WorkspaceRef>)> = Vec::new();
    // Which checkout each candidate directory turned out to be part of. Only
    // git can answer that — an agent's `cwd` may be a subdirectory of the
    // checkout root — so the answers are kept rather than recomputed.
    let mut resolved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for candidate in &candidates {
        match git.identify(candidate) {
            Ok(Some(id)) => {
                let mut here: Vec<WorkspaceRef> = Vec::new();
                for (path, workspace) in &owners {
                    if path == candidate
                        && !here
                            .iter()
                            .any(|w| w.workspace_id == workspace.workspace_id)
                    {
                        here.push(workspace.clone());
                    }
                }
                resolved.push((candidate.clone(), id.path.clone()));
                merge_checkout(&mut checkouts, id, here);
            }
            Ok(None) => {
                let label = label_for(&owners, candidate);
                notes.push(Note::info(format!(
                    "{label}{} is not a git checkout — skipped",
                    candidate.display()
                )));
            }
            Err(err) => notes.push(Note::warning(format!(
                "could not inspect {}: {err}",
                candidate.display()
            ))),
        }
    }

    if config.include_siblings {
        expand_siblings(&git, &mut checkouts, &mut notes);
    }

    let anchors: Vec<PathBuf> = checkouts.iter().map(|(id, _)| id.path.clone()).collect();
    let (reporting_window, window_notes) =
        window::resolve(&git, &anchors, &crate::config::date_ref_repo(), config)?;
    notes.extend(window_notes);

    let (placed, placement_notes) = place_agents(&workspaces, &resolved);
    notes.extend(placement_notes);

    // Report, then group. Grouping by repository is the deliberate choice: see
    // `model::RepoDigest`.
    let mut by_repo: BTreeMap<RepoKey, RepoDigest> = BTreeMap::new();
    for (id, workspaces) in checkouts {
        let report = git.report(&id, &reporting_window);
        let agents = placed
            .iter()
            .find(|(path, _)| *path == id.path)
            .map(|(_, agents)| agents.clone())
            .unwrap_or_default();
        let entry = by_repo
            .entry(id.repo_key.clone())
            .or_insert_with(|| RepoDigest {
                repo_key: id.repo_key.clone(),
                name: display_name(&id),
                repo_root: id.repo_root.clone(),
                checkouts: Vec::new(),
                commits: 0,
                churn: Churn::default(),
                active_days: 0,
            });
        entry.checkouts.push(CheckoutDigest {
            report,
            workspaces,
            agents,
        });
    }

    let mut repos: Vec<RepoDigest> = by_repo.into_values().collect();
    for repo in &mut repos {
        rollup(repo, &ignored);
        repo.checkouts.sort_by(|a, b| {
            b.report
                .activity()
                .cmp(&a.report.activity())
                .then_with(|| latest(b).cmp(&latest(a)))
                .then_with(|| a.report.path.cmp(&b.report.path))
        });
    }
    repos.sort_by(|a, b| {
        b.activity()
            .cmp(&a.activity())
            .then_with(|| b.commits.cmp(&a.commits))
            .then_with(|| a.name.cmp(&b.name))
    });

    if !config.include_quiet {
        repos.retain(|repo| repo.activity() != Activity::Quiet);
    }

    // Written once, after every checkout has been collected, so a session of
    // twenty worktrees costs one file replacement rather than twenty.
    git.save_cache();

    Ok(Digest {
        schema: SCHEMA_VERSION,
        generated_at: clock::stamp(clock::now()),
        window: reporting_window,
        repos,
        notes,
    })
}

/// herdr's workspaces, or a clearly-explained empty list.
///
/// `--offline` skips the socket outright. Otherwise an unreachable herdr is
/// fatal *unless* the user named paths by hand, in which case it degrades to
/// those paths with a warning — running `standup --path .` from a shell with no
/// herdr running is a reasonable thing to want.
fn collect_workspaces(config: &Config, notes: &mut Vec<Note>) -> Result<Vec<WorkspaceRef>> {
    if config.offline {
        if config.extra_paths.is_empty() {
            return Err("--offline needs at least one --path to report on".into());
        }
        return Ok(Vec::new());
    }
    match Herdr::connect().and_then(|mut herdr| herdr.workspaces_scoped(config.repository_scope()))
    {
        Ok(workspaces) => Ok(workspaces),
        Err(err) if !config.extra_paths.is_empty() => {
            notes.push(Note::warning(format!(
                "herdr is not reachable ({err}); reporting only the paths given with --path"
            )));
            Ok(Vec::new())
        }
        Err(err) => Err(err),
    }
}

/// Decides which checkout each agent worked in.
///
/// The problem this solves: herdr reports agents **per workspace**, and a
/// workspace is not a place. Its panes can sit in different checkouts, so
/// handing a checkout the whole workspace roster credits every agent with work
/// in every directory the workspace touched. A reader takes "shear-classifier
/// worked here" as a fact, and two agents in one window collapsing into
/// whichever herdr mentioned last is the same failure from the other side.
///
/// So each agent is placed by its **own** directory, resolved through `resolved`
/// — the candidate directories git already turned into checkouts, which is the
/// only authority on which checkout a directory belongs to.
///
/// Three cases, and the third is the point:
///
/// 1. The agent's directory resolves to a checkout. It is placed there. Exact.
/// 2. The agent has no directory, and its workspace touches exactly one
///    checkout. There is nowhere else it could have been, so it is placed
///    there. Still exact.
/// 3. The agent has no directory and its workspace spans several checkouts.
///    **Unknowable**, so it is placed nowhere and the caller gets a note naming
///    the workspace and the count. Spreading it across the candidates would put
///    a guess where the digest promises a fact.
///
/// Returns `(checkout root, agents)` pairs and the notes for case three.
fn place_agents(
    workspaces: &[WorkspaceRef],
    resolved: &[(PathBuf, PathBuf)],
) -> (Vec<(PathBuf, Vec<AgentRef>)>, Vec<Note>) {
    let checkout_of = |dir: &Path| -> Option<&PathBuf> {
        resolved
            .iter()
            .find(|(candidate, _)| candidate == dir)
            .map(|(_, root)| root)
    };

    let mut placed: Vec<(PathBuf, Vec<AgentRef>)> = Vec::new();
    let mut notes: Vec<Note> = Vec::new();
    let place = |placed: &mut Vec<(PathBuf, Vec<AgentRef>)>, root: &PathBuf, agent: &AgentRef| {
        match placed.iter_mut().find(|(seen, _)| seen == root) {
            // One entry per pane. Two agents sharing a display name are two
            // agents; the same pane reached twice is one.
            Some((_, agents)) => {
                if !agents.iter().any(|a| a.pane_id == agent.pane_id) {
                    agents.push(agent.clone());
                }
            }
            None => placed.push((root.clone(), vec![agent.clone()])),
        }
    };

    for workspace in workspaces {
        // The checkouts this workspace actually touches, in path order.
        let mut spans: Vec<&PathBuf> = Vec::new();
        for path in &workspace.paths {
            if let Some(root) = checkout_of(path) {
                if !spans.contains(&root) {
                    spans.push(root);
                }
            }
        }

        let mut unplaceable = 0usize;
        for agent in &workspace.agents {
            match agent.cwd.as_deref().and_then(checkout_of) {
                Some(root) => place(&mut placed, root, agent),
                None => match spans.as_slice() {
                    [only] => place(&mut placed, only, agent),
                    [] => {}
                    _ => unplaceable += 1,
                },
            }
        }

        if unplaceable > 0 {
            notes.push(Note::warning(format!(
                "workspace {:?} spans {} checkouts and herdr did not say which of them {} \
                 worked in, so that work is credited to none of them",
                workspace.label,
                spans.len(),
                quantity(unplaceable, "agent", "agents"),
            )));
        }
    }

    (placed, notes)
}

/// Adds every other checkout of every repository already present.
///
/// Failures are notes rather than errors: a repository whose worktree list
/// cannot be read still has its open checkouts reported, and saying so is
/// better than dropping it.
fn expand_siblings(
    git: &Git,
    checkouts: &mut Vec<(CheckoutId, Vec<WorkspaceRef>)>,
    notes: &mut Vec<Note>,
) {
    let seeds: Vec<CheckoutId> = {
        let mut seen: Vec<RepoKey> = Vec::new();
        let mut seeds = Vec::new();
        for (id, _) in checkouts.iter() {
            if !seen.contains(&id.repo_key) {
                seen.push(id.repo_key.clone());
                seeds.push(id.clone());
            }
        }
        seeds
    };
    for seed in seeds {
        let siblings = match git.worktrees(&seed) {
            Ok(siblings) => siblings,
            Err(err) => {
                notes.push(Note::warning(format!(
                    "could not list worktrees of {}: {err}",
                    seed.repo_root.display()
                )));
                continue;
            }
        };
        for path in siblings {
            if checkouts.iter().any(|(id, _)| id.path == path) {
                continue;
            }
            match git.identify(&path) {
                Ok(Some(id)) => merge_checkout(checkouts, id, Vec::new()),
                // A pruned or moved worktree: git listed it, the directory is
                // gone. Not worth a warning; it is not where any agent worked.
                Ok(None) => {}
                Err(err) => notes.push(Note::warning(format!(
                    "could not inspect worktree {}: {err}",
                    path.display()
                ))),
            }
        }
    }
}

fn merge_checkout(
    checkouts: &mut Vec<(CheckoutId, Vec<WorkspaceRef>)>,
    id: CheckoutId,
    mut workspaces: Vec<WorkspaceRef>,
) {
    if let Some((_, existing)) = checkouts.iter_mut().find(|(seen, _)| seen.path == id.path) {
        for workspace in workspaces {
            if !existing
                .iter()
                .any(|w| w.workspace_id == workspace.workspace_id)
            {
                existing.push(workspace);
            }
        }
        return;
    }
    workspaces.dedup_by(|a, b| a.workspace_id == b.workspace_id);
    checkouts.push((id, workspaces));
}

/// Distinct commits and the union of touched paths across a repository's
/// checkouts. Two worktrees of one repository share history, so a commit
/// visible from both must be counted once — summing per-checkout totals would
/// silently double the day's output.
///
/// The excluded count is recomputed here rather than summed, for the same
/// reason: it is the size of a union, and it is a pure function of the path, so
/// testing each path in the union is both cheaper and correct where adding up
/// per-checkout counts would double a lockfile touched in two worktrees.
pub(crate) fn rollup(repo: &mut RepoDigest, ignored: &Ignored) {
    let mut seen_commits: HashSet<&str> = HashSet::new();
    let mut files: HashSet<&str> = HashSet::new();
    let mut days: HashSet<&str> = HashSet::new();
    let mut churn = Churn::default();
    for checkout in &repo.checkouts {
        for commit in &checkout.report.commits {
            if !seen_commits.insert(&commit.oid) {
                continue;
            }
            churn.insertions += commit.insertions;
            churn.deletions += commit.deletions;
            // The local date out of the rendered stamp, which is already
            // formatted through `localtime_r`: the day this commit belongs to is
            // the day a reader would see beside it, not a UTC one.
            let day = commit
                .committed
                .local
                .split(' ')
                .next()
                .unwrap_or(&commit.committed.local);
            days.insert(day);
            for file in &commit.files {
                files.insert(file);
            }
        }
    }
    churn.files = files.len();
    churn.excluded = files.iter().filter(|file| ignored.matches(file)).count();
    repo.commits = seen_commits.len();
    repo.active_days = days.len();
    repo.churn = churn;
}

/// Most recent commit instant in a checkout, for ordering. Zero when quiet,
/// which sorts it last among equals.
fn latest(checkout: &CheckoutDigest) -> i64 {
    checkout
        .report
        .commits
        .iter()
        .map(|c| c.committed.epoch)
        .max()
        .unwrap_or(0)
}

fn display_name(id: &CheckoutId) -> String {
    id.repo_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| id.repo_root.display().to_string())
}

/// `workspace "notes": ` — so a skipped directory can be traced back to the
/// workspace it belongs to without the reader having to guess.
fn label_for(owners: &[(PathBuf, WorkspaceRef)], path: &PathBuf) -> String {
    match owners.iter().find(|(candidate, _)| candidate == path) {
        Some((_, workspace)) => format!("workspace {:?}: ", workspace.label),
        None => String::new(),
    }
}

fn push_unique(list: &mut Vec<PathBuf>, path: PathBuf) {
    if !list.contains(&path) {
        list.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Commit, Dirty, Head, Landed, RepoKey, Severity, Stamp, Tracking, Unpushed};

    fn agent(pane: &str, name: Option<&str>, program: &str, cwd: Option<&str>) -> AgentRef {
        AgentRef {
            name: name.map(str::to_string),
            program: Some(program.to_string()),
            session_id: None,
            pane_id: pane.to_string(),
            status: None,
            cwd: cwd.map(PathBuf::from),
        }
    }

    fn workspace(label: &str, paths: &[&str], agents: Vec<AgentRef>) -> WorkspaceRef {
        WorkspaceRef {
            workspace_id: format!("ws-{label}"),
            label: label.to_string(),
            number: None,
            paths: paths.iter().map(PathBuf::from).collect(),
            agents,
            agent_status: None,
        }
    }

    fn resolved(pairs: &[(&str, &str)]) -> Vec<(PathBuf, PathBuf)> {
        pairs
            .iter()
            .map(|(dir, root)| (PathBuf::from(dir), PathBuf::from(root)))
            .collect()
    }

    fn credited<'a>(placed: &'a [(PathBuf, Vec<AgentRef>)], root: &str) -> Vec<&'a str> {
        placed
            .iter()
            .find(|(path, _)| path == &PathBuf::from(root))
            .map(|(_, agents)| agents.iter().map(|a| a.pane_id.as_str()).collect())
            .unwrap_or_default()
    }

    fn rollup_checkout(
        path: &str,
        is_linked_worktree: bool,
        commits: Vec<Commit>,
    ) -> CheckoutDigest {
        CheckoutDigest {
            report: crate::model::CheckoutReport {
                path: PathBuf::from(path),
                repo_key: RepoKey("/repo/.git".to_string()),
                repo_root: PathBuf::from("/repo"),
                is_linked_worktree,
                head: Head::Branch {
                    name: "main".to_string(),
                    oid: "0000000000000000000000000000000000000000".to_string(),
                },
                commits,
                churn: Churn::default(),
                dirty: Dirty::default(),
                tracking: Tracking::NoUpstream,
                landed: Landed::IsDefault {
                    name: "main".to_string(),
                },
                unpushed: Unpushed::Commits { count: 0 },
                problems: Vec::new(),
            },
            workspaces: Vec::new(),
            agents: Vec::new(),
        }
    }

    #[test]
    fn large_overlapping_worktrees_roll_up_distinct_commits_files_and_days() {
        const DISTINCT: usize = 4_096;
        let mut commits = Vec::with_capacity(DISTINCT);
        for index in 0..DISTINCT {
            // Every generated date is calendar-valid and unique: limiting each
            // month to 28 days avoids a date dependency in this deterministic
            // fixture while still exercising thousands of day memberships.
            let year = 2000 + index / (12 * 28);
            let month = index / 28 % 12 + 1;
            let day = index % 28 + 1;
            let file = if index % 8 == 0 {
                format!("generated/{index}/Cargo.lock")
            } else {
                format!("src/file-{index:04}.rs")
            };
            commits.push(Commit {
                oid: format!("{index:040x}"),
                author: "Fixture".to_string(),
                committed: Stamp {
                    epoch: index as i64,
                    local: format!("{year:04}-{month:02}-{day:02} 12:00"),
                    zone: "UTC +0000".to_string(),
                    offset_seconds: Some(0),
                },
                subject: format!("Generated commit {index}"),
                is_merge: false,
                insertions: 2,
                deletions: 1,
                files: vec![file],
            });
        }

        let mut repo = RepoDigest {
            repo_key: RepoKey("/repo/.git".to_string()),
            name: "repo".to_string(),
            repo_root: PathBuf::from("/repo"),
            checkouts: vec![
                rollup_checkout("/repo", false, commits.clone()),
                rollup_checkout("/repo-worktree", true, commits),
            ],
            commits: 0,
            churn: Churn::default(),
            active_days: 0,
        };

        rollup(&mut repo, &Ignored::default());

        assert_eq!(repo.commits, DISTINCT);
        assert_eq!(repo.active_days, DISTINCT);
        assert_eq!(
            repo.churn,
            Churn {
                files: DISTINCT,
                excluded: DISTINCT / 8,
                insertions: (DISTINCT * 2) as u64,
                deletions: DISTINCT as u64,
            }
        );
    }

    /// The headline of #19: two agents in one window, in one checkout, are two
    /// agents. They share a program and neither is named, which is the shape the
    /// live capture makes likely — `claude` on all but one of eighteen rows, and
    /// three of them unnamed — and it is exactly the shape that collapsed when
    /// agents were deduplicated by display name.
    #[test]
    fn two_unnamed_agents_sharing_a_checkout_stay_two_agents() {
        let workspaces = vec![workspace(
            "atlas",
            &["/code/atlas"],
            vec![
                agent("w1:p1", None, "claude", Some("/code/atlas")),
                agent("w1:p2", None, "claude", Some("/code/atlas")),
            ],
        )];
        let (placed, notes) =
            place_agents(&workspaces, &resolved(&[("/code/atlas", "/code/atlas")]));

        assert_eq!(credited(&placed, "/code/atlas"), ["w1:p1", "w1:p2"]);
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// A workspace is not a place. Its panes can sit in different checkouts, and
    /// crediting every agent to every one of them is a guess presented as a fact.
    #[test]
    fn a_workspace_spanning_two_checkouts_credits_each_agent_where_it_worked() {
        let workspaces = vec![workspace(
            "atlas",
            &["/code/atlas", "/code/atlas-wt"],
            vec![
                agent("w1:p1", Some("kestrel"), "claude", Some("/code/atlas")),
                agent("w1:p2", Some("wren"), "claude", Some("/code/atlas-wt")),
            ],
        )];
        let (placed, notes) = place_agents(
            &workspaces,
            &resolved(&[
                ("/code/atlas", "/code/atlas"),
                ("/code/atlas-wt", "/code/atlas-wt"),
            ]),
        );

        assert_eq!(credited(&placed, "/code/atlas"), ["w1:p1"]);
        assert_eq!(credited(&placed, "/code/atlas-wt"), ["w1:p2"]);
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// An agent's directory need not be the checkout root, so the answer comes
    /// from git's own resolution of the candidate rather than a path comparison.
    #[test]
    fn an_agent_in_a_subdirectory_is_credited_to_the_checkout() {
        let workspaces = vec![workspace(
            "atlas",
            &["/code/atlas/crates/core"],
            vec![agent(
                "w1:p1",
                Some("kestrel"),
                "claude",
                Some("/code/atlas/crates/core"),
            )],
        )];
        let (placed, notes) = place_agents(
            &workspaces,
            &resolved(&[("/code/atlas/crates/core", "/code/atlas")]),
        );

        assert_eq!(credited(&placed, "/code/atlas"), ["w1:p1"]);
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// `cwd` is optional in herdr's protocol. With one checkout in the workspace
    /// there is nowhere else the agent could have been, so this is still a fact
    /// rather than a guess and gets no note.
    #[test]
    fn an_agent_with_no_directory_is_placed_when_there_is_only_one_candidate() {
        let workspaces = vec![workspace(
            "atlas",
            &["/code/atlas"],
            vec![agent("w1:p1", Some("kestrel"), "claude", None)],
        )];
        let (placed, notes) =
            place_agents(&workspaces, &resolved(&[("/code/atlas", "/code/atlas")]));

        assert_eq!(credited(&placed, "/code/atlas"), ["w1:p1"]);
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// The case the issue insists on: unknowable, so it reads as unknown. The
    /// agent is credited to neither checkout and the digest says why, rather than
    /// being spread across both or silently attached to the last one seen.
    #[test]
    fn an_unplaceable_agent_is_credited_to_nobody_and_said_out_loud() {
        let workspaces = vec![workspace(
            "atlas",
            &["/code/atlas", "/code/atlas-wt"],
            vec![
                agent("w1:p1", Some("kestrel"), "claude", Some("/code/atlas")),
                agent("w1:p2", Some("wren"), "claude", None),
            ],
        )];
        let (placed, notes) = place_agents(
            &workspaces,
            &resolved(&[
                ("/code/atlas", "/code/atlas"),
                ("/code/atlas-wt", "/code/atlas-wt"),
            ]),
        );

        assert_eq!(credited(&placed, "/code/atlas"), ["w1:p1"]);
        assert!(
            credited(&placed, "/code/atlas-wt").is_empty(),
            "an unknown directory must not become a credit"
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].severity, Severity::Warning);
        assert!(notes[0].message.contains("atlas"), "{:?}", notes[0].message);
        assert!(notes[0].message.contains('2'), "{:?}", notes[0].message);
        assert!(
            notes[0].message.contains("1 agent"),
            "the count must be exact: {:?}",
            notes[0].message
        );
    }

    /// A directory that is not a checkout resolves to nothing, and an agent
    /// sitting in one is not evidence about any repository.
    #[test]
    fn an_agent_outside_every_checkout_is_credited_nowhere() {
        let workspaces = vec![workspace(
            "notes",
            &["/home/dev/notes"],
            vec![agent(
                "w1:p1",
                Some("kestrel"),
                "claude",
                Some("/home/dev/notes"),
            )],
        )];
        let (placed, notes) = place_agents(&workspaces, &resolved(&[]));

        assert!(placed.is_empty(), "{placed:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }
}
