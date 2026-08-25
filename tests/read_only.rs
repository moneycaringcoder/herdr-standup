//! Proof of the plugin's central safety claim: building a digest from a
//! repository changes nothing in it.
//!
//! Every other test here checks that standup computes the right answer. This one
//! checks that computing it costs the user nothing — no index writeback, no
//! stray object, no touched file, no leftover lock, no moved ref. The claim used
//! to live only in prose, and prose does not fail CI.
//!
//! The fingerprint deliberately covers more than the assertions strictly need:
//! the whole common git directory (minus the object store, which is compared by
//! path set), every index with its mtime, every working-tree file including
//! untracked and ignored ones, every ref, every reflog, the config, and the
//! loose-object and pack inventories. Anything git writes shows up.

#[path = "fixtures.rs"]
mod fixtures;

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use standup::git::Git;
use standup::model::Landed;

use fixtures::{window, Fixture, T_IN1, T_IN2, T_IN3};

const TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Fingerprinting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    hash: u64,
    len: u64,
}

fn hash_of(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn stamp(path: &Path) -> FileStamp {
    let bytes = std::fs::read(path).unwrap_or_default();
    FileStamp {
        hash: hash_of(&bytes),
        len: bytes.len() as u64,
    }
}

/// Everything about a repository that a read-only operation must leave alone.
#[derive(Debug)]
struct Fingerprint {
    /// Full bytes of every index file, so a failure can say what changed rather
    /// than only that something did.
    index_bytes: BTreeMap<PathBuf, Vec<u8>>,
    /// mtime of every index file. A stat-cache writeback moves this even when
    /// the contents happen to round-trip identically, so it is asserted
    /// separately rather than trusted as the only signal.
    index_mtimes: BTreeMap<PathBuf, SystemTime>,
    /// Every file in the common git dir (excluding the object store) and in
    /// every working tree, untracked and ignored files included.
    files: BTreeMap<PathBuf, FileStamp>,
    /// Loose objects, by path.
    loose: BTreeSet<PathBuf>,
    /// Packs and their indexes, by path.
    packs: BTreeSet<PathBuf>,
    /// Refs, worktree list and config as git itself reports them.
    refs: String,
    config: String,
    reflogs: BTreeMap<PathBuf, FileStamp>,
    /// Any `*.lock` present. Excluded from `files` because a lock is transient
    /// by nature; tracked here so leftovers are still caught.
    locks: BTreeSet<PathBuf>,
}

fn walk(
    root: &Path,
    exclude: &[PathBuf],
    files: &mut BTreeMap<PathBuf, FileStamp>,
    locks: &mut BTreeSet<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if exclude.contains(&path) {
            continue;
        }
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&path, exclude, files, locks),
            Ok(_) => {
                if path.extension().map(|e| e == "lock").unwrap_or(false) {
                    locks.insert(path);
                } else {
                    let s = stamp(&path);
                    files.insert(path, s);
                }
            }
            Err(_) => {}
        }
    }
}

fn collect_paths(root: &Path, out: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_paths(&path, out),
            Ok(_) => {
                out.insert(path);
            }
            Err(_) => {}
        }
    }
}

fn fingerprint(fixture: &Fixture, worktrees: &[PathBuf]) -> Fingerprint {
    fingerprint_of(fixture, &fixture.repo, worktrees)
}

/// The same, for a repository that is not the fixture's primary one — the
/// partial clone is its own repository with its own object store.
fn fingerprint_of(fixture: &Fixture, repo: &Path, worktrees: &[PathBuf]) -> Fingerprint {
    let common_dir = fixture.common_dir(repo);
    let objects = common_dir.join("objects");

    let mut files = BTreeMap::new();
    let mut locks = BTreeSet::new();
    // The object store is compared as a path set instead of file-by-file, so it
    // is excluded from the byte walk.
    walk(
        &common_dir,
        std::slice::from_ref(&objects),
        &mut files,
        &mut locks,
    );
    for worktree in worktrees {
        // `.git` inside a linked worktree is a gitlink file, and inside the main
        // worktree it is the git dir itself; either way it is repository state,
        // already covered by the common-dir walk.
        walk(worktree, &[worktree.join(".git")], &mut files, &mut locks);
    }

    let mut loose = BTreeSet::new();
    collect_paths(&objects, &mut loose);
    let packs: BTreeSet<PathBuf> = loose
        .iter()
        .filter(|path| path.parent().map(|p| p.ends_with("pack")).unwrap_or(false))
        .cloned()
        .collect();
    loose.retain(|path| !packs.contains(path));

    let mut index_bytes = BTreeMap::new();
    let mut index_mtimes = BTreeMap::new();
    for worktree in worktrees {
        let index = fixture.git_dir(worktree).join("index");
        if let Ok(bytes) = std::fs::read(&index) {
            index_bytes.insert(index.clone(), bytes);
            if let Ok(mtime) = std::fs::metadata(&index).and_then(|m| m.modified()) {
                index_mtimes.insert(index, mtime);
            }
        }
    }

    let mut refs = fixture.git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname) %(objecttype)",
        ],
    );
    refs.push('\n');
    refs.push_str(&fixture.git(repo, &["worktree", "list", "--porcelain"]));

    let config = fixture.git(repo, &["config", "--list", "--local"]);

    let mut reflogs = BTreeMap::new();
    let mut reflog_locks = BTreeSet::new();
    walk(
        &common_dir.join("logs"),
        &[],
        &mut reflogs,
        &mut reflog_locks,
    );
    for worktree in worktrees {
        walk(
            &fixture.git_dir(worktree).join("logs"),
            &[],
            &mut reflogs,
            &mut reflog_locks,
        );
    }

    Fingerprint {
        index_bytes,
        index_mtimes,
        files,
        loose,
        packs,
        refs,
        config,
        reflogs,
        locks,
    }
}

/// Reports every difference, so one run names all the damage instead of only
/// the first byte that moved.
fn assert_unchanged(before: &Fingerprint, after: &Fingerprint) {
    let mut problems: Vec<String> = Vec::new();

    // 1. Indexes, byte for byte, plus mtime. This is what `--no-optional-locks`
    //    exists for: plain `status` rewrites the index to save its stat cache.
    for (path, bytes) in &before.index_bytes {
        match after.index_bytes.get(path) {
            None => problems.push(format!("index removed: {}", path.display())),
            Some(now) if now != bytes => problems.push(format!(
                "index rewritten: {} ({} bytes -> {} bytes)",
                path.display(),
                bytes.len(),
                now.len()
            )),
            Some(_) => {}
        }
    }
    for path in after.index_bytes.keys() {
        if !before.index_bytes.contains_key(path) {
            problems.push(format!("index created: {}", path.display()));
        }
    }
    for (path, mtime) in &before.index_mtimes {
        if after.index_mtimes.get(path) != Some(mtime) {
            problems.push(format!(
                "index mtime moved (stat-cache writeback): {}",
                path.display()
            ));
        }
    }

    // 2. Working trees, refs, reflogs, config, and the rest of the git dir.
    for (path, was) in &before.files {
        match after.files.get(path) {
            None => problems.push(format!("file removed: {}", path.display())),
            Some(now) if now != was => problems.push(format!("file modified: {}", path.display())),
            Some(_) => {}
        }
    }
    for path in after.files.keys() {
        if !before.files.contains_key(path) {
            problems.push(format!("file created: {}", path.display()));
        }
    }
    if before.refs != after.refs {
        problems.push(format!(
            "refs changed:\n--- before\n{}\n--- after\n{}",
            before.refs, after.refs
        ));
    }
    if before.config != after.config {
        problems.push("the repository config was rewritten".to_string());
    }
    for (path, was) in &before.reflogs {
        if after.reflogs.get(path) != Some(was) {
            problems.push(format!("reflog changed: {}", path.display()));
        }
    }
    for path in after.reflogs.keys() {
        if !before.reflogs.contains_key(path) {
            problems.push(format!("reflog created: {}", path.display()));
        }
    }

    // 3. The object store. Growth here is the failure mode that matters most:
    //    anything that stages, writes a tree or merges leaves objects behind.
    let new_loose: Vec<&PathBuf> = after.loose.difference(&before.loose).collect();
    if !new_loose.is_empty() {
        problems.push(format!(
            "{} loose object(s) leaked into the user's ODB, first few: {:?}",
            new_loose.len(),
            new_loose.iter().take(5).collect::<Vec<_>>()
        ));
    }
    if before.loose.len() != after.loose.len() {
        problems.push(format!(
            "loose object count changed: {} -> {}",
            before.loose.len(),
            after.loose.len()
        ));
    }
    if before.packs != after.packs {
        problems.push(format!(
            "the pack list changed: {:?} -> {:?}",
            before.packs, after.packs
        ));
    }

    // 4. Locks. Anything new is a leftover.
    let new_locks: Vec<&PathBuf> = after.locks.difference(&before.locks).collect();
    if !new_locks.is_empty() {
        problems.push(format!("lock files left behind: {new_locks:?}"));
    }

    assert!(
        problems.is_empty(),
        "the repository was modified:\n  {}",
        problems.join("\n  ")
    );
}

/// The scratch indexes `Git::dirty` copies live in the shared temp directory
/// and are named for this process, so a leftover is detectable — but only in a
/// window where no sibling test in this binary is holding one. [`scratch_guard`]
/// creates that window.
fn assert_no_scratch_indexes() {
    let prefix = format!("standup-index-{}-", std::process::id());
    let mut leftovers = Vec::new();
    if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                leftovers.push(entry.path());
            }
        }
    }
    assert!(
        leftovers.is_empty(),
        "scratch index copies were left behind: {leftovers:?}"
    );
}

/// Serialises the tests in this file. They all report on checkouts, so they
/// each create scratch indexes under the shared temp directory and cannot judge
/// each other's leftovers.
fn scratch_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// The pipeline under test
// ---------------------------------------------------------------------------

/// A repository with every checkout shape the collector has to handle, plus
/// untracked and ignored files that must survive untouched.
fn kitchen_sink(tag: &str) -> (Fixture, Vec<PathBuf>) {
    let fixture = Fixture::new(tag);
    fixture.fake_origin();
    fixture.commits_around_the_window();
    fixture.awkward_paths_commit(T_IN2);
    fixture.tricky_subject_commit(T_IN2);

    // A merge, so the log parser walks the no-numstat path.
    fixture.merge_commit(T_IN2);

    // A linked worktree with an upstream, ahead of it.
    let published = fixture.worktree("published", "published");
    fixture.write(&published, "published.txt", "work\n");
    fixture.commit_all_at(&published, T_IN1, "published work");
    fixture.publish("published", "main");

    // A branch whose upstream ref has been deleted underneath it.
    let orphaned = fixture.worktree("orphaned", "orphaned");
    fixture.write(&orphaned, "orphaned.txt", "work\n");
    fixture.commit_all_at(&orphaned, T_IN1, "orphaned work");
    fixture.publish("orphaned", "main");
    fixture.unpublish("orphaned");

    // A merge left in progress: an unmerged index and conflict markers on disk.
    let mid_merge = fixture.merge_in_progress_worktree("mid-merge");
    let detached = fixture.detached_worktree("detached");
    let unborn = fixture.unborn_worktree("unborn", "unborn");
    let deleted = fixture.deleted_branch_worktree("deleted", "doomed");

    // Squash-merged: the only shape that runs the patch-id probes to
    // completion. `git cherry` finds nothing, so the branch's combined diff is
    // taken with `diff-tree -p` and compared against every commit on the trunk
    // with `log -p`, and both are piped through `patch-id`. None of the three
    // may touch the index — `diff` refreshing it is what this file exists for.
    let squashed = fixture.squash_merged_worktree("squashed", "squashed");

    // Uncommitted work — staged, unstaged, renamed, untracked and ignored — in
    // the places most likely to be written back somewhere they should not be.
    fixture.dirty_up(&fixture.repo);
    fixture.dirty_up(&published);

    // Without this the whole fixture is a checkout git has nothing to write
    // back to, and every assertion below is satisfied by doing nothing.
    fixture.make_stat_cache_stale(&fixture.repo);
    fixture.make_stat_cache_stale(&published);

    let worktrees = vec![
        fixture.repo.clone(),
        published,
        orphaned,
        mid_merge,
        detached,
        unborn,
        deleted,
        squashed,
    ];
    (fixture, worktrees)
}

/// Runs everything the plugin ever runs against a repository.
fn run_full_pipeline(git: &Git, fixture: &Fixture, worktrees: &[PathBuf]) -> Vec<String> {
    let mut problems = Vec::new();
    let window = window();

    for path in worktrees {
        let Some(id) = git.identify(path).expect("git ran") else {
            problems.push(format!("{} did not identify as a checkout", path.display()));
            continue;
        };
        // Sibling expansion: every worktree lists every other one.
        for sibling in git.worktrees(&id).expect("worktree list") {
            if let Some(sibling) = git.identify(&sibling).expect("git ran") {
                problems.extend(git.report(&sibling, &window).problems);
            }
        }
        problems.extend(git.report(&id, &window).problems);
    }

    // Window resolution runs inside a real checkout, which is the normal case.
    git.resolve_date(&fixture.repo, "midnight")
        .expect("resolve midnight");
    assert!(git.resolve_date(&fixture.repo, "bogusgarbage").is_err());

    problems
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn the_full_pipeline_changes_nothing_in_the_repository() {
    let _serialised = scratch_guard();
    let (fixture, worktrees) = kitchen_sink("read-only");
    let git = Git::new(TIMEOUT);

    let before = fingerprint(&fixture, &worktrees);
    assert!(
        before.loose.len() > 20,
        "fixture has almost no objects to protect: {}",
        before.loose.len()
    );
    assert!(
        before.index_bytes.len() >= 5,
        "fixture has too few indexes to be a real test: {}",
        before.index_bytes.len()
    );

    let problems = run_full_pipeline(&git, &fixture, &worktrees);
    // The only checkout that is *meant* to report a problem is the one whose
    // branch was deleted underneath it.
    assert!(
        problems.iter().all(|p| p.contains("deleted underneath")),
        "the pipeline reported unexpected problems: {problems:?}"
    );

    let after = fingerprint(&fixture, &worktrees);
    assert_unchanged(&before, &after);
    assert_no_scratch_indexes();
}

/// The writeback the whole module is arranged to avoid.
///
/// The subtlety this test got wrong once, and which let a real bug through: a
/// checkout whose stat cache is **fresh** gives git nothing to write back, so
/// the test passed even with `--no-optional-locks` deleted from `src/git.rs`
/// outright. The cache has to be made *stale* first, and staleness needs two
/// things — a tracked file rewritten with identical bytes, and enough elapsed
/// time on either side that git's one-second racy-clean window is not what is
/// being measured.
///
/// With that in place this catches both writers: `status` without
/// `--no-optional-locks`, and `diff`, whose index refresh is **not** optional
/// and which neither the flag nor `GIT_OPTIONAL_LOCKS=0` suppresses.
#[test]
fn reporting_a_dirty_checkout_does_not_rewrite_its_index() {
    let _serialised = scratch_guard();
    let fixture = Fixture::new("index-writeback");
    fixture.dirty_up(&fixture.repo);
    let worktrees = vec![fixture.repo.clone()];
    let git = Git::new(TIMEOUT);

    fixture.make_stat_cache_stale(&fixture.repo);

    let before = fingerprint(&fixture, &worktrees);
    let id = git
        .identify(&fixture.repo)
        .expect("git ran")
        .expect("checkout");
    for _ in 0..3 {
        let report = git.report(&id, &window());
        assert!(report.problems.is_empty(), "{:?}", report.problems);
    }
    let after = fingerprint(&fixture, &worktrees);
    assert_unchanged(&before, &after);
    assert_no_scratch_indexes();
}

/// A worktree stopped mid-merge is the state most likely to be "helpfully"
/// repaired by a tool that stages or writes trees. standup only reads it.
#[test]
fn a_worktree_mid_merge_is_reported_without_being_touched() {
    let _serialised = scratch_guard();
    let fixture = Fixture::new("mid-merge-readonly");
    let mid = fixture.merge_in_progress_worktree("mid-merge");
    let worktrees = vec![fixture.repo.clone(), mid.clone()];
    let git = Git::new(TIMEOUT);

    let status = fixture.git(&mid, &["status", "--porcelain"]);
    assert!(status.contains("UU conflict.txt"), "{status}");

    let before = fingerprint(&fixture, &worktrees);
    let id = git.identify(&mid).expect("git ran").expect("checkout");
    let report = git.report(&id, &window());
    assert_eq!(report.dirty.conflicted, 1);
    let after = fingerprint(&fixture, &worktrees);
    assert_unchanged(&before, &after);

    let body = std::fs::read_to_string(mid.join("conflict.txt")).expect("read");
    assert!(body.contains("<<<<<<<"), "conflict markers were rewritten");
}

/// Concurrency is the other half of the promise: an agent running `git add` in
/// the same checkout holds `index.lock`, and nothing standup does may need that
/// lock or disturb its holder.
#[test]
fn a_run_is_safe_while_another_process_holds_the_index_lock() {
    let _serialised = scratch_guard();
    let (fixture, worktrees) = kitchen_sink("locked");
    let git = Git::new(TIMEOUT);

    let mut holders = LockHolders::default();
    let mut lock_paths = Vec::new();
    for worktree in [&fixture.repo, &worktrees[1]] {
        let lock = fixture.git_dir(worktree).join("index.lock");
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                ": > '{}'; exec sleep 120",
                lock.to_string_lossy().replace('\'', "'\\''")
            ))
            // Never inherit the harness's pipes: a leaked holder would keep
            // stdout open and hang `cargo test` long after the test finished.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn lock holder");
        holders.0.push(child);
        lock_paths.push(lock);
    }
    for _ in 0..200 {
        if lock_paths.iter().all(|p| p.exists()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for lock in &lock_paths {
        assert!(lock.exists(), "lock holder never took {}", lock.display());
    }

    let before = fingerprint(&fixture, &worktrees);
    let problems: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| run_full_pipeline(&git, &fixture, &worktrees)))
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("a pipeline thread panicked"))
            .collect()
    });
    assert!(
        problems.iter().all(|p| p.contains("deleted underneath")),
        "concurrent pipelines reported unexpected problems: {problems:?}"
    );

    let after = fingerprint(&fixture, &worktrees);
    assert_unchanged(&before, &after);
    assert_no_scratch_indexes();

    for lock in &lock_paths {
        assert!(
            lock.exists(),
            "standup removed a lock it did not take: {}",
            lock.display()
        );
    }
    drop(holders);
    for lock in &lock_paths {
        let _ = std::fs::remove_file(lock);
    }
}

/// The negative control, so the test above cannot quietly go vacuous.
///
/// If the fingerprint were too loose it would pass no matter what standup did.
/// A plain `git status` — the same command without `--no-optional-locks` — takes
/// `index.lock` and rewrites the index to save its stat cache, and this asserts
/// that the fingerprint notices. Verified on git 2.53.0: the index mtime moves
/// while its bytes stay the same length, which is precisely why the mtime is
/// fingerprinted separately from the contents.
#[test]
fn the_fingerprint_catches_the_writeback_that_no_optional_locks_prevents() {
    let _serialised = scratch_guard();
    let fixture = Fixture::new("negative-control");
    for name in ["one.txt", "two.txt", "three.txt"] {
        fixture.write(&fixture.repo, name, "content\n");
    }
    fixture.commit_all_at(&fixture.repo, T_IN1, "files with a stat cache");
    let worktrees = vec![fixture.repo.clone()];

    // Rewrite the files with identical bytes: the content is unchanged, only
    // the stat data git cached is now stale.
    std::thread::sleep(Duration::from_millis(1_100));
    for name in ["one.txt", "two.txt", "three.txt"] {
        fixture.write(&fixture.repo, name, "content\n");
    }

    let before = fingerprint(&fixture, &worktrees);
    fixture.git(&fixture.repo, &["status", "--porcelain"]);
    let after = fingerprint(&fixture, &worktrees);

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_unchanged(&before, &after)
    }));
    std::panic::set_hook(previous);

    assert!(
        caught.is_err(),
        "the fingerprint did not notice git rewriting the index, so the read-only test above \
         proves nothing"
    );
}

/// The other half of the promise, and the one the fixtures above cannot make: a
/// repository standup does not write to may still be one git *fills in*.
///
/// A blobless clone has the commits and the trees and almost none of the blobs.
/// A `--numstat` needs them, and git's default answer is to fetch them from the
/// promisor remote and write them into the object store — measured on git 2.53.0
/// as 8 object files becoming 24, from a plugin that promises neither a write nor
/// a network call. `GIT_NO_LAZY_FETCH=1` refuses, and this pins all three halves
/// of that: nothing is written, the commits survive without their line counts,
/// and the object git could not read is named.
#[test]
fn reporting_a_partial_clone_fetches_nothing_and_names_what_it_could_not_read() {
    let _serialised = scratch_guard();
    let fixture = Fixture::new("promisor");
    // A file rewritten twice: the superseded versions of it are what the clone
    // does not have.
    fixture.commits_around_the_window();
    // And a squash-merged branch, so the landing probes go past
    // `merge-base --is-ancestor` to their `log -p` over the trunk range.
    fixture.squash_merged_worktree("squashed", "squashed");
    // The trunk then moves on over a file it already had, **twice**, which is
    // what leaves a superseded blob inside that range and in neither tip tree:
    // the clone fetches the blobs of the two commits it checks out, and the
    // middle version belongs to neither. Without the second rewrite the probes
    // read only trees the clone already has, and answer.
    fixture.write(&fixture.repo, "inside.txt", "one\nTWO\nthree\nfour\n");
    fixture.commit_all_at(&fixture.repo, T_IN3, "the trunk moves on");
    fixture.write(&fixture.repo, "inside.txt", "one\nTWO\nthree\nfour\nfive\n");
    fixture.commit_all_at(&fixture.repo, T_IN3 + 60, "and moves on again");
    let clone = fixture.promisor_clone("blobless", "squashed");
    let worktrees = vec![clone.clone()];

    let git = Git::new(TIMEOUT);
    let before = fingerprint_of(&fixture, &clone, &worktrees);
    let id = git
        .identify(&clone)
        .expect("git ran")
        .expect("the clone is a checkout");
    let report = git.report(&id, &window());
    let after = fingerprint_of(&fixture, &clone, &worktrees);
    assert_unchanged(&before, &after);

    // The commits are the part that needs no blobs, and losing them would turn a
    // missing line count into a day that reads as empty.
    assert!(
        !report.commits.is_empty(),
        "a partial clone still has its commits: {report:?}"
    );
    assert!(
        report.churn.is_zero(),
        "the line counts cannot be read without the blobs: {:?}",
        report.churn
    );

    // Which sentence a reader gets depends on the git in front of them, and both
    // are honest. A git that honours `GIT_NO_LAZY_FETCH` asks for the diff, is
    // refused, and names the object it could not read; one that predates the
    // variable (2.37) is not asked for the diff at all, and names the remote it
    // declined to reach for and the reason why.
    let refuses = fixture.git_version() >= (2, 37);
    let named = report
        .problems
        .iter()
        .find(|problem| problem.contains("partial clone"))
        .unwrap_or_else(|| {
            panic!(
                "a partial clone must say what it could not read: {:?}",
                report.problems
            )
        });
    if refuses {
        assert!(
            names_an_object(named),
            "the problem has to name the object, not just the category: {named}"
        );
    } else {
        assert!(
            named.contains("GIT_NO_LAZY_FETCH") && named.contains("partial clone of origin"),
            "an old git has to say which remote it refused to reach for, and why: {named}"
        );
    }

    // And the merge status, which the same missing blobs make unanswerable.
    // "I could not find out" is not "this did not land".
    match &report.landed {
        Landed::Unknown { reason } if refuses => assert!(
            names_an_object(reason),
            "the unknown verdict has to say which object it could not read: {reason}"
        ),
        Landed::Unknown { reason } => assert!(
            reason.contains("GIT_NO_LAZY_FETCH"),
            "the unknown verdict has to say why the probes did not run: {reason}"
        ),
        other => panic!("a probe that could not read the trunk is not an answer: {other:?}"),
    }
}

/// The negative control for the test above: proof that the fixture has something
/// to fetch and that the fingerprint sees it arrive.
///
/// Without this, a clone that happened to be complete — a filter the server
/// refused, a git that fetched everything anyway — would satisfy every assertion
/// above by doing nothing. The same log the collector runs, without
/// `GIT_NO_LAZY_FETCH`, writes a pack into the user's repository.
#[test]
fn the_fingerprint_catches_the_promisor_fetch_that_no_lazy_fetch_refuses() {
    let _serialised = scratch_guard();
    let fixture = Fixture::new("promisor-control");
    fixture.commits_around_the_window();
    fixture.squash_merged_worktree("squashed", "squashed");
    let clone = fixture.promisor_clone("blobless", "squashed");
    let worktrees = vec![clone.clone()];

    let before = fingerprint_of(&fixture, &clone, &worktrees);
    // The fixture's own git runs without GIT_NO_LAZY_FETCH, which is exactly
    // what standup used to do.
    fixture.git(&clone, &["log", "--numstat", "--format=%H"]);
    let after = fingerprint_of(&fixture, &clone, &worktrees);

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_unchanged(&before, &after)
    }));
    std::panic::set_hook(previous);

    assert!(
        caught.is_err(),
        "nothing was fetched into the clone, so the fixture is complete and the test above \
         proves nothing"
    );
}

/// Whether a problem string carries a full object id, which is what makes it
/// actionable: a reader can run `git cat-file -p <oid>` and see for themselves.
fn names_an_object(problem: &str) -> bool {
    problem
        .split(|c: char| !c.is_ascii_hexdigit())
        .any(|word| word.len() == 40 || word.len() == 64)
}

/// Reaps the lock-holding processes even when an assertion above panics, so a
/// failing test never leaves `sleep` children behind.
#[derive(Default)]
struct LockHolders(Vec<std::process::Child>);

impl Drop for LockHolders {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
