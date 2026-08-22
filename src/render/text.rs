//! The terminal report.
//!
//! A person scanning this wants, in order: what window, what came out of it,
//! and what needs attention. The layout follows that, and nothing is drawn in a
//! box — a box costs two columns on every line and buys nothing a blank line
//! does not.
//!
//! Every line leaves this module through [`line`] or [`wrapped`], which is the
//! only place the 80-column budget is enforced. Prose wraps; identifiers
//! truncate. Truncating an explanation removes the explanation, but truncating
//! a 200-character path removes only the part nobody reads.

use super::*;
use crate::model::Severity;

pub fn text(digest: &Digest, config: &Config) -> String {
    let mut out = String::new();
    header(&mut out, digest);

    let repos = sorted_repos(digest);
    let busy: Vec<&RepoDigest> = repos
        .iter()
        .copied()
        .filter(|repo| repo.activity() != Activity::Quiet)
        .collect();
    let quiet: Vec<&RepoDigest> = repos
        .iter()
        .copied()
        .filter(|repo| repo.activity() == Activity::Quiet)
        .collect();

    blank(&mut out);
    if repos.is_empty() {
        // An empty screen and a quiet day must never look the same.
        wrapped(
            &mut out,
            "  ",
            "  ",
            "No repositories were found in this session.",
        );
    } else if busy.is_empty() {
        wrapped(
            &mut out,
            "  ",
            "  ",
            &format!(
                "Nothing landed in this window, across {}.",
                quantity(repos.len(), "repository", "repositories")
            ),
        );
    } else {
        line(
            &mut out,
            &format!(
                "  {}  across {}",
                repo_stats(digest.total_commits(), digest.total_churn()),
                quantity(repos.len(), "repository", "repositories")
            ),
        );
        let with_date = dates_needed(digest);
        let list_commits = lists_commits(&digest.window);
        for repo in &busy {
            blank(&mut out);
            repo_block(&mut out, repo, config, with_date, list_commits);
        }
    }

    if !quiet.is_empty() {
        blank(&mut out);
        let names: Vec<&str> = quiet.iter().map(|repo| repo.name.as_str()).collect();
        wrapped(&mut out, "  quiet: ", "         ", &names.join(", "));
    }

    notes(&mut out, digest);
    out
}

/// The terminal comparison.
///
/// Laid out to be read top to bottom as a list of things that moved, not as a
/// digest with numbers. There is no churn here and no commit list on purpose:
/// "how much" is what a digest answers, and repeating it would turn this back
/// into the longer digest it exists not to be.
pub fn comparison(comparison: &Comparison) -> String {
    let mut out = String::new();
    // Two full stamps and a sentence do not fit in eighty columns, and a
    // truncated header loses one of the two instants being compared — which is
    // the one thing this report cannot be vague about.
    line(
        &mut out,
        "standup \u{2014} what changed between two digests",
    );
    line(&mut out, &format!("  before  {}", comparison.before.full()));
    line(&mut out, &format!("  after   {}", comparison.after.full()));
    blank(&mut out);

    if comparison.repos.is_empty() {
        wrapped(
            &mut out,
            "  ",
            "  ",
            "Neither digest found a repository, so there is nothing to compare.",
        );
        return out;
    }
    if comparison.is_quiet() {
        wrapped(
            &mut out,
            "  ",
            "  ",
            &format!(
                "Nothing moved, across {}.",
                quantity(comparison.repos.len(), "repository", "repositories")
            ),
        );
        return out;
    }

    line(
        &mut out,
        &format!(
            "  {} across {}",
            quantity(comparison.total_commits(), "new commit", "new commits"),
            quantity(comparison.repos.len(), "repository", "repositories")
        ),
    );

    let unchanged: Vec<&str> = comparison
        .repos
        .iter()
        .filter(|repo| {
            repo.checkouts
                .iter()
                .all(|(_, movement)| movement.activity() == Activity::Quiet)
        })
        .map(|repo| repo.name.as_str())
        .collect();

    for repo in &comparison.repos {
        if unchanged.contains(&repo.name.as_str()) {
            continue;
        }
        blank(&mut out);
        line(&mut out, &format!("  {}", repo.name));
        for (path, movement) in &repo.checkouts {
            if movement.activity() == Activity::Quiet {
                continue;
            }
            let shown = shorten_path(std::path::Path::new(path), WIDTH.saturating_sub(10));
            wrapped(&mut out, "    ", "      ", &shown);
            marked(&mut out, movement.loud(), &movement.sentence());
        }
    }

    if !unchanged.is_empty() {
        blank(&mut out);
        wrapped(
            &mut out,
            "  unchanged: ",
            "             ",
            &unchanged.join(", "),
        );
    }
    out
}

/// The window, in the unambiguous form, and why it is this window.
fn header(out: &mut String, digest: &Digest) {
    let window = &digest.window;
    let mut first = format!("standup \u{2014} since {}", window.since.full());
    match &window.until {
        // Two full stamps and a "generated" would not fit on one line, and a
        // wrapped header reads as a mistake.
        Some(until) => {
            first.push_str(&format!(" until {}", until.full()));
            line(out, &first);
            line(out, &format!("  generated {}", digest.generated_at.full()));
        }
        None => {
            first.push_str(&format!(" (generated {})", generated_display(digest)));
            line(out, &first);
        }
    }
    if let Some(note) = window_note(window) {
        wrapped(out, "  ", "  ", &note);
    }
}

/// One repository: a header line with the numbers right-aligned, then its
/// checkouts.
fn repo_block(
    out: &mut String,
    repo: &RepoDigest,
    config: &Config,
    with_date: bool,
    list_commits: bool,
) {
    let stats = repo_stats(repo.commits, repo.churn);
    let stats_width = display_width(&stats);
    let name_budget = WIDTH.saturating_sub(stats_width + 4).max(8);
    let name = truncate_right(&repo.name, name_budget);
    let gap = WIDTH
        .saturating_sub(2 + display_width(&name) + stats_width)
        .max(2);
    line(out, &format!("  {name}{}{stats}", " ".repeat(gap)));

    // Only worth saying over a window longer than a day, where "34 commits" is a
    // very different month depending on whether it was nine days or one.
    if !list_commits && repo.active_days > 0 {
        line(
            out,
            &format!(
                "    over {}",
                quantity(repo.active_days, "active day", "active days")
            ),
        );
    }

    let branch_width = branch_column(repo);
    for checkout in sorted_checkouts(repo) {
        checkout_block(out, checkout, branch_width, config, with_date, list_commits);
    }
}

/// One checkout: where it is, who was there, what it did, and anything that
/// makes those numbers incomplete.
fn checkout_block(
    out: &mut String,
    checkout: &CheckoutDigest,
    branch_width: usize,
    config: &Config,
    with_date: bool,
    list_commits: bool,
) {
    let report = &checkout.report;

    let branch = truncate_right(&head_label(&report.head), branch_width);
    let path_budget = WIDTH.saturating_sub(6 + branch_width).max(12);
    let path = shorten_path(&report.path, path_budget);
    let gap = branch_width - display_width(&branch) + 2;
    line(out, &format!("    {branch}{}{path}", " ".repeat(gap)));

    // A branch that vanished underneath a live checkout, or one with no commits
    // yet: the branch column cannot carry either, and both change what the rest
    // of the block means.
    let head_note = head_note(&report.head, REF_COLUMNS);
    if let Some((note, loud)) = &head_note {
        marked(out, *loud, note);
    }
    // Never dropped for brevity: a problem means the numbers beside it are
    // incomplete, which is the one thing a digest must not hide.
    for problem in &report.problems {
        marked(out, true, problem);
    }
    if let Some((label, who)) = attribution(checkout) {
        wrapped(out, "      ", "        ", &format!("{label}: {who}"));
    }

    match quiet_word(report.activity()) {
        Some(word) => {
            // The head note has already said why this checkout is empty; a
            // "quiet" under it would be a second, weaker version of that.
            if head_note.is_none() {
                line(out, &format!("      {word}"));
            }
        }
        None => wrapped(
            out,
            "      ",
            "        ",
            &checkout_clauses(report, REF_COLUMNS).join(", "),
        ),
    }
    // Lifecycle order, which is also fragility order: what was committed, what
    // is committed but only here, what is not committed at all.
    if let Some(unpushed) = unpushed_sentence(&report.unpushed) {
        wrapped(out, "      ", "        ", &unpushed);
    }
    if let Some(dirty) = dirty_sentence(&report.dirty) {
        wrapped(out, "      ", "        ", &dirty);
    }

    // A rollup aggregates rather than lists. A month of commits printed one per
    // line is a `git log` with extra steps, and not the thing anybody forwards.
    if list_commits {
        commit_lines(out, report, config, with_date);
    }
}

fn commit_lines(out: &mut String, report: &CheckoutReport, config: &Config, with_date: bool) {
    let commits = sorted_commits(report);
    let (shown, held_back) = split_commits(&commits, config.max_commits);

    let stamp_width = if with_date { 16 } else { 5 };
    let oid_width = OID_COLUMNS;
    let subject_budget = WIDTH
        .saturating_sub(8 + stamp_width + 2 + oid_width + 2)
        .max(12);

    for commit in &shown {
        let when = commit_time(&commit.committed, with_date);
        let subject = truncate_right(commit.subject.trim(), subject_budget);
        line(
            out,
            &format!(
                "        {when:>stamp_width$}  {:<oid_width$}  {subject}",
                commit.short_oid()
            ),
        );
    }

    if held_back > 0 {
        let counted = if shown.is_empty() {
            quantity(held_back, "commit", "commits")
        } else {
            quantity(held_back, "more commit", "more commits")
        };
        line(out, &format!("        {ELLIPSIS} {counted} not listed"));
    }
}

/// Notes belong to the session rather than to any one checkout, so they sit at
/// the end, where "what needs attention" is looked for.
fn notes(out: &mut String, digest: &Digest) {
    if digest.notes.is_empty() {
        return;
    }
    blank(out);
    line(out, "  notes");
    for note in &digest.notes {
        match note.severity {
            Severity::Warning => wrapped(out, "    ! ", "      ", &note.message),
            Severity::Info => wrapped(out, "    - ", "      ", &note.message),
        }
    }
}

// ---------------------------------------------------------------------------
// Line plumbing
// ---------------------------------------------------------------------------

/// A warning-marked line, indented under its checkout. `!` rather than colour:
/// this output is as likely to be piped as it is to be read on a terminal.
fn marked(out: &mut String, loud: bool, message: &str) {
    if loud {
        wrapped(out, "      ! ", "        ", message);
    } else {
        wrapped(out, "      ", "        ", message);
    }
}

fn blank(out: &mut String) {
    out.push('\n');
}

/// The single place the width budget is enforced.
fn line(out: &mut String, text: &str) {
    out.push_str(&truncate_right(text.trim_end(), WIDTH));
    out.push('\n');
}

/// Greedy word wrap. Prose wraps rather than truncates, because truncating an
/// explanation removes the explanation.
fn wrapped(out: &mut String, first: &str, rest: &str, text: &str) {
    let mut prefix = first;
    let mut current = String::new();

    for word in text.split_whitespace() {
        let budget = WIDTH.saturating_sub(display_width(prefix)).max(1);
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if display_width(&candidate) <= budget || current.is_empty() {
            current = candidate;
        } else {
            line(out, &format!("{prefix}{current}"));
            prefix = rest;
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        line(out, &format!("{prefix}{current}"));
    }
}
