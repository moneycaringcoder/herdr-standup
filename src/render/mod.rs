//! The three renderers.
//!
//! The output *is* the product. All three take exactly one [`Digest`] and add
//! nothing to it — no second pass over git, no lookups, no interpretation. If a
//! renderer wants a number, the number belongs in the model.
//!
//! Quality bar, from the roadmap:
//!
//! - A checkout with nothing in the window is **summarised as quiet**, never
//!   omitted and never padded out with a row of zeros.
//! - Times are local and say so. Every [`Stamp`](crate::model::Stamp) carries
//!   its zone; print it.
//! - Long branch names and paths must not wrap into soup at 80 columns.
//! - The Markdown must render on GitHub even when a branch name contains
//!   characters Markdown treats specially — `feat/re[factor]`, `a|b`,
//!   `fix_*_thing` and backticks all occur in real branch names.
//! - Anything in `problems` or `notes` is always shown. A digest that hides a
//!   failure is worse than no digest.
//!
//! This module holds what the two human formats share: the width arithmetic,
//! the truncation rules, and — more importantly — the *sentences*. "Not merged"
//! and "no default branch to compare against" are phrased in exactly one place,
//! so the terminal report and the pasted Markdown can never drift into saying
//! different things about the same checkout.

mod markdown;
mod text;

pub use markdown::markdown;
pub use text::text;

use std::path::Path;

use crate::config::{Config, Format};
use crate::model::{
    Activity, AgentRef, CheckoutDigest, CheckoutReport, Churn, Commit, Digest, Dirty, Equivalence,
    Head, Landed, Period, RepoDigest, Stamp, Tracking, Unpushed, Window, WindowSource,
};
use crate::Result;

/// The width every terminal line is laid out for. A digest is read in a pane
/// beside other panes, so 80 is the budget, not the aspiration.
pub const WIDTH: usize = 80;

/// Object ids are shown at a fixed width. A digest is read as a column, and a
/// variable-width id ruins the alignment; [`Commit::short_oid`] agrees.
pub const OID_COLUMNS: usize = 8;

/// Widest branch column in the terminal report. Past this a branch name is
/// truncated rather than allowed to eat the path beside it.
const BRANCH_COLUMNS: usize = 30;

/// Longest ref name the terminal report allows inside a sentence ("not merged
/// into …"). Beyond this the sentence stops being a sentence.
pub(crate) const REF_COLUMNS: usize = 28;

/// The Markdown has no column budget to spend, so it spends none: a ref name
/// pasted into a channel is something a reader may need to type back.
pub(crate) const REF_UNLIMITED: usize = usize::MAX;

const ELLIPSIS: char = '\u{2026}'; // …
const MINUS: char = '\u{2212}'; // −
const MIDDOT: char = '\u{b7}'; // ·

/// Renders in whichever format the config asks for.
pub fn render(digest: &Digest, config: &Config) -> Result<String> {
    match config.format {
        Format::Text => Ok(text(digest, config)),
        Format::Markdown => Ok(markdown(digest, config)),
        Format::Json => json(digest),
    }
}

/// Machine-readable digest. Stable shape, versioned by
/// [`SCHEMA_VERSION`](crate::model::SCHEMA_VERSION).
pub fn json(digest: &Digest) -> Result<String> {
    Ok(serde_json::to_string_pretty(digest)?)
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------
//
// `standup::build` already sorts what it produces, but a renderer that depends
// on its input being sorted is a renderer that breaks the first time somebody
// builds a `Digest` by hand — which is exactly what the tests do. Sorting is
// ordering, not interpretation: nothing here invents a number.

/// Repositories, loudest first: broken, then busiest, then alphabetical.
pub(crate) fn sorted_repos(digest: &Digest) -> Vec<&RepoDigest> {
    let mut repos: Vec<&RepoDigest> = digest.repos.iter().collect();
    repos.sort_by(|a, b| {
        b.activity()
            .cmp(&a.activity())
            .then_with(|| b.commits.cmp(&a.commits))
            .then_with(|| b.churn.insertions.cmp(&a.churn.insertions))
            .then_with(|| a.name.cmp(&b.name))
    });
    repos
}

/// Checkouts within one repository, on the same rule.
pub(crate) fn sorted_checkouts(repo: &RepoDigest) -> Vec<&CheckoutDigest> {
    let mut checkouts: Vec<&CheckoutDigest> = repo.checkouts.iter().collect();
    checkouts.sort_by(|a, b| {
        b.report
            .activity()
            .cmp(&a.report.activity())
            .then_with(|| b.report.commits.len().cmp(&a.report.commits.len()))
            .then_with(|| latest(b).cmp(&latest(a)))
            .then_with(|| a.report.path.cmp(&b.report.path))
    });
    checkouts
}

fn latest(checkout: &CheckoutDigest) -> i64 {
    checkout
        .report
        .commits
        .iter()
        .map(|c| c.committed.epoch)
        .max()
        .unwrap_or(0)
}

/// Commits newest first. When `max_commits` cuts the list it is the oldest that
/// go, and the caller says how many.
pub(crate) fn sorted_commits(report: &CheckoutReport) -> Vec<&Commit> {
    let mut commits: Vec<&Commit> = report.commits.iter().collect();
    commits.sort_by(|a, b| {
        b.committed
            .epoch
            .cmp(&a.committed.epoch)
            .then_with(|| a.oid.cmp(&b.oid))
    });
    commits
}

/// Splits a commit list at `max_commits`, returning what to show and how many
/// were held back. A remainder is always reported as a count, never dropped.
pub(crate) fn split_commits<'a>(commits: &[&'a Commit], max: usize) -> (Vec<&'a Commit>, usize) {
    if commits.len() <= max {
        return (commits.to_vec(), 0);
    }
    (commits[..max].to_vec(), commits.len() - max)
}

// ---------------------------------------------------------------------------
// Sentences
// ---------------------------------------------------------------------------

/// "1 commit", "2 commits" — never `1 commits` in a pasted standup.
pub(crate) fn quantity(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// `7 commits`, `1 commit`, `no commits`. Zero gets a word rather than a digit:
/// a repository line reading `0 commits` is the row of zeros the roadmap asks
/// us not to print.
pub(crate) fn commit_count(commits: usize) -> String {
    if commits == 0 {
        "no commits".to_string()
    } else {
        quantity(commits, "commit", "commits")
    }
}

/// `+812 −140`, with a real minus sign so it lines up under the plus.
pub(crate) fn delta(insertions: u64, deletions: u64) -> String {
    format!("+{insertions} {MINUS}{deletions}")
}

/// `23 files`, or `23 files (1 generated)` when some of them contributed no
/// lines.
///
/// The parenthesis is the whole of "visible rather than silently applied". A
/// line total quietly smaller than the diff is precisely the kind of number this
/// plugin exists not to print, and a reader who sees the count can go and look
/// at what was left out.
///
/// Excluded paths still count as **files**, because the commit really did touch
/// them. That is the same treatment a binary file has always had.
pub(crate) fn files_count(churn: Churn) -> String {
    let files = quantity(churn.files, "file", "files");
    match churn.excluded {
        0 => files,
        excluded => format!("{files} ({excluded} generated)"),
    }
}

/// The stat block that sits at the right-hand end of a repository line:
/// `7 commits  ·  23 files  +812 −140`.
pub(crate) fn repo_stats(commits: usize, churn: Churn) -> String {
    let mut out = commit_count(commits);
    if !churn.is_zero() {
        out.push_str(&format!(
            "  {MIDDOT}  {}  {}",
            files_count(churn),
            delta(churn.insertions, churn.deletions)
        ));
    }
    out
}

/// What goes in the branch column. Detachment is visible here; the states that
/// need explaining get a sentence of their own from [`head_note`].
pub(crate) fn head_label(head: &Head) -> String {
    match head {
        Head::Detached { oid } => format!("(detached at {})", abbrev_oid(oid)),
        _ => match head.branch_name().map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) => name.to_string(),
            None => "(unnamed branch)".to_string(),
        },
    }
}

/// The half of a `Head` that a branch name cannot carry. The flag says whether
/// this is a problem the reader has to act on, so both renderers mark it the
/// same way.
pub(crate) fn head_note(head: &Head, ref_max: usize) -> Option<(String, bool)> {
    match head {
        Head::Branch { .. } | Head::Detached { .. } => None,
        Head::Unborn { .. } => Some((
            "unborn branch \u{2014} no commits here yet".to_string(),
            false,
        )),
        Head::BranchDeleted { name } => Some((
            format!(
                "the branch {} was deleted underneath this checkout",
                ref_name(name, ref_max)
            ),
            true,
        )),
    }
}

/// Fixed-width abbreviation, matching [`Commit::short_oid`].
pub(crate) fn abbrev_oid(oid: &str) -> &str {
    let end = oid.len().min(OID_COLUMNS);
    &oid[..end]
}

/// Whether the work has landed on the default branch. Five states, five
/// sentences — and never a bare `false`, which would read as "did not land"
/// when what we mean is "there was nothing to compare against".
///
/// "Merged" is kept for containment, which is exact. A squash or a rebase
/// merge only leaves a matching patch behind, and a matching patch is strong
/// evidence rather than proof, so those get "by patch, not by sha" and name
/// what was matched — the reader can re-run `git cherry` or `git patch-id` and
/// see the same thing.
pub(crate) fn landed_sentence(landed: &Landed, ref_max: usize) -> String {
    match landed {
        Landed::IsDefault { name } => format!("on the default branch {}", ref_name(name, ref_max)),
        Landed::Merged { into } => format!("merged into {}", ref_name(into, ref_max)),
        Landed::Equivalent {
            into,
            how: Equivalence::EveryCommit { .. },
        } => format!(
            "every commit is on {} by patch, not by sha",
            ref_name(into, ref_max)
        ),
        Landed::Equivalent {
            into,
            how: Equivalence::Squashed { oid },
        } => format!(
            "on {} by patch as {}, not by sha",
            ref_name(into, ref_max),
            abbrev_oid(oid)
        ),
        Landed::NotMerged { into } => format!("not merged into {}", ref_name(into, ref_max)),
        Landed::Unknown { reason } => format!("merge status unknown: {reason}"),
    }
}

/// Upstream state. "Nobody is watching this branch" and "the remote branch was
/// deleted" are different facts and get different words.
pub(crate) fn tracking_sentence(tracking: &Tracking, ref_max: usize) -> String {
    match tracking {
        Tracking::NotApplicable => "no branch to track".to_string(),
        Tracking::NoUpstream => "no upstream".to_string(),
        Tracking::UpstreamMissing { name } => {
            format!("upstream {} no longer exists", ref_name(name, ref_max))
        }
        Tracking::Upstream {
            name,
            ahead: 0,
            behind: 0,
        } => format!("in sync with {}", ref_name(name, ref_max)),
        Tracking::Upstream {
            name,
            ahead,
            behind: 0,
        } => format!("{ahead} ahead of {}", ref_name(name, ref_max)),
        Tracking::Upstream {
            name,
            ahead: 0,
            behind,
        } => format!("{behind} behind {}", ref_name(name, ref_max)),
        Tracking::Upstream {
            name,
            ahead,
            behind,
        } => format!("{ahead} ahead, {behind} behind {}", ref_name(name, ref_max)),
    }
}

/// Work sitting in the checkout right now, uncommitted. `None` when the
/// checkout is clean, so the caller prints nothing rather than a row of zeros.
pub(crate) fn dirty_sentence(dirty: &Dirty) -> Option<String> {
    if dirty.is_clean() {
        return None;
    }
    let mut parts = Vec::new();
    if dirty.tracked_changed > 0 {
        parts.push(format!(
            "{} changed",
            quantity(dirty.tracked_changed, "file", "files")
        ));
    }
    if dirty.untracked > 0 {
        parts.push(format!("{} untracked", dirty.untracked));
    }
    if dirty.conflicted > 0 {
        parts.push(format!(
            "{} conflicted",
            quantity(dirty.conflicted, "file", "files")
        ));
    }
    if dirty.insertions > 0 || dirty.deletions > 0 {
        parts.push(delta(dirty.insertions, dirty.deletions));
    }
    Some(format!("uncommitted: {}", parts.join(", ")))
}

/// Work that exists in this checkout and nowhere else.
///
/// `None` when there is none of it, and when there is no remote for it to have
/// been pushed to — a repository nobody ever pushed is not holding work at risk
/// of a failed push, and saying so of every local-only scratch repository would
/// bury the case worth reading.
///
/// `None` for [`Unpushed::Unknown`] too, and that is not silence: `git.rs`
/// records the same failure on `report.problems`, which every renderer is
/// obliged to show and which lifts the checkout out of "quiet" so it cannot be
/// summarised away. A second, weaker copy of it here would only compete with
/// that one.
///
/// A line of its own rather than a clause, for the same reason uncommitted work
/// gets one: both are present-tense facts about the directory rather than
/// anything the window measured, and both are lost if the checkout is removed.
/// That is the point of naming this state — it reads at a glance, next to the
/// neighbour it used to be filed under.
pub(crate) fn unpushed_sentence(unpushed: &Unpushed) -> Option<String> {
    match unpushed {
        Unpushed::Commits { count } if *count > 0 => Some(format!(
            "unpushed: {} on no remote",
            quantity(*count as usize, "commit", "commits")
        )),
        Unpushed::Commits { .. } | Unpushed::NoRemote | Unpushed::Unknown { .. } => None,
    }
}

/// The three clauses describing one checkout's window, in reading order: what
/// came out of it, whether it landed, and who is watching it.
///
/// Three, not five: the counts belong together in one clause, so a renderer
/// that separates the clauses with something other than a comma does not end up
/// with `3 commits — 3 files — +30 −12`.
pub(crate) fn checkout_clauses(report: &CheckoutReport, ref_max: usize) -> Vec<String> {
    let mut parts = Vec::new();
    if report.commits.is_empty() {
        parts.push("no commits in this window".to_string());
    } else {
        let merges = report.commits.iter().filter(|c| c.is_merge).count();
        let mut counted = quantity(report.commits.len(), "commit", "commits");
        if merges > 0 {
            // Merges carry no diffstat of their own, so a commit count that is
            // larger than the churn suggests has an explanation, and this is it.
            counted.push_str(&format!(" ({})", quantity(merges, "merge", "merges")));
        }
        if !report.churn.is_zero() {
            counted.push_str(&format!(
                ", {}, {}",
                files_count(report.churn),
                delta(report.churn.insertions, report.churn.deletions)
            ));
        }
        parts.push(counted);
    }
    parts.push(landed_sentence(&report.landed, ref_max));
    parts.push(tracking_sentence(&report.tracking, ref_max));
    parts
}

/// `shear-classifier (claude)`, or whichever half of that herdr reported.
pub(crate) fn agent_label(agent: &AgentRef) -> Option<String> {
    let display = agent.display()?;
    let program = agent
        .program
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    match program {
        Some(program) if program != display => Some(format!("{display} ({program})")),
        _ => Some(display.to_string()),
    }
}

/// Who to credit: the agents herdr placed here, or failing that the commit
/// authors. A sibling worktree whose workspace was closed has no agents left,
/// and "somebody committed this" is still worth saying.
///
/// Agents that share a label are counted rather than repeated. Two unnamed
/// `claude` agents in one checkout are two agents, and `claude, claude` reads
/// like a bug where `claude ×2` reads like the fact it is.
pub(crate) fn attribution(checkout: &CheckoutDigest) -> Option<(&'static str, String)> {
    let mut labelled: Vec<(String, usize)> = Vec::new();
    for agent in &checkout.agents {
        let Some(label) = agent_label(agent) else {
            continue;
        };
        match labelled.iter_mut().find(|(seen, _)| *seen == label) {
            Some((_, count)) => *count += 1,
            None => labelled.push((label, 1)),
        }
    }
    if !labelled.is_empty() {
        let agents: Vec<String> = labelled
            .into_iter()
            .map(|(label, count)| match count {
                1 => label,
                many => format!("{label} \u{d7}{many}"),
            })
            .collect();
        return Some(("agents", agents.join(", ")));
    }
    let authors = checkout.report.authors();
    if !authors.is_empty() {
        return Some(("authors", authors.join(", ")));
    }
    None
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// `2026-08-15` out of `2026-08-15 09:12`.
pub(crate) fn date_of(stamp: &Stamp) -> &str {
    stamp.local.split(' ').next().unwrap_or(&stamp.local)
}

/// `09:12` out of `2026-08-15 09:12`.
pub(crate) fn time_of(stamp: &Stamp) -> &str {
    stamp.local.rsplit(' ').next().unwrap_or(&stamp.local)
}

/// How to print "generated at": the time alone once the header has already
/// established the day and the zone, and the full form otherwise.
pub(crate) fn generated_display(digest: &Digest) -> String {
    let generated = &digest.generated_at;
    let since = &digest.window.since;
    if generated.zone == since.zone && date_of(generated) == date_of(since) {
        time_of(generated).to_string()
    } else {
        generated.full()
    }
}

/// Whether commit lines have to carry their date.
///
/// True as soon as the window spans more than one day — and also if any commit
/// in it is dated differently from the window start, which is the same question
/// asked of the data rather than of the boundaries.
pub(crate) fn dates_needed(digest: &Digest) -> bool {
    let start = date_of(&digest.window.since);
    let end = match &digest.window.until {
        Some(until) => date_of(until),
        None => date_of(&digest.generated_at),
    };
    if start != end {
        return true;
    }
    digest.repos.iter().any(|repo| {
        repo.checkouts.iter().any(|checkout| {
            checkout
                .report
                .commits
                .iter()
                .any(|commit| date_of(&commit.committed) != start)
        })
    })
}

/// How a commit's instant is printed: `09:12` inside a single day, the full
/// local date and time once the window spans more than one.
pub(crate) fn commit_time(stamp: &Stamp, with_date: bool) -> &str {
    if with_date {
        &stamp.local
    } else {
        time_of(stamp)
    }
}

/// Why this window, when the answer is not "the default". A window the user did
/// not get is the first thing to explain when a digest looks empty.
pub(crate) fn window_note(window: &Window) -> Option<String> {
    match &window.source {
        WindowSource::Default => None,
        WindowSource::Explicit { spec } => Some(format!("window from --since \"{spec}\"")),
        WindowSource::SinceLast { previous_run } => Some(format!(
            "window from --since-last; the previous run was {}",
            previous_run.full()
        )),
        WindowSource::SinceLastFirstRun => Some(
            "window from --since-last, but no previous run is on record — \
             showing today instead"
                .to_string(),
        ),
        // Names the boundary convention rather than only the flag. A reader who
        // disagrees about when a week starts needs to know which Monday this is,
        // and the resolved instant is printed on the line above.
        WindowSource::Rollup {
            period: Period::Week,
        } => Some("window from --weekly: this ISO week, Monday to now".to_string()),
        WindowSource::Rollup {
            period: Period::Month,
        } => Some("window from --monthly: this calendar month, the 1st to now".to_string()),
    }
}

/// Whether the digest lists individual commits.
///
/// A rollup does not. "The same data answers what happened this month if it is
/// aggregated rather than listed" is the whole request, and a month listed
/// commit by commit is not a digest — it is a `git log` with extra steps.
pub(crate) fn lists_commits(window: &Window) -> bool {
    !matches!(window.source, WindowSource::Rollup { .. })
}

// ---------------------------------------------------------------------------
// Paths and widths
// ---------------------------------------------------------------------------

/// `~/repos/app` rather than `/home/somebody/repos/app`. Shorter, and it does
/// not put the user's login name in a message they are about to paste.
pub(crate) fn home_relative(path: &Path) -> String {
    let shown = path.to_string_lossy().into_owned();
    let Some(home) = crate::config::non_empty_env("HOME") else {
        return shown;
    };
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return shown;
    }
    if shown == home {
        return "~".to_string();
    }
    match shown.strip_prefix(&format!("{home}/")) {
        Some(rest) => format!("~/{rest}"),
        None => shown,
    }
}

/// A path at most `max` columns wide, home-relative, cut in the middle.
///
/// The cut lands on a path separator when it can — `~/repos/…/fix-media-fetch`
/// reads as a path, where a cut through the middle of a component reads as
/// damage. Falls back to a plain middle truncation when even one component will
/// not fit.
pub(crate) fn shorten_path(path: &Path, max: usize) -> String {
    let shown = home_relative(path);
    if display_width(&shown) <= max {
        return shown;
    }
    let parts: Vec<&str> = shown.split('/').collect();
    if parts.len() > 2 {
        let prefix = format!("{}/{ELLIPSIS}/", parts[0]);
        let prefix_width = display_width(&prefix);
        if prefix_width < max {
            let budget = max - prefix_width;
            let mut kept: Vec<&str> = Vec::new();
            let mut used = 0;
            for part in parts.iter().skip(1).rev() {
                let cost = display_width(part) + usize::from(!kept.is_empty());
                if used + cost > budget {
                    break;
                }
                used += cost;
                kept.push(part);
            }
            if !kept.is_empty() {
                kept.reverse();
                return format!("{prefix}{}", kept.join("/"));
            }
        }
    }
    truncate_middle(&shown, max)
}

/// A ref name inside a sentence. Truncated at the end: the head of
/// `origin/feature/…` is the half that identifies it.
pub(crate) fn ref_name(name: &str, max: usize) -> String {
    truncate_right(name.trim(), max)
}

/// Width of `text` in terminal display columns. Hand-rolled because the crate
/// takes no width dependency: control characters count zero, combining marks
/// count zero, and the common East Asian wide blocks count two.
pub fn display_width(text: &str) -> usize {
    text.chars().map(char_columns).sum()
}

fn char_columns(ch: char) -> usize {
    if ch.is_control() {
        return 0;
    }
    let code = ch as u32;
    let zero_width = matches!(code,
        0x0300..=0x036f      // combining diacriticals
        | 0x1ab0..=0x1aff    // combining diacriticals extended
        | 0x20d0..=0x20ff    // combining marks for symbols
        | 0x200b..=0x200f    // zero width space .. RLM
        | 0xfe00..=0xfe0f    // variation selectors
        | 0xfe20..=0xfe2f    // combining half marks
        | 0xfeff);
    if zero_width {
        return 0;
    }
    let wide = matches!(code,
        0x1100..=0x115f
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe10..=0xfe19
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f900..=0x1f9ff
        | 0x20000..=0x2fffd
        | 0x30000..=0x3fffd);
    if wide {
        2
    } else {
        1
    }
}

/// Trims `text` to `max` display columns from the right, marking the cut with
/// `…`. For branch names, labels and subjects, whose head identifies them.
pub fn truncate_right(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    match max {
        0 => String::new(),
        1 => ELLIPSIS.to_string(),
        _ => {
            let mut out = take_front(text, max - 1);
            out.push(ELLIPSIS);
            out
        }
    }
}

/// Trims `text` to `max` display columns, dropping characters from the middle.
/// For paths, where both ends carry meaning.
pub fn truncate_middle(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    match max {
        0 => String::new(),
        1 | 2 => ELLIPSIS.to_string(),
        _ => {
            let budget = max - 1;
            // The tail of a path is the informative half, so it gets more of
            // the budget than the head.
            let tail_budget = budget * 2 / 3;
            let head = take_front(text, budget - tail_budget);
            let tail = take_back(text, tail_budget);
            format!("{head}{ELLIPSIS}{tail}")
        }
    }
}

fn take_front(text: &str, budget: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let columns = char_columns(ch);
        if used + columns > budget {
            break;
        }
        used += columns;
        out.push(ch);
    }
    out
}

fn take_back(text: &str, budget: usize) -> String {
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0;
    for ch in text.chars().rev() {
        let columns = char_columns(ch);
        if used + columns > budget {
            break;
        }
        used += columns;
        kept.push(ch);
    }
    kept.reverse();
    kept.into_iter().collect()
}

/// Widest branch column a repository needs, capped so that a single 90-column
/// branch name cannot squeeze every path in the block off the screen.
pub(crate) fn branch_column(repo: &RepoDigest) -> usize {
    repo.checkouts
        .iter()
        .map(|checkout| display_width(&head_label(&checkout.report.head)))
        .max()
        .unwrap_or(0)
        .clamp(4, BRANCH_COLUMNS)
}

/// One word for a checkout with nothing to say. Used by both human formats so
/// the quiet vocabulary is identical.
///
/// [`Activity::Unpushed`] deliberately has no word here: it has something to
/// say, and `unpushed_sentence` says it. Giving it a quiet word would put it
/// back under the label this state exists to escape.
pub(crate) fn quiet_word(activity: Activity) -> Option<&'static str> {
    match activity {
        Activity::Quiet => Some("quiet"),
        _ => None,
    }
}
