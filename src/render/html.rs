//! HTML, for an email.
//!
//! Written for the least capable renderer it will meet rather than the most
//! capable, which for HTML means an email client:
//!
//! - **Every style is inline.** Gmail and Outlook strip `<style>` blocks and
//!   `<link>` outright, so a stylesheet is a stylesheet that will not arrive. An
//!   inline `style` attribute is the only thing that survives everywhere.
//! - **No external resources.** No images, no fonts, no scripts. Remote content
//!   is blocked by default in most clients and a blocked resource is worse than
//!   an absent one; a script is simply stripped.
//! - **Layout by table.** Not nostalgia: table layout is the one thing Outlook's
//!   renderer handles predictably, and the digest is a table of numbers anyway.
//! - **`&`, `<`, `>`, `"` escaped, always.** A branch called `feat/<x>` must not
//!   become a tag, and a path in an attribute must not end it.
//!
//! What this is not: a web page. There is no interactivity, no collapsing, and
//! nothing that needs a browser. It is the digest, laid out so it survives being
//! forwarded — which is the version of it somebody sends to someone else, and
//! the whole reason a monthly rollup wanted an HTML form.

use super::*;
use crate::model::Severity;

/// Colours, in hex because a named colour is not reliable across clients, and
/// chosen to stay legible against a white background — a dark-mode email is not
/// something the sender controls.
const INK: &str = "#1a1a1a";
const QUIET: &str = "#6a6a6a";
const LOUD: &str = "#a8331a";
const RULE: &str = "#e2e2e2";

/// One font stack, inline, on every text element. Email clients do not inherit
/// reliably, which is the single most surprising thing about writing HTML for
/// them.
const FONT: &str =
    "font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif";
const MONO: &str = "font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace";

pub fn html(digest: &Digest, config: &Config) -> String {
    let mut out = String::new();
    open(&mut out, "standup");

    let window = &digest.window;
    let mut head = format!("since {}", esc(&window.since.full()));
    if let Some(until) = &window.until {
        head.push_str(&format!(" until {}", esc(&until.full())));
    }
    out.push_str(&format!(
        "<h1 style=\"{FONT};font-size:18px;font-weight:600;color:{INK};margin:0 0 4px\">\
         standup</h1>\n\
         <p style=\"{FONT};font-size:13px;color:{QUIET};margin:0 0 2px\">{head} \
         (generated {})</p>\n",
        esc(&generated_display(digest))
    ));
    if let Some(note) = window_note(window) {
        out.push_str(&format!(
            "<p style=\"{FONT};font-size:13px;color:{QUIET};margin:0 0 16px\">{}</p>\n",
            esc(&note)
        ));
    } else {
        out.push_str("<div style=\"height:16px\"></div>\n");
    }

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
        out.push_str(&format!(
            "<p style=\"{FONT};font-size:15px;font-weight:600;color:{INK};margin:0 0 16px\">\
             {} across {}</p>\n",
            esc(&stats(digest.total_commits(), digest.total_churn())),
            quantity(repos.len(), "repository", "repositories")
        ));
        let with_date = dates_needed(digest);
        let list_commits = lists_commits(&digest.window);
        for repo in &busy {
            repo_block(&mut out, repo, config, with_date, list_commits);
        }
    }

    if !quiet.is_empty() {
        let names: Vec<String> = quiet.iter().map(|repo| esc(&repo.name)).collect();
        out.push_str(&format!(
            "<p style=\"{FONT};font-size:13px;color:{QUIET};margin:16px 0 0\">\
             Quiet: {}.</p>\n",
            names.join(", ")
        ));
    }

    for note in &digest.notes {
        let colour = if note.severity == Severity::Info {
            QUIET
        } else {
            LOUD
        };
        out.push_str(&format!(
            "<p style=\"{FONT};font-size:13px;color:{colour};margin:12px 0 0\">{}</p>\n",
            esc(&note.message)
        ));
    }

    close(&mut out);
    out
}

/// The comparison, as an email.
pub fn comparison(comparison: &Comparison) -> String {
    let mut out = String::new();
    open(&mut out, "standup — what changed");

    out.push_str(&format!(
        "<h1 style=\"{FONT};font-size:18px;font-weight:600;color:{INK};margin:0 0 4px\">\
         standup \u{2014} what changed</h1>\n\
         <p style=\"{FONT};font-size:13px;color:{QUIET};margin:0 0 16px\">\
         between {} and {}</p>\n",
        esc(&comparison.before.full()),
        esc(&comparison.after.full())
    ));

    if comparison.repos.is_empty() {
        paragraph(
            &mut out,
            "Neither digest found a repository, so there is nothing to compare.",
        );
        close(&mut out);
        return out;
    }
    if comparison.is_quiet() {
        paragraph(
            &mut out,
            &format!(
                "Nothing moved, across {}.",
                quantity(comparison.repos.len(), "repository", "repositories")
            ),
        );
        close(&mut out);
        return out;
    }

    out.push_str(&format!(
        "<p style=\"{FONT};font-size:15px;font-weight:600;color:{INK};margin:0 0 16px\">\
         {} across {}</p>\n",
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
        out.push_str(&format!(
            "<p style=\"{FONT};font-size:14px;font-weight:600;color:{INK};\
             margin:0 0 4px;padding-top:12px;border-top:1px solid {RULE}\">{}</p>\n",
            esc(&repo.name)
        ));
        out.push_str(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" \
                      border=\"0\" style=\"width:100%;margin:0 0 12px\">\n",
        );
        for (path, movement) in moved {
            let colour = if movement.loud() { LOUD } else { INK };
            out.push_str(&format!(
                "<tr><td style=\"{MONO};font-size:12px;color:{QUIET};padding:2px 12px 2px 0;\
                 vertical-align:top;white-space:nowrap\">{}</td>\
                 <td style=\"{FONT};font-size:13px;color:{colour};padding:2px 0;\
                 vertical-align:top\">{}</td></tr>\n",
                esc(&shorten_path(std::path::Path::new(path), HTML_PATH_COLUMNS)),
                esc(&movement.sentence())
            ));
        }
        out.push_str("</table>\n");
    }

    if !unchanged.is_empty() {
        out.push_str(&format!(
            "<p style=\"{FONT};font-size:13px;color:{QUIET};margin:16px 0 0\">\
             Unchanged: {}.</p>\n",
            unchanged.join(", ")
        ));
    }

    close(&mut out);
    out
}

/// Longer than the terminal's budget: an email is read in a window somebody can
/// widen, and a truncated path in an archive cannot be recovered.
const HTML_PATH_COLUMNS: usize = 96;

fn repo_block(
    out: &mut String,
    repo: &RepoDigest,
    config: &Config,
    with_date: bool,
    list_commits: bool,
) {
    let mut heading = format!(
        "{} \u{2014} {}",
        esc(&repo.name),
        esc(&stats(repo.commits, repo.churn))
    );
    if !list_commits && repo.active_days > 0 {
        heading.push_str(&format!(
            " \u{2014} over {}",
            quantity(repo.active_days, "active day", "active days")
        ));
    }
    out.push_str(&format!(
        "<p style=\"{FONT};font-size:14px;font-weight:600;color:{INK};\
         margin:0 0 6px;padding-top:12px;border-top:1px solid {RULE}\">{heading}</p>\n"
    ));

    for checkout in sorted_checkouts(repo) {
        let report = &checkout.report;
        out.push_str(&format!(
            "<p style=\"{MONO};font-size:12px;color:{INK};margin:0 0 2px\">{} \
             <span style=\"color:{QUIET}\">{}</span></p>\n",
            esc(&head_label(&report.head)),
            esc(&shorten_path(&report.path, HTML_PATH_COLUMNS))
        ));

        let head_note = head_note(&report.head, REF_UNLIMITED);
        match quiet_word(report.activity()) {
            Some(word) => {
                if head_note.is_none() {
                    detail(out, false, word);
                }
            }
            None => detail(
                out,
                false,
                &checkout_clauses(report, REF_UNLIMITED).join(", "),
            ),
        }
        if let Some((note, loud)) = &head_note {
            detail(out, *loud, note);
        }
        for problem in &report.problems {
            detail(out, true, problem);
        }
        if let Some((label, who)) = attribution(checkout) {
            detail(out, false, &format!("{label}: {who}"));
        }
        if let Some(unpushed) = unpushed_sentence(&report.unpushed) {
            detail(out, false, &unpushed);
        }
        if let Some(dirty) = dirty_sentence(&report.dirty) {
            detail(out, false, &dirty);
        }
        if list_commits {
            commit_rows(out, report, config, with_date);
        }
    }
}

fn commit_rows(out: &mut String, report: &CheckoutReport, config: &Config, with_date: bool) {
    let commits = sorted_commits(report);
    let (shown, held_back) = split_commits(&commits, config.max_commits);
    if shown.is_empty() {
        return;
    }
    out.push_str(
        "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
         style=\"width:100%;margin:4px 0 12px 16px\">\n",
    );
    for commit in &shown {
        out.push_str(&format!(
            "<tr><td style=\"{MONO};font-size:12px;color:{QUIET};padding:1px 10px 1px 0;\
             vertical-align:top;white-space:nowrap\">{} {}</td>\
             <td style=\"{FONT};font-size:13px;color:{INK};padding:1px 0;\
             vertical-align:top\">{}</td></tr>\n",
            esc(commit_time(&commit.committed, with_date)),
            esc(commit.short_oid()),
            esc(commit.subject.trim())
        ));
    }
    out.push_str("</table>\n");
    if held_back > 0 {
        out.push_str(&format!(
            "<p style=\"{FONT};font-size:12px;color:{QUIET};margin:0 0 12px 16px\">\
             \u{2026} {} not listed</p>\n",
            quantity(held_back, "more commit", "more commits")
        ));
    }
}

fn detail(out: &mut String, loud: bool, text: &str) {
    let colour = if loud { LOUD } else { INK };
    let prefix = if loud { "problem: " } else { "" };
    out.push_str(&format!(
        "<p style=\"{FONT};font-size:13px;color:{colour};margin:0 0 2px 16px\">{prefix}{}</p>\n",
        esc(text)
    ));
}

fn paragraph(out: &mut String, text: &str) {
    out.push_str(&format!(
        "<p style=\"{FONT};font-size:14px;color:{INK};margin:0\">{}</p>\n",
        esc(text)
    ));
}

/// A complete document rather than a fragment. An email body needs the charset
/// declared — a digest is full of `−`, `·` and `…`, and a client that guesses
/// latin-1 turns all three into mojibake.
fn open(out: &mut String, title: &str) {
    out.push_str(&format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
         <title>{}</title>\n</head>\n\
         <body style=\"margin:0;padding:20px;background:#ffffff\">\n\
         <div style=\"max-width:720px;margin:0 auto\">\n",
        esc(title)
    ));
}

fn close(out: &mut String) {
    out.push_str("</div>\n</body>\n</html>\n");
}

/// The same sentence every other format uses, with the same numbers.
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

/// The four characters that must never reach the parser as themselves.
///
/// `"` as well as the usual three, because paths and branch names are
/// interpolated into `style` and `title` attributes here, and a quote inside one
/// ends the attribute and puts the rest of the value into the tag.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            ch => out.push(ch),
        }
    }
    out
}
