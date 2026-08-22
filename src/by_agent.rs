//! Regrouping a digest by agent.
//!
//! # Why this is opt-in
//!
//! Grouping by **repository** is the deliberate default, and the reason is in
//! `model::RepoDigest`: grouping by time would put two commits from one branch
//! on opposite sides of an unrelated project's commit. Agent grouping has the
//! same hazard from the other direction — one agent's work is spread across
//! repositories, so an agent group interleaves projects that have nothing to do
//! with each other.
//!
//! It also has a hazard of its own, and this one is about the numbers rather
//! than the reading order.
//!
//! # A commit cannot be split, and reaches two groups two ways
//!
//! Agents are placed per **checkout**, which is as fine-grained as herdr's data
//! goes: a commit's author identity is shared by every agent on the machine —
//! "three agents, one author identity" is the normal shape here — so there is
//! nothing in git or in the snapshot that says which of two agents sharing a
//! checkout wrote which commit. Its commits are therefore counted under **both**.
//!
//! The second route is less obvious and was found by running this against a live
//! session rather than reasoned about: two **checkouts of one repository** in
//! different groups. Worktrees share history, so a commit visible from both is
//! counted in each — with one agent per checkout, which is the ordinary case.
//!
//! Either way the per-agent totals add up to more than the digest's. That is the
//! honest answer rather than splitting a commit down the middle or picking
//! whichever agent herdr mentioned last — and because it is a number that no
//! longer reconciles, the digest says so out loud, with the difference measured
//! rather than described. See [`Grouping::double_counted`].
//!
//! Nothing here recollects anything. It is a pure regrouping of a digest that has
//! already been built, so every number in it came from the same place as the
//! numbers in the ungrouped form.
use crate::config::Ignored;
use crate::model::{AgentRef, Churn, Digest, RepoDigest};
use crate::render::quantity;

/// One agent, and the work placed in the checkouts they occupied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGroup {
    /// `None` is the group for checkouts no agent was placed in: a sibling
    /// worktree whose workspace was closed, or an agent herdr could not place.
    /// Reported rather than dropped — the work happened.
    pub agent: Option<AgentRef>,
    /// What to call this group. For an agent, the same label the ungrouped digest
    /// credits; for `None`, a sentence rather than a name.
    pub label: String,
    /// The repositories this agent touched, each carrying only the checkouts the
    /// agent was placed in, with the totals recomputed over that subset by the
    /// same union rule the ungrouped digest uses.
    pub repos: Vec<RepoDigest>,
    pub commits: usize,
    pub churn: Churn,
}

/// A digest regrouped by agent, and what is not reconcilable about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grouping {
    pub groups: Vec<AgentGroup>,
    /// How many checkouts had more than one agent placed in them.
    pub shared: usize,
    /// How many more commits the per-agent totals add up to than the digest's
    /// own, which is the number a reader would otherwise have to spot.
    ///
    /// Measured rather than reasoned about, because there are **two** ways for a
    /// commit to land in more than one group and only one of them is obvious:
    ///
    /// 1. Two agents shared a checkout, so it belongs to both their groups.
    /// 2. Two *checkouts of one repository* are in different groups. Worktrees
    ///    share history, so a commit visible from both is counted in each — and
    ///    this happens with one agent per checkout, which is the ordinary case.
    ///
    /// The second was found by running the grouping against a live session and
    /// noticing the totals did not add up. Comparing the sums catches both
    /// without having to enumerate the reasons.
    pub double_counted: usize,
}

/// Regroups a built digest by agent.
pub fn group(digest: &Digest, ignored: &Ignored) -> Grouping {
    // Agents in first-seen order across the digest, which is already sorted, so
    // the grouping is stable between runs without sorting again by name.
    let mut order: Vec<(String, Option<AgentRef>)> = Vec::new();
    let mut shared = 0usize;

    for repo in &digest.repos {
        for checkout in &repo.checkouts {
            let placed = &checkout.agents;
            if placed.len() > 1 {
                shared += 1;
            }
            if placed.is_empty() {
                remember(&mut order, UNATTRIBUTED.to_string(), None);
                continue;
            }
            for agent in placed {
                let label = agent
                    .display()
                    .map(str::to_string)
                    .unwrap_or_else(|| UNATTRIBUTED.to_string());
                let agent = agent.display().map(|_| agent.clone());
                remember(&mut order, label, agent);
            }
        }
    }

    let groups: Vec<AgentGroup> = order
        .into_iter()
        .map(|(label, agent)| {
            let repos = repos_for(digest, &label, ignored);
            let commits = repos.iter().map(|repo| repo.commits).sum();
            let mut churn = Churn::default();
            for repo in &repos {
                churn.add(repo.churn);
            }
            AgentGroup {
                agent,
                label,
                repos,
                commits,
                churn,
            }
        })
        .collect();

    // Measured against the digest's own total rather than inferred from the
    // shapes above, because a commit reaches two groups by two different routes
    // and only one of them is obvious. See `Grouping::double_counted`.
    let grouped: usize = groups.iter().map(|group| group.commits).sum();
    let double_counted = grouped.saturating_sub(digest.total_commits());

    Grouping {
        groups,
        shared,
        double_counted,
    }
}

/// The label for work no agent was placed in. A sentence rather than a name, so
/// it cannot be mistaken for an agent called "unattributed".
pub const UNATTRIBUTED: &str = "no agent reported";

fn remember(order: &mut Vec<(String, Option<AgentRef>)>, label: String, agent: Option<AgentRef>) {
    if order.iter().any(|(seen, _)| *seen == label) {
        return;
    }
    order.push((label, agent));
}

/// The repositories one group covers, each filtered to the checkouts that belong
/// to it and re-totalled over exactly those.
///
/// Re-totalled rather than carried over: a repository's `commits` is the count of
/// *distinct* commits across its checkouts, so taking the whole repository's
/// number for a group holding one of its three checkouts would report work the
/// agent did not do.
fn repos_for(digest: &Digest, label: &str, ignored: &Ignored) -> Vec<RepoDigest> {
    let mut out: Vec<RepoDigest> = Vec::new();
    for repo in &digest.repos {
        let checkouts: Vec<_> = repo
            .checkouts
            .iter()
            .filter(|checkout| belongs(checkout, label))
            .cloned()
            .collect();
        if checkouts.is_empty() {
            continue;
        }
        let mut filtered = RepoDigest {
            checkouts,
            ..repo.clone()
        };
        retotal(&mut filtered, ignored);
        out.push(filtered);
    }
    out
}

fn belongs(checkout: &crate::model::CheckoutDigest, label: &str) -> bool {
    if checkout.agents.is_empty() {
        return label == UNATTRIBUTED;
    }
    checkout
        .agents
        .iter()
        .any(|agent| agent.display().unwrap_or(UNATTRIBUTED) == label)
}

/// The same rule `standup::rollup` applies, over a subset of one repository's
/// checkouts: distinct commits, the union of touched paths, and the excluded
/// count recomputed over that union rather than summed.
fn retotal(repo: &mut RepoDigest, ignored: &Ignored) {
    let mut seen: Vec<&str> = Vec::new();
    let mut files: Vec<&str> = Vec::new();
    let mut days: Vec<&str> = Vec::new();
    let mut churn = Churn::default();
    for checkout in &repo.checkouts {
        for commit in &checkout.report.commits {
            if seen.contains(&commit.oid.as_str()) {
                continue;
            }
            seen.push(&commit.oid);
            churn.insertions += commit.insertions;
            churn.deletions += commit.deletions;
            let day = commit
                .committed
                .local
                .split(' ')
                .next()
                .unwrap_or(&commit.committed.local);
            if !days.contains(&day) {
                days.push(day);
            }
            for file in &commit.files {
                if !files.contains(&file.as_str()) {
                    files.push(file);
                }
            }
        }
    }
    churn.files = files.len();
    churn.excluded = files.iter().filter(|file| ignored.matches(file)).count();
    let (commits, active_days) = (seen.len(), days.len());
    repo.commits = commits;
    repo.active_days = active_days;
    repo.churn = churn;
}

impl Grouping {
    /// The sentence every renderer prints under the header.
    ///
    /// Two things a reader has to know before trusting a per-agent number, and
    /// neither is obvious from looking at the output. The second is stated with
    /// the **measured** difference rather than a general warning, because a
    /// number that no longer reconciles should say by how much.
    pub fn caveat(&self) -> String {
        let mut caveat = "grouped by agent, which interleaves unrelated projects \u{2014} \
                          the default grouping is by repository"
            .to_string();
        if self.double_counted > 0 {
            caveat.push_str(&format!(
                ". These totals add up to {} more than the digest's, because a commit cannot \
                 be split between two agents sharing a checkout, or between two checkouts of \
                 one repository in different groups, so it is counted in each",
                quantity(self.double_counted, "commit", "commits")
            ));
        } else if self.shared > 0 {
            caveat.push_str(&format!(
                ". {} shared with another agent, so those commits are counted under each",
                quantity_checkouts(self.shared)
            ));
        }
        caveat
    }
}

fn quantity_checkouts(count: usize) -> String {
    match count {
        1 => "1 checkout".to_string(),
        many => format!("{many} checkouts"),
    }
}
