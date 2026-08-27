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

use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

// ---------------------------------------------------------------------------
// Herdr plugin actions
// ---------------------------------------------------------------------------

const MANIFEST: &str = include_str!("../herdr-plugin.toml");
const ACTION_IDS: [&str; 7] = [
    "today",
    "since-last",
    "yesterday",
    "markdown",
    "slack",
    "html",
    "json",
];

fn manifest_entries(section: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    let header = format!("[[{section}]]");
    MANIFEST
        .split(header.as_str())
        .skip(1)
        .map(|rest| {
            let block = rest.split("\n[[").next().expect("manifest section");
            let id = block
                .lines()
                .find_map(|line| line.strip_prefix("id = \"")?.strip_suffix('"'))
                .expect("entry id");
            let title = block
                .lines()
                .find_map(|line| line.strip_prefix("title = \"")?.strip_suffix('"'))
                .expect("entry title");
            let command = block
                .lines()
                .find_map(|line| line.strip_prefix("command = "))
                .expect("entry command");
            (id, title, command)
        })
        .collect()
}

struct FakeHerdr {
    root: PathBuf,
    binary: PathBuf,
    argv: PathBuf,
}

impl FakeHerdr {
    fn new(exit_code: i32) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);

        let root = std::env::temp_dir().join(format!(
            "standup-action-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).expect("action test directory");
        let binary = root.join("fake herdr");
        let argv = root.join("argv");
        std::fs::write(
            &binary,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HERDR_TEST_ARGV\"\nexit {exit_code}\n"),
        )
        .expect("fake Herdr executable");
        let mut permissions = std::fs::metadata(&binary)
            .expect("fake Herdr metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).expect("fake Herdr permissions");

        Self { root, binary, argv }
    }

    fn invoke(&self, action_id: &str) -> Output {
        Command::new(BIN)
            // If action dispatch does not happen before parsing and collection,
            // this deliberately invalid option makes the process fail.
            .arg("--not-a-real-standup-option")
            .env("HERDR_PLUGIN_ACTION_ID", action_id)
            .env("HERDR_BIN_PATH", &self.binary)
            .env("HERDR_PLUGIN_ID", "plugin id;still-one-argument")
            .env("HERDR_TEST_ARGV", &self.argv)
            .output()
            .expect("standup action ran")
    }

    fn recorded_argv(&self) -> Vec<String> {
        std::fs::read_to_string(&self.argv)
            .expect("fake Herdr recorded argv")
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for FakeHerdr {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn every_declared_action_opens_its_same_id_one_shot_pane() {
    let actions = manifest_entries("actions");
    let panes = manifest_entries("panes");
    assert_eq!(
        actions.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        ACTION_IDS
    );
    assert_eq!(
        actions, panes,
        "every action must have a same-id, same-title pane reusing its report command"
    );

    let fake = FakeHerdr::new(0);
    for action_id in actions.iter().map(|entry| entry.0) {
        let output = fake.invoke(action_id);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{action_id}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fake.recorded_argv(),
            [
                "plugin",
                "pane",
                "open",
                "--plugin",
                "plugin id;still-one-argument",
                "--entrypoint",
                action_id,
            ],
            "{action_id}"
        );
        assert!(
            output.stdout.is_empty(),
            "the action surface is the pane, not standup stdout"
        );
    }
}

#[test]
fn pane_entrypoints_stay_on_the_existing_report_path() {
    let fixture = Fixture::new("pane-entrypoint");
    let output = Command::new(BIN)
        .args(["--report", "--offline", "--path"])
        .arg(&fixture.repo)
        .args(["--since", &format!("@{T_SINCE}")])
        .env("HERDR_PLUGIN_ACTION_ID", "")
        .env("HERDR_PLUGIN_ENTRYPOINT_ID", "today")
        .env("HERDR_BIN_PATH", "/must/not/be/invoked")
        .env("HERDR_PLUGIN_ID", "moneycaringcoder.standup")
        .output()
        .expect("pane entrypoint report ran");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        flatten(&String::from_utf8_lossy(&output.stdout)).contains("standup"),
        "the pane command must render the report normally"
    );
}

#[test]
fn a_plugin_pane_stays_visible_until_its_input_closes() {
    let fixture = Fixture::new("pane-lifetime");
    let temp = FakeHerdr::new(0);
    let output_path = temp.root.join("pane-output");
    let output_file = File::create(&output_path).expect("pane output file");
    let mut child = Command::new(BIN)
        .args(["--report", "--offline", "--path"])
        .arg(&fixture.repo)
        .args(["--since", &format!("@{T_SINCE}")])
        .env("HERDR_PLUGIN_ACTION_ID", "")
        .env("HERDR_PLUGIN_ENTRYPOINT_ID", "today")
        .stdin(Stdio::piped())
        .stdout(output_file)
        .stderr(Stdio::piped())
        .spawn()
        .expect("plugin pane report started");
    let input = child.stdin.take().expect("piped pane input");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rendered = std::fs::read_to_string(&output_path).expect("read pane output");
        if flatten(&rendered).contains("standup") {
            break;
        }
        if let Some(status) = child.try_wait().expect("poll pane report") {
            let stderr = child
                .stderr
                .take()
                .map(|mut pipe| {
                    use std::io::Read;
                    let mut text = String::new();
                    pipe.read_to_string(&mut text).expect("read pane stderr");
                    text
                })
                .unwrap_or_default();
            panic!("plugin pane exited early with {status}: {stderr}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("plugin pane did not render within {deadline:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        child.try_wait().expect("poll held pane").is_none(),
        "the rendered pane must remain available while Herdr keeps stdin open"
    );
    drop(input);
    let output = child.wait_with_output().expect("pane report exited at EOF");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_action_requires_the_injected_herdr_binary_and_plugin_id() {
    let missing_binary = Command::new(BIN)
        .env("HERDR_PLUGIN_ACTION_ID", "today")
        .env_remove("HERDR_BIN_PATH")
        .env("HERDR_PLUGIN_ID", "moneycaringcoder.standup")
        .output()
        .expect("standup action ran");
    assert_eq!(missing_binary.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&missing_binary.stderr),
        "standup: HERDR_BIN_PATH is required when HERDR_PLUGIN_ACTION_ID is set\n"
    );

    let fake = FakeHerdr::new(0);
    let missing_plugin = Command::new(BIN)
        .env("HERDR_PLUGIN_ACTION_ID", "today")
        .env("HERDR_BIN_PATH", &fake.binary)
        .env("HERDR_PLUGIN_ID", "")
        .env("HERDR_TEST_ARGV", &fake.argv)
        .output()
        .expect("standup action ran");
    assert_eq!(missing_plugin.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&missing_plugin.stderr),
        "standup: HERDR_PLUGIN_ID is required when HERDR_PLUGIN_ACTION_ID is set\n"
    );
    assert!(
        !fake.argv.exists(),
        "configuration errors must fail before spawning Herdr"
    );
}

#[test]
fn an_action_names_spawn_and_nonzero_herdr_failures() {
    let fake = FakeHerdr::new(23);
    let failed = fake.invoke("json");
    assert_eq!(failed.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&failed.stderr),
        format!(
            "standup: HERDR_BIN_PATH {:?} returned exit status: 23 while opening plugin pane \"json\"\n",
            fake.binary.as_os_str()
        )
    );

    let missing = fake.root.join("missing-herdr");
    let not_spawned = Command::new(BIN)
        .env("HERDR_PLUGIN_ACTION_ID", "today")
        .env("HERDR_BIN_PATH", &missing)
        .env("HERDR_PLUGIN_ID", "moneycaringcoder.standup")
        .output()
        .expect("standup action ran");
    assert_eq!(not_spawned.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&not_spawned.stderr),
        format!(
            "standup: failed to spawn HERDR_BIN_PATH {:?}: {}\n",
            missing.as_os_str(),
            std::io::Error::from_raw_os_error(2)
        )
    );
}
