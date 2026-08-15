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

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::clock;
use crate::config::Config;
use crate::git::{CheckoutId, Git};
use crate::herdr::Herdr;
use crate::model::{
    Activity, CheckoutDigest, Churn, Digest, Note, RepoDigest, RepoKey, WorkspaceRef,
    SCHEMA_VERSION,
};
use crate::window;
use crate::Result;

/// Builds the digest. Everything that can degrade does, and says so in
/// `digest.notes`; only a failure that would make the whole report a lie is
/// returned as an error.
pub fn build(config: &Config) -> Result<Digest> {
    let git = Git::new(config.git_timeout);
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
    for path in &config.extra_paths {
        push_unique(&mut candidates, path.clone());
    }

    // Identify. A directory that is not a repository is ordinary data — most
    // sessions have at least one — but it is still worth one line, because
    // "standup did not mention that workspace" should never be a mystery.
    let mut checkouts: Vec<(CheckoutId, Vec<WorkspaceRef>)> = Vec::new();
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

    // Report, then group. Grouping by repository is the deliberate choice: see
    // `model::RepoDigest`.
    let mut by_repo: BTreeMap<RepoKey, RepoDigest> = BTreeMap::new();
    for (id, workspaces) in checkouts {
        let report = git.report(&id, &reporting_window);
        let entry = by_repo
            .entry(id.repo_key.clone())
            .or_insert_with(|| RepoDigest {
                repo_key: id.repo_key.clone(),
                name: display_name(&id),
                repo_root: id.repo_root.clone(),
                checkouts: Vec::new(),
                commits: 0,
                churn: Churn::default(),
            });
        entry.checkouts.push(CheckoutDigest { report, workspaces });
    }

    let mut repos: Vec<RepoDigest> = by_repo.into_values().collect();
    for repo in &mut repos {
        rollup(repo);
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
    match Herdr::connect().and_then(|mut herdr| herdr.workspaces()) {
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
fn rollup(repo: &mut RepoDigest) {
    let mut seen_commits: Vec<&str> = Vec::new();
    let mut files: Vec<&str> = Vec::new();
    let mut churn = Churn::default();
    for checkout in &repo.checkouts {
        for commit in &checkout.report.commits {
            if seen_commits.contains(&commit.oid.as_str()) {
                continue;
            }
            seen_commits.push(&commit.oid);
            churn.insertions += commit.insertions;
            churn.deletions += commit.deletions;
            for file in &commit.files {
                if !files.contains(&file.as_str()) {
                    files.push(file);
                }
            }
        }
    }
    churn.files = files.len();
    repo.commits = seen_commits.len();
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

/// `workspace 3 "herdr-shear": ` — so a skipped directory can be traced back to
/// the workspace it belongs to without the reader having to guess.
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
