//! Slack mrkdwn and HTML.
//!
//! Two kinds of assertion here, and the first matters more than the second.
//!
//! **A format is a rendering, never a different answer.** Every format has to
//! carry the same numbers, so the totals are extracted from each one and
//! compared. A format that quietly disagreed with the others would be worse than
//! no format at all, because there would be no way to tell which one was lying.
//!
//! **And each format has to be correct for where it goes.** Slack's mrkdwn is
//! not Markdown, and "renders approximately" is the thing #13 asks to fix — so
//! the specific ways a Markdown paste degrades in Slack are asserted as absent.
//! HTML goes in an email, where a `<style>` block does not survive and a
//! `<script>` is stripped.

use std::path::PathBuf;

use standup::config::{Config, Format};
use standup::model::{
    Activity, CheckoutDigest, CheckoutReport, Churn, Commit, Digest, Dirty, Head, Landed, Note,
    RepoDigest, RepoKey, Severity, Stamp, Tracking, Unpushed, Window, WindowSource, SCHEMA_VERSION,
};
use standup::render;

const FORMATS: [Format; 4] = [Format::Text, Format::Markdown, Format::Slack, Format::Html];

fn stamp(local: &str, epoch: i64) -> Stamp {
    Stamp {
        epoch,
        local: local.to_string(),
        zone: "UTC +0000".to_string(),
    }
}

/// A digest with a number of every kind in it, and a branch name full of the
/// punctuation each format has to survive.
fn digest() -> Digest {
    let commit = Commit {
        oid: "aaaa0001aaaa0001aaaa0001aaaa0001aaaa0001".to_string(),
        committed: stamp("2026-08-15 08:00", 1_786_028_400),
        author: "Agent Smith".to_string(),
        // Every character the four formats treat specially, in one subject:
        // Markdown's brackets and asterisks, Slack's angle brackets and
        // ampersand, HTML's quote.
        subject: "Fix <the> thing & [nearly] *all* of \"it\"".to_string(),
        files: vec!["a.rs".to_string(), "Cargo.lock".to_string()],
        insertions: 42,
        deletions: 7,
        is_merge: false,
    };
    let report = CheckoutReport {
        path: PathBuf::from("/repos/app/wt-feature"),
        repo_key: RepoKey("/repos/app/.git".to_string()),
        repo_root: PathBuf::from("/repos/app"),
        is_linked_worktree: true,
        head: Head::Branch {
            name: "feat/re[factor]<&>".to_string(),
            oid: "aaaa0001aaaa0001aaaa0001aaaa0001aaaa0001".to_string(),
        },
        commits: vec![commit],
        churn: Churn {
            files: 2,
            excluded: 1,
            insertions: 42,
            deletions: 7,
        },
        dirty: Dirty {
            tracked_changed: 3,
            untracked: 1,
            conflicted: 0,
            insertions: 9,
            deletions: 2,
        },
        tracking: Tracking::NoUpstream,
        landed: Landed::NotMerged {
            into: "origin/main".to_string(),
        },
        unpushed: Unpushed::Commits { count: 4 },
        problems: Vec::new(),
    };
    Digest {
        schema: SCHEMA_VERSION,
        generated_at: stamp("2026-08-15 09:12", 1_786_033_920),
        window: Window {
            since: stamp("2026-08-15 00:00", 1_786_000_000),
            until: None,
            source: WindowSource::Default,
        },
        repos: vec![RepoDigest {
            repo_key: RepoKey("/repos/app/.git".to_string()),
            name: "app".to_string(),
            repo_root: PathBuf::from("/repos/app"),
            commits: 1,
            churn: Churn {
                files: 2,
                excluded: 1,
                insertions: 42,
                deletions: 7,
            },
            active_days: 1,
            checkouts: vec![CheckoutDigest {
                report,
                workspaces: Vec::new(),
                agents: Vec::new(),
            }],
        }],
        notes: vec![Note {
            severity: Severity::Warning,
            message: "something <went> wrong & it matters".to_string(),
        }],
    }
}

fn rendered(format: Format) -> String {
    let config = Config {
        format,
        ..Config::default()
    };
    render::render(&digest(), &config).expect("rendered")
}

// ---------------------------------------------------------------------------
// A format is a rendering, never a different answer
// ---------------------------------------------------------------------------

/// The numbers are the product. Every format has to carry the same ones, or
/// there is no way to tell which of them is lying.
#[test]
fn every_format_carries_the_same_numbers() {
    for format in FORMATS {
        let out = rendered(format);
        for number in [
            "1 commit",        // the commit count
            "2 files",         // the file count
            "(1 generated)",   // and what was excluded from the lines
            "+42",             // insertions
            "3 files changed", // uncommitted
            "1 untracked",
            "4 commits on no remote", // unpushed
        ] {
            assert!(
                out.contains(number),
                "{format:?} is missing {number:?}:\n{out}"
            );
        }
        // The minus sign is the real one in every format, not a hyphen.
        assert!(out.contains("\u{2212}7"), "{format:?}:\n{out}");
    }
}

/// And the problems, which outrank brevity in every format.
#[test]
fn every_format_shows_the_notes() {
    for format in FORMATS {
        let out = rendered(format);
        assert!(
            out.contains("wrong") && out.contains("it matters"),
            "{format:?} dropped a warning:\n{out}"
        );
    }
}

// ---------------------------------------------------------------------------
// Slack mrkdwn, which is not Markdown
// ---------------------------------------------------------------------------

/// The four specific ways a Markdown paste degrades in Slack. Each of these is
/// what "renders approximately" means, and each is checked as absent.
#[test]
fn slack_avoids_every_markdown_construct_that_degrades_there() {
    let out = rendered(Format::Slack);

    assert!(
        !out.contains("**"),
        "mrkdwn's bold is a single asterisk; `**` renders literally:\n{out}"
    );
    assert!(
        !out.contains("]("),
        "mrkdwn links are <url|text>; `[text](url)` renders literally:\n{out}"
    );
    assert!(
        !out.lines().any(|line| line.starts_with('#')),
        "mrkdwn has no headings; a `#` renders literally:\n{out}"
    );
    assert!(
        !out.lines().any(|line| line.trim_start().starts_with("- ")),
        "mrkdwn has no list syntax; `- item` renders as those characters:\n{out}"
    );
}

/// Bold is there, with one asterisk, because the digest needs emphasis and this
/// is the only spelling Slack reads.
#[test]
fn slack_bolds_with_a_single_asterisk() {
    let out = rendered(Format::Slack);
    assert!(out.contains("*standup*"), "{out}");
    assert!(
        out.contains("*app*"),
        "the repository name is emphasised:\n{out}"
    );
}

/// Bullets are literal characters, since mrkdwn will not supply them.
#[test]
fn slack_uses_literal_bullet_characters() {
    let out = rendered(Format::Slack);
    assert!(out.contains('\u{2022}'), "no top-level bullet:\n{out}");
    assert!(out.contains('\u{25e6}'), "no nested bullet:\n{out}");
}

/// The three characters Slack interprets, and only those three. Escaping more
/// would put visible backslashes in a channel, because mrkdwn has no escape
/// character — which is the opposite of the Markdown renderer's problem.
#[test]
fn slack_escapes_exactly_the_three_characters_slack_interprets() {
    let out = rendered(Format::Slack);

    assert!(
        out.contains("&lt;the&gt;"),
        "angle brackets unescaped:\n{out}"
    );
    assert!(out.contains("&amp;"), "ampersand unescaped:\n{out}");
    assert!(
        !out.contains("<the>"),
        "a raw angle bracket would be read as markup:\n{out}"
    );
    // And nothing else is escaped: a backslash in front of ordinary punctuation
    // is visible in Slack, so the branch name arrives as it is.
    assert!(
        !out.contains('\\'),
        "mrkdwn has no escape character; a backslash is just a backslash:\n{out}"
    );
    assert!(
        out.contains("re[factor]"),
        "brackets are not special in mrkdwn and must arrive intact:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// HTML, for an email
// ---------------------------------------------------------------------------

/// A complete document with a charset, because the digest is full of `−`, `·`
/// and `…` and a client that guesses latin-1 turns all three into mojibake.
#[test]
fn html_is_a_complete_document_that_declares_its_charset() {
    let out = rendered(Format::Html);
    assert!(out.starts_with("<!doctype html>"), "{out}");
    assert!(out.contains("<meta charset=\"utf-8\">"), "{out}");
    assert!(out.trim_end().ends_with("</html>"), "{out}");
}

/// Email clients strip `<style>` and `<link>`, so a stylesheet is one that will
/// not arrive. Every style is inline, and there is nothing to fetch.
#[test]
fn html_survives_an_email_client() {
    let out = rendered(Format::Html);

    assert!(
        !out.contains("<style"),
        "Gmail strips <style> blocks; styles have to be inline:\n{out}"
    );
    assert!(!out.contains("<link"), "{out}");
    assert!(
        !out.contains("<script"),
        "a script is stripped at best and suspicious at worst:\n{out}"
    );
    assert!(
        !out.contains("http://") && !out.contains("https://"),
        "remote content is blocked by default, so there is none:\n{out}"
    );
    assert!(
        !out.contains("<img"),
        "an image is a blocked resource, which is worse than no image:\n{out}"
    );
    assert!(
        out.contains("style=\""),
        "the styles have to be somewhere:\n{out}"
    );
}

/// Four characters, not three: paths and branch names are interpolated into
/// attributes here, and a quote inside one ends the attribute.
#[test]
fn html_escapes_everything_that_could_become_markup() {
    let out = rendered(Format::Html);

    assert!(out.contains("&lt;the&gt;"), "{out}");
    assert!(out.contains("&amp;"), "{out}");
    assert!(
        out.contains("&quot;"),
        "a quote has to be an entity:\n{out}"
    );
    // The branch name is the dangerous one: it reaches the output verbatim
    // everywhere else.
    assert!(
        out.contains("feat/re[factor]&lt;&amp;&gt;"),
        "the branch name is not escaped:\n{out}"
    );
    // Nothing that came from the digest can have opened a tag. Every `<` left in
    // the output belongs to a tag this module wrote.
    for fragment in ["<the", "<&", "<\"", "&>"] {
        assert!(
            !out.contains(fragment),
            "{fragment:?} reached the output raw:\n{out}"
        );
    }
}

/// Tags are balanced. Not a parser, but enough to catch the mistake that
/// actually happens — a `<p>` or `<table>` opened in one branch and closed in
/// another.
#[test]
fn html_closes_what_it_opens() {
    let out = rendered(Format::Html);
    for tag in [
        "p", "table", "tr", "td", "div", "h1", "span", "body", "html",
    ] {
        let opened =
            out.matches(&format!("<{tag} ")).count() + out.matches(&format!("<{tag}>")).count();
        let closed = out.matches(&format!("</{tag}>")).count();
        assert_eq!(
            opened, closed,
            "{tag}: {opened} opened, {closed} closed:\n{out}"
        );
    }
}

// ---------------------------------------------------------------------------
// The states every format has to be able to say
// ---------------------------------------------------------------------------

/// An empty session and a quiet one must never look the same, in any format.
#[test]
fn every_format_tells_an_empty_session_from_a_quiet_one() {
    let mut empty = digest();
    empty.repos = Vec::new();
    empty.notes = Vec::new();

    let mut quiet = digest();
    quiet.notes = Vec::new();
    quiet.repos[0].checkouts[0].report.commits = Vec::new();
    quiet.repos[0].checkouts[0].report.churn = Churn::default();
    quiet.repos[0].checkouts[0].report.dirty = Dirty::default();
    quiet.repos[0].checkouts[0].report.unpushed = Unpushed::Commits { count: 0 };
    quiet.repos[0].commits = 0;
    quiet.repos[0].churn = Churn::default();
    assert_eq!(
        quiet.repos[0].activity(),
        Activity::Quiet,
        "the fixture is not actually quiet"
    );

    for format in FORMATS {
        let config = Config {
            format,
            ..Config::default()
        };
        let empty = render::render(&empty, &config).expect("rendered");
        let quiet = render::render(&quiet, &config).expect("rendered");
        assert!(
            empty.contains("No repositories were found"),
            "{format:?} on an empty session:\n{empty}"
        );
        assert!(
            quiet.contains("Nothing landed") || quiet.contains("quiet") || quiet.contains("Quiet"),
            "{format:?} on a quiet session:\n{quiet}"
        );
        assert_ne!(empty, quiet, "{format:?} renders both the same way");
    }
}
