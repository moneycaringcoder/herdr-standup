//! The Markdown digest — the one people judge.
//!
//! It is pasted into a standup channel or a journal with no editing, so two
//! things drive every decision here.
//!
//! **It must survive GitHub.** Real branch names contain `_`, `*`, `[`, `]`,
//! `|` and backticks, and a subject is arbitrary text that may itself look like
//! a heading. Every value out of the model therefore goes through exactly one
//! of two doors: [`code`], which wraps it in a backtick fence long enough to
//! contain it, or [`esc`], which backslash-escapes the characters Markdown
//! would otherwise read as syntax. Nothing is interpolated raw.
//!
//! **It must survive Slack, which renders no Markdown tables at all.** So there
//! are no tables: a nested bullet list carries the same structure, degrades to
//! readable plain text, and cannot be broken by a `|` in a branch name. For the
//! same reason there is no HTML.

use super::*;
use crate::model::Severity;

/// A path in a bullet is not competing with anything for space, but a
/// 200-character one is still noise in a channel.
const MD_PATH_COLUMNS: usize = 72;
/// Branch names truncate at the end: the head identifies them.
const MD_BRANCH_COLUMNS: usize = 72;
/// Long enough for a real subject, short enough that one commit cannot take
/// over the paste.
const MD_SUBJECT_COLUMNS: usize = 120;

pub fn markdown(digest: &Digest, config: &Config, grouping: Option<&Grouping>) -> String {
    let mut out = String::new();
    header(&mut out, digest);
    if let Some(grouping) = grouping {
        paragraph(&mut out, &esc(&grouping.caveat()));
    }

    let sections = sections(digest, grouping);
    let total: usize = sections.iter().map(|section| section.repos.len()).sum();
    let busy_anywhere = sections.iter().any(|section| {
        section
            .repos
            .iter()
            .any(|r| r.activity() != Activity::Quiet)
    });

    if total == 0 {
        paragraph(&mut out, "No repositories were found in this session.");
    } else if !busy_anywhere {
        paragraph(
            &mut out,
            &format!(
                "Nothing landed in this window, across {}.",
                quantity(total, "repository", "repositories")
            ),
        );
    } else {
        paragraph(
            &mut out,
            &format!(
                "{} across {}.",
                stats(digest.total_commits(), digest.total_churn()),
                quantity(digest.repos.len(), "repository", "repositories")
            ),
        );
        let with_date = dates_needed(digest);
        let list_commits = lists_commits(&digest.window);
        for section in &sections {
            if let Some(heading) = &section.heading {
                out.push_str(&format!(
                    "\n**{}**{}\n\n",
                    esc(heading),
                    section
                        .stats
                        .as_deref()
                        .map(|stats| format!(" \u{2014} {}", esc(stats)))
                        .unwrap_or_default()
                ));
            }
            for repo in section.busy() {
                repo_bullets(&mut out, repo, config, with_date, list_commits);
            }
            // Under its own heading when there is one: with a grouping, one
            // repository can be busy in one group and quiet in another, and a
            // single trailing list would contradict a busy block above it.
            if section.heading.is_some() {
                quiet_paragraph(&mut out, section.quiet().map(|repo| esc(&repo.name)));
            }
        }
        out.push('\n');
    }

    if sections.iter().all(|section| section.heading.is_none()) {
        quiet_paragraph(
            &mut out,
            sections
                .iter()
                .flat_map(|section| section.quiet())
                .map(|repo| esc(&repo.name)),
        );
    }

    notes(&mut out, digest);

    // A paste should not begin with somebody's cursor three lines below the
    // last word: exactly one trailing newline, whatever the last block was.
    while out.ends_with('\n') {
        out.pop();
    }
    out.push('\n');
    out
}

/// The one place a list of quiet repository names is formatted, so a section's
/// list and the whole digest's cannot drift apart.
fn quiet_paragraph(out: &mut String, names: impl Iterator<Item = String>) {
    let names: Vec<String> = names.collect();
    if names.is_empty() {
        return;
    }
    paragraph(out, &format!("Quiet: {}.", names.join(", ")));
}

/// The same comparison as Markdown, for pasting where the digest goes.
pub fn comparison(comparison: &Comparison) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "**standup** \u{2014} what changed between **{}** and **{}**\n\n",
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
        out.push_str(&format!("- **{}**\n", esc(&repo.name)));
        for (path, movement) in moved {
            let shown = shorten_path(std::path::Path::new(path), MD_PATH_COLUMNS);
            let sentence = if movement.loud() {
                format!("**{}**", esc(&movement.sentence()))
            } else {
                esc(&movement.sentence())
            };
            out.push_str(&format!("  - {} \u{2014} {sentence}\n", code(&shown)));
        }
    }

    if !unchanged.is_empty() {
        out.push_str(&format!("\nUnchanged: {}.\n", unchanged.join(", ")));
    }
    out
}

fn header(out: &mut String, digest: &Digest) {
    let window = &digest.window;
    let mut head = format!("**standup** \u{2014} since {}", esc(&window.since.full()));
    if let Some(until) = &window.until {
        head.push_str(&format!(" until {}", esc(&until.full())));
    }
    head.push_str(&format!(" (generated {})", esc(&generated_display(digest))));
    paragraph(out, &head);

    if let Some(note) = window_note(window) {
        paragraph(out, &esc(&note));
    }
}

fn repo_bullets(
    out: &mut String,
    repo: &RepoDigest,
    config: &Config,
    with_date: bool,
    list_commits: bool,
) {
    out.push_str(&format!(
        "- **{}** \u{2014} {}",
        esc(&repo.name),
        stats(repo.commits, repo.churn)
    ));
    // Only worth saying over a window longer than a day, where "34 commits" is a
    // very different month depending on whether it was nine days or one.
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
            "  - {} \u{2014} {}",
            code(&truncate_right(
                &head_label(&report.head),
                MD_BRANCH_COLUMNS
            )),
            code(&shorten_path(&report.path, MD_PATH_COLUMNS))
        );
        let head_note = head_note(&report.head, REF_UNLIMITED);
        match quiet_word(report.activity()) {
            // The head note, on the line below, already says why this checkout
            // is empty; "quiet" under it would be a weaker second version.
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
            item(out, 4, &marked(*loud, note));
        }
        // A problem means the numbers above it are incomplete. It outranks
        // brevity every time.
        for problem in &report.problems {
            item(out, 4, &marked(true, problem));
        }
        if let Some((label, who)) = attribution(checkout) {
            item(out, 4, &format!("{label}: {}", esc(&who)));
        }
        if let Some(unpushed) = unpushed_sentence(&report.unpushed) {
            item(out, 4, &esc(&unpushed));
        }
        if let Some(dirty) = dirty_sentence(&report.dirty) {
            item(out, 4, &esc(&dirty));
        }

        // A rollup aggregates rather than lists. A month of commits printed one
        // per line is a `git log` with extra steps.
        if list_commits {
            commit_items(out, report, config, with_date);
        }
    }
}

fn commit_items(out: &mut String, report: &CheckoutReport, config: &Config, with_date: bool) {
    let commits = sorted_commits(report);
    let (shown, held_back) = split_commits(&commits, config.max_commits);

    for commit in &shown {
        // The subject is prose, not an identifier, so it is escaped rather than
        // fenced: a backtick inside a code span needs a longer fence, and a
        // subject is where backticks actually turn up.
        item(
            out,
            4,
            &format!(
                "{} {} \u{2014} {}",
                code(commit_time(&commit.committed, with_date)),
                code(commit.short_oid()),
                esc(&truncate_right(commit.subject.trim(), MD_SUBJECT_COLUMNS))
            ),
        );
    }

    if held_back > 0 {
        let counted = if shown.is_empty() {
            quantity(held_back, "commit", "commits")
        } else {
            quantity(held_back, "more commit", "more commits")
        };
        item(out, 4, &format!("{ELLIPSIS} {counted} not listed"));
    }
}

fn notes(out: &mut String, digest: &Digest) {
    if digest.notes.is_empty() {
        return;
    }
    paragraph(out, "Notes:");
    for note in &digest.notes {
        let loud = note.severity == Severity::Warning;
        item(out, 0, &marked(loud, &note.message));
    }
    out.push('\n');
}

/// `7 commits, 23 files, +812 −140`. Commas rather than the terminal report's
/// middle dots: this is a sentence in a channel, not a column in a pane.
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

fn marked(loud: bool, message: &str) -> String {
    if loud {
        format!("**problem:** {}", esc(message))
    } else {
        esc(message)
    }
}

fn paragraph(out: &mut String, text: &str) {
    out.push_str(text);
    out.push_str("\n\n");
}

/// One bullet, `indent` columns in. Four columns per level, which is what a
/// nested list needs to survive both GitHub and a plain-text paste.
fn item(out: &mut String, indent: usize, text: &str) {
    out.push_str(&" ".repeat(indent));
    out.push_str("- ");
    out.push_str(text);
    out.push('\n');
}

// ---------------------------------------------------------------------------
// Escaping
// ---------------------------------------------------------------------------

/// An identifier — branch, path, ref, object id — as an inline code span.
///
/// The fence is one backtick longer than the longest run inside the content, so
/// a branch name containing backticks stays inside its span instead of ending
/// it. Content that begins or ends with a backtick or a space is padded, which
/// CommonMark strips again on render.
fn code(text: &str) -> String {
    let flattened: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if flattened.trim().is_empty() {
        return "`(empty)`".to_string();
    }

    let mut longest = 0;
    let mut run = 0;
    for ch in flattened.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }

    let fence = "`".repeat(longest + 1);
    let pad = if flattened.starts_with('`')
        || flattened.ends_with('`')
        || flattened.starts_with(' ')
        || flattened.ends_with(' ')
    {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{flattened}{pad}{fence}")
}

/// Prose — a subject, a problem, a note — with everything Markdown would read
/// as syntax backslash-escaped. CommonMark honours a backslash before any ASCII
/// punctuation, so the escaped form renders as the original text.
fn esc(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if matches!(
            ch,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '~' | '&'
        ) {
            out.push('\\');
        }
        out.push(if ch.is_control() { ' ' } else { ch });
    }

    // A heading and a list item are only those things at the start of a line,
    // and only when the marker is followed by a space — `+30 −12` is neither. A
    // subject that really is `# oops` will one day be pasted somewhere it *is*
    // at the start of a line, and an escaped leader costs one backslash where
    // an unescaped one costs a heading in somebody's standup.
    let mut leaders = out.chars();
    let marker = leaders.next();
    let after = leaders.next();
    if matches!(marker, Some('#' | '-' | '+')) && matches!(after, None | Some(' ')) {
        out.insert(0, '\\');
        return out;
    }
    let bytes = out.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_digit() && matches!(bytes[1], b'.' | b')') {
        out.insert(1, '\\');
    }
    out
}
