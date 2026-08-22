//! What the plumbing cache promises.
//!
//! Two claims, and the second is the one that could quietly ruin the digest.
//!
//! 1. **A hit is used.** Proved by seeding the cache with an answer that git
//!    cannot produce for that repository and watching the report carry it. If the
//!    report says `merged` where the repository says `not merged`, the answer can
//!    only have come from the cache — no counter, no instrumentation, and nothing
//!    that passes when the cache is bypassed.
//! 2. **A checkout that moved misses.** Proved the same way round: seed a wrong
//!    answer, move a sha, and require the *real* answer back. Every test here that
//!    asserts a miss would pass trivially against a cache that never hit, which is
//!    why the hit tests exist beside them.
//!
//! The seeded answers are deliberately absurd — `merged` for a branch that never
//! landed, a fabricated trunk sha — because a plausible one could also have been
//! computed, and then the test would prove nothing.

#[path = "fixtures.rs"]
mod fixtures;

use std::path::Path;
use std::time::Duration;

use standup::cache::Cache;
use standup::git::Git;
use standup::model::{Equivalence, Landed};

use fixtures::{window, Fixture, T_IN1};

const TIMEOUT: Duration = Duration::from_secs(60);

/// The cache key for a checkout as the collector would compute it: HEAD, the
/// trunk's name, and the trunk's own commit.
fn key(fixture: &Fixture, checkout: &Path, trunk: &str) -> String {
    let head = fixture.head_oid(checkout);
    let tip = fixture.git(checkout, &["rev-parse", &format!("{trunk}^{{commit}}")]);
    Cache::answer_key(&head, trunk, &tip)
}

fn report_landed(git: &Git, path: &Path) -> Landed {
    let id = git
        .identify(path)
        .expect("git ran")
        .expect("the fixture is a checkout");
    git.report(&id, &window()).landed
}

// ---------------------------------------------------------------------------
// A hit is used
// ---------------------------------------------------------------------------

#[test]
fn a_seeded_answer_is_what_the_report_carries() {
    let fixture = Fixture::new("hit");
    let branch = fixture.unmerged_worktree("topic", "topic");
    let cache = Cache::in_memory();

    // An answer git would never give for this repository: the branch is not on
    // the trunk by sha and carries no equivalent patch.
    cache.remember_answer(
        key(&fixture, &branch, "main"),
        Landed::Merged {
            into: "main".to_string(),
        },
    );
    let git = Git::new(TIMEOUT).caching(cache);
    assert_eq!(
        report_landed(&git, &branch),
        Landed::Merged {
            into: "main".to_string()
        },
        "the cached answer must be the one used"
    );
}

#[test]
fn without_the_seed_the_same_checkout_reads_not_merged() {
    // The control for the test above. Without it, a cache that returned garbage
    // for every key would pass it.
    let fixture = Fixture::new("hit-control");
    let branch = fixture.unmerged_worktree("topic", "topic");
    let git = Git::new(TIMEOUT);
    assert_eq!(
        report_landed(&git, &branch),
        Landed::NotMerged {
            into: "main".to_string()
        }
    );
}

#[test]
fn a_seeded_trunk_range_is_used_for_the_squash_probe() {
    // The expensive half: the patch ids of `base..trunk`. Seeded with the
    // branch's real combined patch id pointing at a sha that is not in this
    // repository at all, so an `Equivalent` naming it can only have been read
    // from the cache.
    let fixture = Fixture::new("range-hit");
    let branch = fixture.unmerged_worktree("topic", "topic");
    let base = fixture.git(&branch, &["merge-base", "HEAD", "main"]);
    let head = fixture.head_oid(&branch);
    // The same options `git::PATCH_ID_DIFF_OPTIONS` pins. Spelled out rather
    // than imported because the point of pinning them is that the id depends on
    // them: a test that shared the constant could not notice it changing.
    let combined = fixture
        .patch_id(
            &branch,
            &[
                "diff-tree",
                "-p",
                "-U3",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--no-renames",
                "--no-textconv",
                &base,
                &head,
            ],
        )
        .into_iter()
        .next()
        .expect("the branch has a combined diff")
        .0;

    let invented = "d00dfeed".repeat(5);
    let cache = Cache::in_memory();
    let tip = fixture.git(&branch, &["rev-parse", "main^{commit}"]);
    cache.remember_range(
        Cache::range_key(&base, &tip),
        vec![(combined, invented.clone())],
    );

    let git = Git::new(TIMEOUT).caching(cache);
    assert_eq!(
        report_landed(&git, &branch),
        Landed::Equivalent {
            into: "main".to_string(),
            how: Equivalence::Squashed { oid: invented },
        },
        "the stored trunk range must be what the probe compares against"
    );
}

// ---------------------------------------------------------------------------
// Anything that moved misses
// ---------------------------------------------------------------------------

#[test]
fn a_moved_head_misses() {
    let fixture = Fixture::new("moved-head");
    let branch = fixture.unmerged_worktree("topic", "topic");
    let cache = Cache::in_memory();
    cache.remember_answer(
        key(&fixture, &branch, "main"),
        Landed::Merged {
            into: "main".to_string(),
        },
    );

    // One more commit on the branch, and the seeded answer is for a sha that is
    // no longer HEAD.
    fixture.write(&branch, "more.txt", "later work\n");
    fixture.commit_all_at(&branch, T_IN1 + 600, "moved on");

    let git = Git::new(TIMEOUT).caching(cache);
    assert_eq!(
        report_landed(&git, &branch),
        Landed::NotMerged {
            into: "main".to_string()
        },
        "a checkout that moved must be answered afresh"
    );
}

#[test]
fn a_moved_trunk_misses() {
    let fixture = Fixture::new("moved-trunk");
    let branch = fixture.unmerged_worktree("topic", "topic");
    let cache = Cache::in_memory();
    cache.remember_answer(
        key(&fixture, &branch, "main"),
        Landed::Merged {
            into: "main".to_string(),
        },
    );

    // The trunk moves. Nothing about the branch changed, and the answer still
    // has to be recomputed: the trunk is half the question.
    fixture.write(&fixture.repo, "trunk-later.txt", "unrelated\n");
    fixture.commit_all_at(&fixture.repo, T_IN1 + 600, "trunk moved on");

    let git = Git::new(TIMEOUT).caching(cache);
    assert_eq!(
        report_landed(&git, &branch),
        Landed::NotMerged {
            into: "main".to_string()
        },
        "a trunk that moved must not be served yesterday's verdict"
    );
}

#[test]
fn a_squash_merge_after_a_cached_miss_is_found() {
    // The sequence a real repository goes through, and the one a naive cache
    // gets wrong: the branch is reported as not merged, it is then squashed onto
    // the trunk, and the next run must say so.
    let fixture = Fixture::new("squashed-later");
    let branch = fixture.worktree("topic", "topic");
    fixture.write(&branch, "topic-a.txt", "one\ntwo\nthree\n");
    fixture.commit_all_at(&branch, T_IN1, "topic work");

    let cache = Cache::in_memory();
    let git = Git::new(TIMEOUT).caching(cache);
    assert_eq!(
        report_landed(&git, &branch),
        Landed::NotMerged {
            into: "main".to_string()
        },
        "before the merge"
    );

    fixture.git(&fixture.repo, &["merge", "-q", "--squash", "topic"]);
    fixture.commit_all_at(&fixture.repo, T_IN1 + 600, "squash topic (#1)");

    let after = report_landed(&git, &branch);
    assert!(
        matches!(after, Landed::Equivalent { .. }),
        "the same Git, with the miss already cached, must find the squash: {after:?}"
    );
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

#[test]
fn an_in_memory_cache_writes_nothing() {
    let fixture = Fixture::new("no-file");
    let dir = fixture.repo.parent().expect("temp root").join("state");
    let cache = Cache::in_memory();
    cache.remember_answer(
        "k".to_string(),
        Landed::NotMerged {
            into: "main".to_string(),
        },
    );
    cache.save();
    assert!(
        !dir.exists(),
        "the default cache must not create a state directory"
    );
}

#[test]
fn a_saved_answer_survives_into_the_next_run() {
    let fixture = Fixture::new("round-trip");
    let path = fixture.repo.parent().expect("temp root").join("cache.json");

    let first = Cache::at(path.clone());
    first.remember_answer(
        "head main tip".to_string(),
        Landed::Merged {
            into: "main".to_string(),
        },
    );
    first.remember_range(
        Cache::range_key("base", "tip"),
        vec![("id".to_string(), "oid".to_string())],
    );
    first.save();
    assert!(path.is_file(), "the cache file must exist after a save");

    let second = Cache::at(path);
    assert_eq!(
        second.answer("head main tip"),
        Some(Landed::Merged {
            into: "main".to_string()
        })
    );
    assert_eq!(
        second.range(&Cache::range_key("base", "tip")),
        Some(vec![("id".to_string(), "oid".to_string())])
    );
    assert_eq!(second.answer("never stored"), None);
}

#[test]
fn a_cache_written_by_other_probes_is_discarded() {
    // The version is the one thing a wrong answer could hide behind: the stored
    // value is the output of code that may no longer exist.
    let fixture = Fixture::new("version");
    let path = fixture.repo.parent().expect("temp root").join("cache.json");
    let body = format!(
        r#"{{"version":{},"runs":4,"answers":{{"k":{{"used":4,"value":{{"kind":"merged","into":"main"}}}}}},"ranges":{{}}}}"#,
        standup::cache::VERSION + 1
    );
    std::fs::write(&path, body).expect("wrote a cache from another version");

    let cache = Cache::at(path);
    assert_eq!(
        cache.answer("k"),
        None,
        "an entry from other probes must never be served"
    );
}

#[test]
fn an_unreadable_cache_is_an_empty_one() {
    // A cache that refused to run because of its own file would be worse than no
    // cache: the digest is what the user asked for.
    let fixture = Fixture::new("corrupt");
    let dir = fixture.repo.parent().expect("temp root");
    for (name, body) in [
        ("truncated.json", "{\"version\":1,\"runs\":1,\"answ"),
        ("wrong-shape.json", "[]"),
        ("empty.json", ""),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("wrote a broken cache");
        let cache = Cache::at(path.clone());
        assert_eq!(cache.answer("k"), None, "{name}");
        // And it can still be replaced, so one bad file does not poison every
        // later run.
        cache.remember_answer(
            "k".to_string(),
            Landed::NotMerged {
                into: "main".to_string(),
            },
        );
        cache.save();
        assert!(Cache::at(path).answer("k").is_some(), "{name} after a save");
    }
}

#[test]
fn the_file_does_not_grow_without_bound() {
    let fixture = Fixture::new("bounded");
    let path = fixture.repo.parent().expect("temp root").join("cache.json");
    let cache = Cache::at(path.clone());
    for i in 0..2_000 {
        cache.remember_answer(
            format!("key-{i}"),
            Landed::NotMerged {
                into: "main".to_string(),
            },
        );
    }
    cache.save();

    let reloaded = Cache::at(path.clone());
    let kept = (0..2_000)
        .filter(|i| reloaded.answer(&format!("key-{i}")).is_some())
        .count();
    assert!(
        kept > 0 && kept <= 512,
        "entries are capped, and the cap is not zero: kept {kept}"
    );
    let size = std::fs::metadata(&path).expect("the cache file").len();
    assert!(size < 200_000, "the file stays small: {size} bytes");
}

#[test]
fn a_range_too_large_to_store_is_simply_not_stored() {
    // A convenience must not become a liability on a repository with a hundred
    // thousand commits. Refusing to store is not refusing to answer.
    let fixture = Fixture::new("huge-range");
    let path = fixture.repo.parent().expect("temp root").join("cache.json");
    let cache = Cache::at(path);
    let huge: Vec<(String, String)> = (0..20_001)
        .map(|i| (format!("id{i}"), format!("oid{i}")))
        .collect();
    cache.remember_range(Cache::range_key("base", "tip"), huge);
    assert_eq!(cache.range(&Cache::range_key("base", "tip")), None);
}

// ---------------------------------------------------------------------------
// Invisibility
// ---------------------------------------------------------------------------

#[test]
fn a_hit_and_a_miss_produce_the_same_report() {
    // The whole report, not just the landing field: a cache that changed
    // anything else about a checkout would be a cache with an opinion.
    let fixture = Fixture::new("invisible");
    let branch = fixture.squash_merged_worktree("squashed", "squashed");
    let path = fixture.repo.parent().expect("temp root").join("cache.json");

    let cold = Git::new(TIMEOUT).caching(Cache::at(path.clone()));
    let id = cold
        .identify(&branch)
        .expect("git ran")
        .expect("a checkout");
    let first = cold.report(&id, &window());
    cold.save_cache();

    let warm = Git::new(TIMEOUT).caching(Cache::at(path));
    let second = warm.report(&id, &window());

    assert!(
        matches!(first.landed, Landed::Equivalent { .. }),
        "the fixture exercises the expensive path: {:?}",
        first.landed
    );
    assert_eq!(first, second, "a warm run must produce the same report");
}
