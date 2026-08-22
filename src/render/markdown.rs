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

pub fn markdown(digest: &Digest, config: &Config) -> String {
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

    if repos.is_empty() {
        paragraph(&mut out, "No repositories were found in this session.");
    } else if busy.is_empty() {
        paragraph(
            &mut out,
            &format!(
                "Nothing landed in this window, across {}.",
                quantity(repos.len(), "repository", "repositories")
            ),
        );
    } else {
        paragraph(
            &mut out,
            &format!(
                "{} across {}.",
                stats(digest.total_commits(), digest.total_churn()),
                quantity(repos.len(), "repository", "repositories")
            ),
        );
        let with_date = dates_needed(digest);
        for repo in &busy {
            repo_bullets(&mut out, repo, config, with_date);
        }
        out.push('\n');
    }

    if !quiet.is_empty() {
        let names: Vec<String> = quiet.iter().map(|repo| esc(&repo.name)).collect();
        paragraph(&mut out, &format!("Quiet: {}.", names.join(", ")));
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

fn repo_bullets(out: &mut String, repo: &RepoDigest, config: &Config, with_date: bool) {
    out.push_str(&format!(
        "- **{}** \u{2014} {}\n",
        esc(&repo.name),
        stats(repo.commits, repo.churn)
    ));

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

        commit_items(out, report, config, with_date);
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
            quantity(churn.files, "file", "files"),
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
