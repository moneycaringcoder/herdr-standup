//! Slack mrkdwn.
//!
//! Not Markdown, and the difference is not cosmetic. Pasting the Markdown
//! digest into Slack degrades in four specific ways, each verified against
//! Slack's own formatting documentation:
//!
//! | Markdown | in Slack |
//! |---|---|
//! | `**bold**` | renders **literally**, asterisks and all — mrkdwn's bold is a *single* asterisk |
//! | `- item` | renders literally when posted through the API; mrkdwn has no list syntax at all |
//! | `[text](url)` | renders literally; mrkdwn links are `<url\|text>` |
//! | `&`, `<`, `>` | interpreted, so they have to arrive as `&amp;`, `&lt;`, `&gt;` |
//!
//! The third of those never comes up here — this output contains no links — and
//! the other three all do. Bullets are therefore literal `\u{2022}` characters
//! with their own indentation, because a bulleted list is the shape this report
//! wants and mrkdwn will not supply one.
//!
//! Escaping is the opposite problem from the Markdown renderer's. There, every
//! piece of punctuation Markdown might read had to be escaped, because a branch
//! called `feat/re[factor]` must survive. Here **only** those three characters
//! are special, so escaping anything else would put backslashes in front of
//! ordinary punctuation and make the paste worse rather than better.

use super::*;
use crate::model::Severity;

/// Slack's bullet and its nested form. Literal characters, since mrkdwn has no
/// list syntax to ask for.
const BULLET: char = '\u{2022}'; // •
const SUB_BULLET: char = '\u{25e6}'; // ◦

pub fn slack(digest: &Digest, config: &Config, grouping: Option<&Grouping>) -> String {
    let mut out = String::new();
    let window = &digest.window;

    let mut head = format!("*standup* \u{2014} since {}", esc(&window.since.full()));
    if let Some(until) = &window.until {
        head.push_str(&format!(" until {}", esc(&until.full())));
    }
    head.push_str(&format!(" (generated {})", esc(&generated_display(digest))));
    out.push_str(&head);
    out.push('\n');
    if let Some(note) = window_note(window) {
        out.push_str(&esc(&note));
        out.push('\n');
    }
    if let Some(grouping) = grouping {
        out.push_str(&esc(&grouping.caveat()));
        out.push('\n');
    }
    out.push('\n');

    let sections = sections(digest, grouping);
    let total: usize = sections.iter().map(|section| section.repos.len()).sum();
    let busy_anywhere = sections.iter().any(|section| {
        section
            .repos
            .iter()
            .any(|r| r.activity() != Activity::Quiet)
    });

    if total == 0 {
        out.push_str("No repositories were found in this session.\n");
    } else if !busy_anywhere {
        out.push_str(&format!(
            "Nothing landed in this window, across {}.\n",
            quantity(total, "repository", "repositories")
        ));
    } else {
        out.push_str(&format!(
            "{} across {}.\n\n",
            esc(&stats(digest.total_commits(), digest.total_churn())),
            quantity(digest.repos.len(), "repository", "repositories")
        ));
        let with_date = dates_needed(digest);
        let list_commits = lists_commits(&digest.window);
        for section in &sections {
            if let Some(heading) = &section.heading {
                out.push_str(&format!(
                    "\n*{}*{}\n",
                    esc(heading),
                    section
                        .stats
                        .as_deref()
                        .map(|stats| format!(" \u{2014} {}", esc(stats)))
                        .unwrap_or_default()
                ));
            }
            for repo in section.busy() {
                repo_lines(&mut out, repo, config, with_date, list_commits);
            }
            // Under its own heading when there is one: with a grouping, one
            // repository can be busy in one group and quiet in another, and a
            // single trailing list would contradict a busy block above it.
            if section.heading.is_some() {
                quiet_line(&mut out, section.quiet().map(|repo| esc(&repo.name)));
            }
        }
    }

    if sections.iter().all(|section| section.heading.is_none()) {
        quiet_line(
            &mut out,
            sections
                .iter()
                .flat_map(|section| section.quiet())
                .map(|repo| esc(&repo.name)),
        );
    }

    notes(&mut out, digest);
    out
}

/// The one place a list of quiet repository names is formatted, so a section's
/// list and the whole digest's cannot drift apart.
fn quiet_line(out: &mut String, names: impl Iterator<Item = String>) {
    let names: Vec<String> = names.collect();
    if names.is_empty() {
        return;
    }
    out.push_str(&format!("\nQuiet: {}.\n", names.join(", ")));
}

/// The comparison, in mrkdwn.
pub fn comparison(comparison: &Comparison) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "*standup* \u{2014} what changed between *{}* and *{}*\n\n",
        esc(&comparison.before.full()),
        esc(&comparison.after.full())
    ));

    if comparison.repos.is_empty() {
        out.push_str("Neither digest found a repository, so there is nothing to compare.\n");
        return out;
    }
    if comparison.is_quiet() {
        out.push_str(&format!(
            "Nothing moved, across {}.\n",
            quantity(comparison.repos.len(), "repository", "repositories")
        ));
        return out;
    }

    out.push_str(&format!(
        "{} across {}.\n\n",
        quantity(comparison.total_commits(), "new commit", "new commits"),
        quantity(comparison.repos.len(), "repository", "repositories")
    ));

    let mut unchanged: Vec<String> = Vec::new();
    for repo in &comparison.repos {
        let moved: Vec<&(String, Movement)> = repo
            .checkouts
            .iter()
            .filter(|(_, movement)| movement.activity() != Activity::Quiet)
            .collect();
        if moved.is_empty() {
            unchanged.push(esc(&repo.name));
            continue;
        }
        out.push_str(&format!("{BULLET} *{}*\n", esc(&repo.name)));
        for (path, movement) in moved {
            let shown = shorten_path(std::path::Path::new(path), SLACK_PATH_COLUMNS);
            let sentence = if movement.loud() {
                format!("*{}*", esc(&movement.sentence()))
            } else {
                esc(&movement.sentence())
            };
            out.push_str(&format!(
                "    {SUB_BULLET} `{}` \u{2014} {sentence}\n",
                code(&shown)
            ));
        }
    }

    if !unchanged.is_empty() {
        out.push_str(&format!("\nUnchanged: {}.\n", unchanged.join(", ")));
    }
    out
}

/// A path long enough to be useful in a channel and short enough not to wrap on
/// a phone, which is where a standup message is often read.
const SLACK_PATH_COLUMNS: usize = 60;

fn repo_lines(
    out: &mut String,
    repo: &RepoDigest,
    config: &Config,
    with_date: bool,
    list_commits: bool,
) {
    out.push_str(&format!(
        "{BULLET} *{}* \u{2014} {}",
        esc(&repo.name),
        esc(&stats(repo.commits, repo.churn))
    ));
    if !list_commits && repo.active_days > 0 {
        out.push_str(&format!(
            " \u{2014} over {}",
            quantity(repo.active_days, "active day", "active days")
        ));
    }
    out.push('\n');

    for checkout in sorted_checkouts(repo) {
        let report = &checkout.report;
        let mut headline = format!(
            "    {SUB_BULLET} `{}` \u{2014} `{}`",
            code(&truncate_right(
                &head_label(&report.head),
                SLACK_PATH_COLUMNS
            )),
            code(&shorten_path(&report.path, SLACK_PATH_COLUMNS))
        );
        let head_note = head_note(&report.head, REF_UNLIMITED);
        match quiet_word(report.activity()) {
            Some(word) => {
                if head_note.is_none() {
                    headline.push_str(&format!(" \u{2014} {word}"));
                }
            }
            None => {
                let clauses: Vec<String> = checkout_clauses(report, REF_UNLIMITED)
                    .iter()
                    .map(|clause| esc(clause))
                    .collect();
                headline.push_str(&format!(" \u{2014} {}", clauses.join(" \u{2014} ")));
            }
        }
        out.push_str(&headline);
        out.push('\n');

        if let Some((note, loud)) = &head_note {
            sub_line(out, &marked(*loud, note));
        }
        for problem in &report.problems {
            sub_line(out, &marked(true, problem));
        }
        if let Some((label, who)) = attribution(checkout) {
            sub_line(out, &format!("{label}: {}", esc(&who)));
        }
        if let Some(unpushed) = unpushed_sentence(&report.unpushed) {
            sub_line(out, &esc(&unpushed));
        }
        if let Some(dirty) = dirty_sentence(&report.dirty) {
            sub_line(out, &esc(&dirty));
        }
        if list_commits {
            commit_lines(out, report, config, with_date);
        }
    }
}

fn commit_lines(out: &mut String, report: &CheckoutReport, config: &Config, with_date: bool) {
    let commits = sorted_commits(report);
    let (shown, held_back) = split_commits(&commits, config.max_commits);
    for commit in &shown {
        sub_line(
            out,
            &format!(
                "`{}` `{}` {}",
                code(commit_time(&commit.committed, with_date)),
                code(commit.short_oid()),
                esc(commit.subject.trim())
            ),
        );
    }
    if held_back > 0 {
        sub_line(
            out,
            &format!(
                "\u{2026} {} not listed",
                quantity(held_back, "more commit", "more commits")
            ),
        );
    }
}

/// A third-level line. Indented rather than bulleted: mrkdwn has no nesting to
/// express, and a third bullet character reads as noise.
fn sub_line(out: &mut String, text: &str) {
    out.push_str("        ");
    out.push_str(text);
    out.push('\n');
}

fn notes(out: &mut String, digest: &Digest) {
    if digest.notes.is_empty() {
        return;
    }
    out.push('\n');
    for note in &digest.notes {
        let loud = note.severity != Severity::Info;
        out.push_str(&format!("{BULLET} {}\n", marked(loud, &note.message)));
    }
}

/// `*problem:* …` — bold with a single asterisk, which is the whole point of
/// this module.
fn marked(loud: bool, message: &str) -> String {
    if loud {
        format!("*problem:* {}", esc(message))
    } else {
        esc(message)
    }
}

/// `7 commits, 23 files, +812 −140`, the same sentence the Markdown renderer
/// uses. Same numbers, different punctuation: a format is a rendering, never a
/// different answer.
fn stats(commits: usize, churn: Churn) -> String {
    let mut out = commit_count(commits);
    if !churn.is_zero() {
        out.push_str(&format!(
            ", {}, {}",
            files_count(churn),
            delta(churn.insertions, churn.deletions)
        ));
    }
    out
}

/// Flattens a value for a code span. Backticks are dropped rather than escaped:
/// mrkdwn has no escape for one inside a span, and a stray backtick ends the
/// span and turns the rest of the line into prose.
fn code(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '`' => '\'',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect()
}

/// The three characters Slack interprets, and **only** those three.
///
/// The Markdown renderer escapes every piece of punctuation its parser might
/// read. Doing that here would be worse than doing nothing: mrkdwn treats a
/// backslash as a literal character, so escaping a `.` or a `[` puts a visible
/// backslash in a channel. Slack's own documentation is explicit that these are
/// the only three.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            ch => out.push(ch),
        }
    }
    out
}
