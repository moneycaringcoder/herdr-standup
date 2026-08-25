//! What the exit status promises, asserted by running the real binary.
//!
//! `--fail-if-empty` exists for cron and CI, so the thing under test is the
//! process's status, not a function's return value. Every case below spawns
//! `standup` against a throwaway git repository with `--offline`, which is the
//! shape a scheduled job actually has: no herdr socket, an explicit `--path`,
//! and a decision to make afterwards.
//!
//! Two claims are load-bearing.
//!
//! 1. **A quiet day is not a failure.** Empty exits 2 and a broken run exits 1,
//!    because a caller that cannot tell them apart will either post nothing on
//!    the day something breaks or page somebody on a Sunday.
//! 2. **Work at risk is not empty.** Uncommitted and unpushed work is the case
//!    most worth posting; a flag meant to catch silence that swallowed it would
//!    be worse than no flag.

#[path = "fixtures.rs"]
mod fixtures;

use std::path::Path;
use std::process::Command;

use fixtures::{since_as_filter, Fixture, T_AFTER, T_IN1, T_OLD, T_SINCE};

/// The binary this crate builds, which is what a cron line runs.
const BIN: &str = env!("CARGO_BIN_EXE_standup");

struct Run {
    code: Option<i32>,
    stdout: String,
}

/// Runs the digest against one checkout, with no socket and an explicit window.
fn run(path: &Path, since: i64, extra: &[&str]) -> Run {
    let out = Command::new(BIN)
        .args(["--offline", "--path"])
        .arg(path)
        .args(["--since", &format!("@{since}")])
        .args(extra)
        .output()
        .expect("standup ran");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
    }
}

fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The status a quiet checkout yields under `--fail-if-empty`, on this git.
///
/// 2 wherever the window can be filtered. Below git 2.37 the collector records
/// the `--max-age` fallback on every checkout it walks, and a note saying "these
/// numbers are a lower bound" is something to report rather than silence — so
/// the same run is 0, with the note in the digest. Both are the promise this
/// file exists for, which is that the status and the words agree.
fn quiet_status() -> Option<i32> {
    if since_as_filter() {
        Some(2)
    } else {
        Some(0)
    }
}

/// What a quiet digest says, on this git: silence, or the reason it is a lower
/// bound.
fn quiet_words() -> &'static str {
    if since_as_filter() {
        "Nothing landed in this window"
    } else {
        "does not support --since-as-filter"
    }
}

// ---------------------------------------------------------------------------
// Nothing to report
// ---------------------------------------------------------------------------

#[test]
fn without_the_flag_an_empty_digest_still_succeeds() {
    let fixture = Fixture::new("empty-default");
    // The only commit is older than the window, and the tree is clean.
    let quiet = run(&fixture.repo, T_SINCE, &[]);
    assert_eq!(
        quiet.code,
        Some(0),
        "the default behaviour is unchanged:\n{}",
        quiet.stdout
    );
    assert!(
        flatten(&quiet.stdout).contains(quiet_words()),
        "and it says so in words:\n{}",
        quiet.stdout
    );
}

#[test]
fn an_empty_digest_exits_two_and_still_prints() {
    let fixture = Fixture::new("empty-flagged");
    let quiet = run(&fixture.repo, T_SINCE, &["--fail-if-empty"]);
    assert_eq!(
        quiet.code,
        quiet_status(),
        "empty is 2, unless the run has a degradation to report:\n{}",
        quiet.stdout
    );
    // Suppressing the output would take away the one thing that tells a person
    // reading the cron mail why the run failed.
    assert!(
        flatten(&quiet.stdout).contains(quiet_words()),
        "the digest still prints:\n{}",
        quiet.stdout
    );
}

#[test]
fn a_digest_with_commits_exits_zero() {
    let fixture = Fixture::new("busy");
    fixture.commits_around_the_window();
    let busy = run(&fixture.repo, T_SINCE, &["--fail-if-empty"]);
    assert_eq!(
        busy.code,
        Some(0),
        "the flag must not change what a digest with content does:\n{}",
        busy.stdout
    );
}

#[test]
fn empty_is_not_the_same_status_as_broken() {
    // 1 is a failure. If empty were also 1, a cron line could not tell "quiet
    // day" from "the digest is broken", which are opposite messages.
    let broken = Command::new(BIN)
        .args(["--since", "--fail-if-empty"])
        .output()
        .expect("standup ran");
    assert_eq!(
        broken.status.code(),
        Some(1),
        "a malformed command line is still a failure: {}",
        String::from_utf8_lossy(&broken.stderr)
    );

    let fixture = Fixture::new("empty-vs-broken");
    let quiet = run(&fixture.repo, T_SINCE, &["--fail-if-empty"]);
    assert_ne!(
        quiet.code,
        broken.status.code(),
        "and a quiet day must not wear the same status"
    );
}

// ---------------------------------------------------------------------------
// What counts as something
// ---------------------------------------------------------------------------

#[test]
fn uncommitted_work_is_not_nothing() {
    let fixture = Fixture::new("uncommitted");
    // No commits in the window; a file changed and never committed.
    fixture.write(&fixture.repo, "base.txt", "edited, not committed\n");
    let run = run(&fixture.repo, T_SINCE, &["--fail-if-empty"]);
    assert_eq!(
        run.code,
        Some(0),
        "work in flight is the case most worth posting:\n{}",
        run.stdout
    );
    assert!(
        flatten(&run.stdout).contains("uncommitted"),
        "and it is in the digest:\n{}",
        run.stdout
    );
}

#[test]
fn an_untracked_file_is_not_nothing() {
    let fixture = Fixture::new("untracked");
    fixture.write(&fixture.repo, "scratch.txt", "notes\n");
    let run = run(&fixture.repo, T_SINCE, &["--fail-if-empty"]);
    assert_eq!(
        run.code,
        Some(0),
        "an untracked file is somebody's work with nothing holding it:\n{}",
        run.stdout
    );
}

#[test]
fn a_commit_that_exists_only_here_is_not_nothing() {
    // Committed, on no remote, and outside the window: the digest has no commit
    // lines to show, and the work is still at risk. #8's state, and this flag
    // must not report it as silence.
    let fixture = Fixture::new("unpushed");
    fixture.fake_origin();
    fixture.publish("main", "HEAD");
    fixture.write(&fixture.repo, "later.txt", "kept locally\n");
    fixture.commit_all_at(&fixture.repo, T_OLD + 60, "committed, never pushed");

    let run = run(&fixture.repo, T_SINCE, &["--fail-if-empty"]);
    assert_eq!(
        run.code,
        Some(0),
        "a commit that exists only here is not an empty digest:\n{}",
        run.stdout
    );
}

#[test]
fn a_window_that_excludes_the_work_is_empty_again() {
    // The mirror of the case above: the same repository, a window that starts
    // after everything, and a clean tree. Nothing at risk, nothing to say.
    let fixture = Fixture::new("after-everything");
    fixture.commits_around_the_window();
    let quiet = run(&fixture.repo, T_AFTER, &["--fail-if-empty"]);
    assert_eq!(quiet.code, quiet_status(), "{}", quiet.stdout);
}

// ---------------------------------------------------------------------------
// Every format, and the comparison
// ---------------------------------------------------------------------------

#[test]
fn the_status_is_the_same_in_every_format() {
    let quiet = Fixture::new("formats-quiet");
    let busy = Fixture::new("formats-busy");
    busy.commits_around_the_window();

    for verb in ["--report", "--markdown", "--slack", "--html", "--json"] {
        let empty = run(&quiet.repo, T_SINCE, &[verb, "--fail-if-empty"]);
        assert_eq!(
            empty.code,
            quiet_status(),
            "{verb} disagreed about an empty digest:\n{}",
            empty.stdout
        );
        let full = run(&busy.repo, T_SINCE, &[verb, "--fail-if-empty"]);
        assert_eq!(
            full.code,
            Some(0),
            "{verb} disagreed about a digest with content:\n{}",
            full.stdout
        );
    }
}

#[test]
fn a_comparison_where_nothing_moved_is_empty() {
    // With `--diff`, the comparison is what a caller would post, so that is what
    // "empty" has to describe. The digest underneath has commits in it.
    let fixture = Fixture::new("diff-unchanged");
    fixture.commits_around_the_window();

    let snapshot = Command::new(BIN)
        .args(["--json", "--offline", "--path"])
        .arg(&fixture.repo)
        .args(["--since", &format!("@{T_SINCE}")])
        .output()
        .expect("standup ran");
    assert_eq!(snapshot.status.code(), Some(0));
    let saved = fixture
        .repo
        .parent()
        .expect("temp root")
        .join("before.json");
    std::fs::write(&saved, &snapshot.stdout).expect("wrote the snapshot");

    let digest = run(&fixture.repo, T_SINCE, &["--fail-if-empty"]);
    assert_eq!(
        digest.code,
        Some(0),
        "the digest itself has content:\n{}",
        digest.stdout
    );

    let unchanged = run(
        &fixture.repo,
        T_SINCE,
        &[
            "--diff",
            saved.to_str().expect("utf-8 path"),
            "--fail-if-empty",
        ],
    );
    assert_eq!(
        unchanged.code,
        Some(2),
        "nothing moved between the two:\n{}",
        unchanged.stdout
    );

    // And once something moves, the same comparison succeeds.
    fixture.write(&fixture.repo, "moved.txt", "new work\n");
    fixture.commit_all_at(&fixture.repo, T_IN1 + 120, "moved since the snapshot");
    let moved = run(
        &fixture.repo,
        T_SINCE,
        &[
            "--diff",
            saved.to_str().expect("utf-8 path"),
            "--fail-if-empty",
        ],
    );
    assert_eq!(
        moved.code,
        Some(0),
        "a comparison with a finding in it is not empty:\n{}",
        moved.stdout
    );
}
