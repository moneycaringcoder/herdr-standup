//! Comparing two digests.
//!
//! `compare` is a pure function of two digests, so these tests build both by
//! hand and never touch git or a socket. Every assertion is about *what moved*
//! rather than about how much happened: a comparison that reports totals is a
//! longer digest, which is the one thing #12 says it must not be.

use std::path::PathBuf;

use standup::compare;
use standup::config::{Config, Format};
use standup::model::{
    CheckoutDigest, CheckoutReport, Churn, Commit, Comparison, Digest, Dirty, Head, Landed,
    Movement, Note, RepoDigest, RepoKey, Stamp, Tracking, Unpushed, Window, WindowSource,
    SCHEMA_VERSION,
};
use standup::render;

fn stamp(local: &str, epoch: i64) -> Stamp {
    Stamp {
        epoch,
        local: local.to_string(),
        zone: "UTC +0000".to_string(),
    }
}

fn commit(oid: &str, epoch: i64) -> Commit {
    Commit {
        oid: oid.to_string(),
        committed: stamp("2026-08-15 08:00", epoch),
        author: "Agent Smith".to_string(),
        subject: "Work".to_string(),
        files: vec!["a.rs".to_string()],
        insertions: 1,
        deletions: 0,
        is_merge: false,
    }
}

/// A checkout with everything quiet, which each test then disturbs in exactly
/// one way. Building the interesting state by mutation keeps the difference
/// between two cases visible in the test rather than buried in a constructor.
fn checkout(path: &str) -> CheckoutDigest {
    CheckoutDigest {
        report: CheckoutReport {
            path: PathBuf::from(path),
            repo_key: RepoKey("/repos/app/.git".to_string()),
            repo_root: PathBuf::from("/repos/app"),
            is_linked_worktree: true,
            head: Head::Branch {
                name: "feature/one".to_string(),
                oid: "aaaa0001aaaa0001aaaa0001aaaa0001aaaa0001".to_string(),
            },
            commits: Vec::new(),
            churn: Churn::default(),
            dirty: Dirty::default(),
            tracking: Tracking::NoUpstream,
            landed: Landed::NotMerged {
                into: "origin/main".to_string(),
            },
            unpushed: Unpushed::Commits { count: 0 },
            problems: Vec::new(),
        },
        workspaces: Vec::new(),
        agents: Vec::new(),
    }
}

fn digest(at: &str, epoch: i64, checkouts: Vec<CheckoutDigest>) -> Digest {
    let repos = if checkouts.is_empty() {
        Vec::new()
    } else {
        vec![RepoDigest {
            repo_key: RepoKey("/repos/app/.git".to_string()),
            name: "app".to_string(),
            repo_root: PathBuf::from("/repos/app"),
            commits: checkouts
                .iter()
                .map(|checkout| checkout.report.commits.len())
                .sum(),
            churn: Churn::default(),
            active_days: 0,
            checkouts,
        }]
    };
    Digest {
        schema: SCHEMA_VERSION,
        generated_at: stamp(at, epoch),
        window: Window {
            since: stamp("2026-08-15 00:00", 1_786_000_000),
            until: None,
            source: WindowSource::Default,
        },
        repos,
        notes: Vec::<Note>::new(),
    }
}

fn movement_of(comparison: &Comparison, path: &str) -> Movement {
    comparison
        .repos
        .iter()
        .flat_map(|repo| repo.checkouts.iter())
        .find(|(seen, _)| seen == path)
        .map(|(_, movement)| movement.clone())
        .unwrap_or_else(|| panic!("{path} is not in the comparison"))
}

fn rendered(comparison: &Comparison) -> Vec<String> {
    [Format::Text, Format::Markdown]
        .into_iter()
        .map(|format| {
            let config = Config {
                format,
                ..Config::default()
            };
            render::render_comparison(comparison, &config).expect("rendered")
        })
        .collect()
}

// ---------------------------------------------------------------------------
// What moved
// ---------------------------------------------------------------------------

/// New commits are the first thing a reader wants, and they are counted by oid
/// rather than by total: a digest already says how many there were.
#[test]
fn commits_the_earlier_digest_did_not_have_read_as_new_since() {
    let mut was = checkout("/repos/app/w");
    was.report.commits = vec![commit("1111", 1_786_000_100)];
    let mut now = checkout("/repos/app/w");
    now.report.commits = vec![
        commit("1111", 1_786_000_100),
        commit("2222", 1_786_000_200),
        commit("3333", 1_786_000_300),
    ];

    let comparison = compare::compare(
        &digest("2026-08-15 09:00", 1_786_000_000, vec![was]),
        &digest("2026-08-16 09:00", 1_786_086_400, vec![now]),
    );

    assert_eq!(
        movement_of(&comparison, "/repos/app/w"),
        Movement::Advanced {
            commits: 2,
            landed: false
        },
        "only the two the earlier digest did not have"
    );
    assert_eq!(comparison.total_commits(), 2);
    for rendered in rendered(&comparison) {
        assert!(rendered.contains("2 new since"), "{rendered}");
    }
}

/// Landing is reported whether or not there were also new commits, because
/// "shipped" and "still going" are different things to know.
#[test]
fn work_that_reached_the_trunk_reads_as_landed() {
    let was = checkout("/repos/app/w");
    let mut now = checkout("/repos/app/w");
    now.report.landed = Landed::Merged {
        into: "origin/main".to_string(),
    };

    let comparison = compare::compare(
        &digest("2026-08-15 09:00", 1_786_000_000, vec![was]),
        &digest("2026-08-16 09:00", 1_786_086_400, vec![now]),
    );

    assert_eq!(movement_of(&comparison, "/repos/app/w"), Movement::Landed);
    for rendered in rendered(&comparison) {
        assert!(rendered.contains("landed since"), "{rendered}");
    }
}

/// A squash merge counts as landing. Reporting it as "not landed yet" is the bug
/// #7 fixed, and a comparison that disagreed with the digest beside it would be
/// worse than either.
#[test]
fn a_squash_merge_counts_as_landing_since() {
    let was = checkout("/repos/app/w");
    let mut now = checkout("/repos/app/w");
    now.report.landed = Landed::Equivalent {
        into: "origin/main".to_string(),
        how: standup::model::Equivalence::Squashed {
            oid: "6df5ff43f499b52033c34557418e036589a1854c".to_string(),
        },
    };

    let comparison = compare::compare(
        &digest("2026-08-15 09:00", 1_786_000_000, vec![was]),
        &digest("2026-08-16 09:00", 1_786_086_400, vec![now]),
    );

    assert_eq!(movement_of(&comparison, "/repos/app/w"), Movement::Landed);
}

/// Pushing is not landing: one is about a remote, the other about the trunk, and
/// a reader deciding whether a worktree is safe to delete needs both.
#[test]
fn work_that_reached_a_remote_reads_as_pushed_not_landed() {
    let mut was = checkout("/repos/app/w");
    was.report.unpushed = Unpushed::Commits { count: 3 };
    let now = checkout("/repos/app/w");

    let comparison = compare::compare(
        &digest("2026-08-15 09:00", 1_786_000_000, vec![was]),
        &digest("2026-08-16 09:00", 1_786_086_400, vec![now]),
    );

    assert_eq!(
        movement_of(&comparison, "/repos/app/w"),
        Movement::Pushed { was_holding: 3 }
    );
    for rendered in rendered(&comparison) {
        assert!(rendered.contains("pushed since"), "{rendered}");
        assert!(!rendered.contains("landed"), "{rendered}");
    }
}

/// The comparison's own finding, and the reason it is not just a longer digest:
/// each digest on its own reports the state plainly, and neither says it has not
/// moved.
#[test]
fn a_checkout_that_did_not_move_but_is_still_holding_work_reads_as_stalled() {
    let mut was = checkout("/repos/app/w");
    was.report.unpushed = Unpushed::Commits { count: 2 };
    was.report.dirty = Dirty {
        tracked_changed: 1,
        ..Dirty::default()
    };
    let now = was.clone();

    let comparison = compare::compare(
        &digest("2026-08-15 09:00", 1_786_000_000, vec![was]),
        &digest("2026-08-16 09:00", 1_786_086_400, vec![now]),
    );

    assert_eq!(
        movement_of(&comparison, "/repos/app/w"),
        Movement::Stalled {
            unpushed: 2,
            uncommitted: true
        }
    );
    for rendered in rendered(&comparison) {
        assert!(
            rendered.contains("no new commits, still holding 2 unpushed and uncommitted work"),
            "{rendered}"
        );
    }
}

/// A checkout in the earlier digest and not the later one is where unpushed work
/// goes to die, so the count it *was* holding is carried into the sentence.
#[test]
fn a_checkout_that_vanished_says_what_it_was_holding() {
    let mut was = checkout("/repos/app/gone");
    was.report.unpushed = Unpushed::Commits { count: 4 };

    let comparison = compare::compare(
        &digest("2026-08-15 09:00", 1_786_000_000, vec![was]),
        &digest("2026-08-16 09:00", 1_786_086_400, Vec::new()),
    );

    assert_eq!(
        movement_of(&comparison, "/repos/app/gone"),
        Movement::Gone { was_holding: 4 }
    );
    for rendered in rendered(&comparison) {
        assert!(
            rendered.contains("gone, and was holding 4 that were only there"),
            "{rendered}"
        );
    }
}

#[test]
fn a_checkout_that_is_new_reads_as_new_here() {
    let now = checkout("/repos/app/fresh");

    let comparison = compare::compare(
        &digest("2026-08-15 09:00", 1_786_000_000, Vec::new()),
        &digest("2026-08-16 09:00", 1_786_086_400, vec![now]),
    );

    assert_eq!(
        movement_of(&comparison, "/repos/app/fresh"),
        Movement::Appeared { commits: 0 }
    );
    for rendered in rendered(&comparison) {
        assert!(rendered.contains("new here"), "{rendered}");
    }
}

/// Matched by path, because that is the only identity a checkout keeps across
/// two runs: the branch here is renamed and HEAD has moved, and it is still the
/// same checkout.
#[test]
fn checkouts_are_matched_by_path_not_by_branch() {
    let was = checkout("/repos/app/w");
    let mut now = checkout("/repos/app/w");
    now.report.head = Head::Branch {
        name: "feature/renamed".to_string(),
        oid: "bbbb0002bbbb0002bbbb0002bbbb0002bbbb0002".to_string(),
    };

    let comparison = compare::compare(
        &digest("2026-08-15 09:00", 1_786_000_000, vec![was]),
        &digest("2026-08-16 09:00", 1_786_086_400, vec![now]),
    );

    assert_eq!(
        movement_of(&comparison, "/repos/app/w"),
        Movement::Unchanged,
        "a renamed branch is not a movement; nothing was committed, landed or lost"
    );
}

// ---------------------------------------------------------------------------
// How it reads
// ---------------------------------------------------------------------------

/// The whole request: a comparison, not a longer digest. So no churn, no commit
/// subjects, and no line volume — those are what a digest is for, and repeating
/// them here is exactly how this would turn back into one.
#[test]
fn a_comparison_does_not_read_like_a_digest() {
    let mut was = checkout("/repos/app/w");
    was.report.commits = vec![commit("1111", 1_786_000_100)];
    let mut now = checkout("/repos/app/w");
    now.report.commits = vec![commit("1111", 1_786_000_100), commit("2222", 1_786_000_200)];
    now.report.churn = Churn {
        files: 9,
        excluded: 0,
        insertions: 400,
        deletions: 12,
    };

    let comparison = compare::compare(
        &digest("2026-08-15 09:00", 1_786_000_000, vec![was]),
        &digest("2026-08-16 09:00", 1_786_086_400, vec![now]),
    );

    for rendered in rendered(&comparison) {
        assert!(rendered.contains("1 new since"), "{rendered}");
        assert!(
            !rendered.contains("400"),
            "line volume is a digest's answer, not a comparison's:\n{rendered}"
        );
        assert!(
            !rendered.contains("9 files"),
            "file counts likewise:\n{rendered}"
        );
        assert!(
            !rendered.contains("Work"),
            "a comparison lists no commit subjects:\n{rendered}"
        );
    }
}

/// Both instants are named, and neither is truncated away. Two full stamps and a
/// sentence do not fit in eighty columns, which is why the terminal form puts
/// them on their own lines.
#[test]
fn both_instants_survive_the_eighty_column_budget() {
    let comparison = compare::compare(
        &digest(
            "2026-08-15 09:00",
            1_786_000_000,
            vec![checkout("/repos/app/w")],
        ),
        &digest(
            "2026-08-16 09:00",
            1_786_086_400,
            vec![checkout("/repos/app/w")],
        ),
    );

    for rendered in rendered(&comparison) {
        assert!(rendered.contains("2026-08-15 09:00"), "{rendered}");
        assert!(rendered.contains("2026-08-16 09:00"), "{rendered}");
    }
    let text = &rendered(&comparison)[0];
    for line in text.lines() {
        assert!(
            line.chars().count() <= 80,
            "{} columns: {line:?}",
            line.chars().count()
        );
    }
}

/// Nothing moving is a real answer and has to read as one. An empty screen and a
/// quiet comparison must not look the same — the same rule the digest follows.
#[test]
fn a_comparison_where_nothing_moved_says_so() {
    let comparison = compare::compare(
        &digest(
            "2026-08-15 09:00",
            1_786_000_000,
            vec![checkout("/repos/app/w")],
        ),
        &digest(
            "2026-08-16 09:00",
            1_786_086_400,
            vec![checkout("/repos/app/w")],
        ),
    );

    assert!(comparison.is_quiet());
    for rendered in rendered(&comparison) {
        assert!(rendered.contains("Nothing moved"), "{rendered}");
    }
}

#[test]
fn comparing_two_empty_digests_says_there_was_nothing_to_compare() {
    let comparison = compare::compare(
        &digest("2026-08-15 09:00", 1_786_000_000, Vec::new()),
        &digest("2026-08-16 09:00", 1_786_086_400, Vec::new()),
    );

    for rendered in rendered(&comparison) {
        assert!(rendered.contains("nothing to compare"), "{rendered}");
    }
}

/// The findings a reader has to act on are marked, and the marking is the same
/// in both human formats.
#[test]
fn the_findings_that_need_acting_on_are_marked() {
    let mut stalled = checkout("/repos/app/stalled");
    stalled.report.unpushed = Unpushed::Commits { count: 1 };
    let mut landed = checkout("/repos/app/landed");
    landed.report.landed = Landed::Merged {
        into: "origin/main".to_string(),
    };

    let comparison = compare::compare(
        &digest(
            "2026-08-15 09:00",
            1_786_000_000,
            vec![stalled.clone(), checkout("/repos/app/landed")],
        ),
        &digest("2026-08-16 09:00", 1_786_086_400, vec![stalled, landed]),
    );

    let text = &rendered(&comparison)[0];
    assert!(
        text.contains("! no new commits, still holding"),
        "a stalled checkout is a finding:\n{text}"
    );
    assert!(
        !text.contains("! landed since"),
        "landing is good news and is not marked:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// Reading a saved digest
// ---------------------------------------------------------------------------

/// The JSON round-trips through the same types that wrote it, which is the whole
/// reason the model derives `Deserialize` rather than a second shape being
/// maintained beside it.
#[test]
fn a_digest_written_as_json_reads_back_identically() {
    let original = digest(
        "2026-08-15 09:00",
        1_786_000_000,
        vec![checkout("/repos/app/w")],
    );
    let dir = std::env::temp_dir().join(format!("standup-compare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("digest.json");
    std::fs::write(&path, render::json(&original).expect("rendered")).expect("written");

    let read = compare::read_digest(&path).expect("read back");
    assert_eq!(read, original, "the JSON is not a faithful round trip");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A shape this binary does not know is refused by name. A comparison built on a
/// misread digest would be confidently wrong about what moved, which is worse
/// than refusing to build one.
#[test]
fn a_digest_from_another_schema_is_refused_by_name() {
    let dir = std::env::temp_dir().join(format!("standup-schema-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let future = dir.join("future.json");
    std::fs::write(&future, r#"{"schema": 99, "repos": []}"#).expect("written");
    let err = compare::read_digest(&future).expect_err("schema 99 is not readable");
    assert!(err.to_string().contains("schema 99"), "{err}");

    let missing = dir.join("missing.json");
    std::fs::write(&missing, r#"{"repos": []}"#).expect("written");
    let err = compare::read_digest(&missing).expect_err("no schema is not a digest");
    assert!(err.to_string().contains("no `schema` field"), "{err}");

    let garbage = dir.join("garbage.json");
    std::fs::write(&garbage, "not json at all").expect("written");
    let err = compare::read_digest(&garbage).expect_err("not JSON");
    assert!(err.to_string().contains("not JSON standup wrote"), "{err}");

    let absent = dir.join("absent.json");
    let err = compare::read_digest(&absent).expect_err("no such file");
    assert!(
        err.to_string().contains("could not read the digest"),
        "{err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
