//! What the collector reports, against real repositories.
//!
//! Every assertion here is a number or a named variant, never a shape: a test
//! that only checks "some commits came back" would pass just as happily on a
//! parser that lost the merge, double-counted the churn, or dated everything by
//! author time.

#[path = "fixtures.rs"]
mod fixtures;

use std::path::{Path, PathBuf};
use std::time::Duration;

use standup::config::Ignored;
use standup::git::Git;
use standup::model::{Activity, Equivalence, Head, Landed, Tracking, Unpushed};

use fixtures::{
    window, window_between, Fixture, T_AFTER, T_IN1, T_IN2, T_IN3, T_OLD, T_OLDER, T_SINCE, T_UNTIL,
};

const TIMEOUT: Duration = Duration::from_secs(60);

fn git() -> Git {
    Git::new(TIMEOUT)
}

/// `identify` on a path that must be a checkout.
fn id(git: &Git, path: &Path) -> standup::git::CheckoutId {
    git.identify(path)
        .expect("git ran")
        .unwrap_or_else(|| panic!("{} is not a checkout", path.display()))
}

fn subjects(report: &standup::model::CheckoutReport) -> Vec<&str> {
    report.commits.iter().map(|c| c.subject.as_str()).collect()
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

#[test]
fn identify_resolves_the_toplevel_the_repo_key_and_the_main_checkout() {
    let fixture = Fixture::new("identify");
    let git = git();

    let main = id(&git, &fixture.repo);
    assert_eq!(main.path, std::fs::canonicalize(&fixture.repo).unwrap());
    assert_eq!(
        main.repo_key.as_str(),
        std::fs::canonicalize(fixture.common_dir(&fixture.repo))
            .unwrap()
            .to_string_lossy()
    );
    assert_eq!(main.repo_root, main.path);
    assert!(!main.is_linked_worktree);
}

/// The property the whole per-repository grouping rests on.
#[test]
fn every_worktree_of_one_repository_shares_its_repo_key() {
    let fixture = Fixture::new("repo-key");
    let git = git();
    let linked = fixture.worktree("linked", "linked");

    let main = id(&git, &fixture.repo);
    let side = id(&git, &linked);

    assert_eq!(main.repo_key, side.repo_key);
    assert_eq!(side.repo_root, main.path);
    assert!(side.is_linked_worktree);
    assert!(!main.is_linked_worktree);
}

/// A subdirectory of a checkout identifies as the checkout, not as itself.
#[test]
fn a_subdirectory_identifies_as_its_checkout() {
    let fixture = Fixture::new("subdir");
    let git = git();
    fixture.write(&fixture.repo, "sub/deep/file.txt", "x\n");

    let deep = id(&git, &fixture.repo.join("sub/deep"));
    assert_eq!(deep.path, std::fs::canonicalize(&fixture.repo).unwrap());
}

#[test]
fn a_plain_directory_is_ordinary_data_not_a_failure() {
    let fixture = Fixture::new("not-a-repo");
    let plain = fixture.root().join("home");
    let git = git();

    // Only meaningful where the temp directory is not itself inside a
    // repository. A home directory kept in git, with `TMPDIR` pointed inside
    // it, makes every directory under it part of a checkout — correctly, and
    // git agrees — so there is nothing to assert. Said out loud rather than
    // asserted falsely.
    let (enclosed, _, _) = fixture.try_git(&plain, &["rev-parse", "--git-dir"]);
    if enclosed == 0 {
        eprintln!(
            "note: {} is inside a git repository, so the plain-directory half of this test \
             cannot run here",
            plain.display()
        );
    } else {
        assert_eq!(git.identify(&plain).expect("git ran"), None);
    }

    // A path that does not exist at all is the same answer, everywhere.
    assert_eq!(
        git.identify(&fixture.root().join("nowhere"))
            .expect("git ran"),
        None
    );
}

#[test]
fn worktrees_lists_every_checkout_of_the_repository() {
    let fixture = Fixture::new("worktrees");
    let git = git();
    let linked = fixture.worktree("linked", "linked");
    let detached = fixture.detached_worktree("detached");

    let mut found = git.worktrees(&id(&git, &fixture.repo)).expect("list");
    found.sort();
    let mut want: Vec<PathBuf> = [&fixture.repo, &linked, &detached]
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap())
        .collect();
    want.sort();
    assert_eq!(found, want);

    // Every worktree sees the same list, whichever one is asked.
    let from_linked = git.worktrees(&id(&git, &linked)).expect("list");
    let mut from_linked = from_linked;
    from_linked.sort();
    assert_eq!(from_linked, want);
}

// ---------------------------------------------------------------------------
// resolve_date — the loud-failure contract
// ---------------------------------------------------------------------------

#[test]
fn resolve_date_rejects_a_window_git_could_not_parse() {
    let fixture = Fixture::new("dates");
    let git = git();

    // The trap: git exits 0 and answers *now* for all of these.
    //
    // Not every misspelling lands here — approxidate is a scavenger, and
    // `last tuesdayish` really does resolve to last Tuesday while `2026-13-45`
    // resolves to a nearby day. Those are answers git stands behind, and this
    // check is only about the ones it silently gives up on.
    for garbage in [
        "bogusgarbage",
        "",
        "   ",
        "not a date at all",
        "zzz",
        "42 fortnights ago",
    ] {
        let err = git
            .resolve_date(&fixture.repo, garbage)
            .expect_err(&format!("{garbage:?} was accepted as a window"));
        let message = err.to_string();
        assert!(
            message.contains("empty") || message.contains("could not parse"),
            "unhelpful rejection of {garbage:?}: {message}"
        );
    }
}

#[test]
fn resolve_date_accepts_what_git_accepts() {
    let fixture = Fixture::new("dates-ok");
    let git = git();
    let now = standup::clock::now();

    let midnight = git
        .resolve_date(&fixture.repo, "midnight")
        .expect("midnight");
    assert!(midnight <= now && now - midnight < 86_400 + 3_600);

    let yesterday = git
        .resolve_date(&fixture.repo, "yesterday")
        .expect("yesterday");
    assert!(yesterday < midnight || (now - yesterday) >= 86_000);

    let absolute = git
        .resolve_date(&fixture.repo, "2026-08-01 00:00:00 +0000")
        .expect("an absolute date");
    assert_eq!(absolute, T_SINCE);

    // `now` genuinely means now, so it must survive the check that rejects
    // everything else landing on now. `today` is on the same allowlist, and
    // which instant it means depends on the git in front of you: through 2.54 it
    // resolved to **now**, and 2.55 changed it to the local **midnight**.
    // Measured on 2.53.0 and 2.54.0 (now) and on a 2.55.0 runner, where it came
    // back 49137 seconds earlier than now — midnight to the second.
    //
    // Both readings are fine here, which is why this asserts the property rather
    // than the instant. The allowlist exists so that a spec landing on the
    // current instant is not mistaken for git's silent "I could not parse that"
    // answer; a `today` that means midnight simply never reaches that check.
    // What has to hold on every git is that none of these is rejected, and that
    // each lands inside the current local day.
    for legitimate in ["now", "today", "NOW"] {
        let resolved = git
            .resolve_date(&fixture.repo, legitimate)
            .unwrap_or_else(|err| panic!("{legitimate:?} was rejected: {err}"));
        let now_again = standup::clock::now();
        assert!(
            (resolved - now_again).abs() <= 5 || resolved == midnight,
            "{legitimate:?} resolved to {resolved}, which is neither now \
             ({now_again}) nor today's midnight ({midnight})"
        );
    }
}

#[test]
fn the_date_reference_repository_is_created_once_and_reused() {
    let fixture = Fixture::new("date-ref");
    let git = git();
    let dateref = fixture.root().join("state/dateref.git");

    git.ensure_date_ref_repo(&dateref).expect("create");
    assert!(dateref.join("HEAD").is_file());
    let before = std::fs::read(dateref.join("HEAD")).unwrap();

    // Idempotent.
    git.ensure_date_ref_repo(&dateref).expect("reuse");
    assert_eq!(std::fs::read(dateref.join("HEAD")).unwrap(), before);

    // It is a usable parsing context even though it has no commits.
    assert!(git.resolve_date(&dateref, "midnight").is_ok());
    assert!(git.resolve_date(&dateref, "bogusgarbage").is_err());
}

/// A home directory kept in git — ordinary dotfiles — puts the plugin's own
/// state directory inside a repository. That must work, and it must not be
/// mistaken for the date-reference repository already existing.
///
/// The earlier version of this test asserted the opposite, and the guard it
/// pinned made `standup` fail outright on exactly that setup whenever no
/// checkout was available to anchor date parsing.
#[test]
fn a_date_reference_repository_inside_another_repository_works_and_disturbs_nothing() {
    let fixture = Fixture::new("date-ref-nested");
    let git = git();
    let inside = fixture.repo.join("state/dateref.git");

    let refs_before = fixture.git(&fixture.repo, &["for-each-ref"]);
    let index_before = std::fs::read(fixture.git_dir(&fixture.repo).join("index")).expect("index");

    git.ensure_date_ref_repo(&inside)
        .expect("a state directory inside a checkout is where dotfiles users keep theirs");

    // It really is its own repository, not the enclosing one seen from inside.
    assert!(inside.join("HEAD").is_file());
    assert_eq!(
        std::fs::canonicalize(fixture.git(
            &inside,
            &["rev-parse", "--path-format=absolute", "--git-dir"]
        ))
        .unwrap(),
        std::fs::canonicalize(&inside).unwrap()
    );
    assert!(git.resolve_date(&inside, "midnight").is_ok());

    // And the enclosing checkout is untouched: no staged file, no moved ref, no
    // rewritten index.
    assert_eq!(fixture.git(&fixture.repo, &["for-each-ref"]), refs_before);
    assert_eq!(
        std::fs::read(fixture.git_dir(&fixture.repo).join("index")).expect("index"),
        index_before
    );

    // Idempotent here too.
    git.ensure_date_ref_repo(&inside).expect("reuse");
}

// ---------------------------------------------------------------------------
// Commits and churn
// ---------------------------------------------------------------------------

#[test]
fn only_commits_inside_the_window_are_reported_and_churn_is_the_union() {
    let fixture = Fixture::new("window");
    fixture.commits_around_the_window();
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(
        subjects(&report),
        vec!["inside: edit the same file again", "inside: two lines"],
        "the window must exclude `base` and `outside the window`"
    );
    // Newest first, and dated by commit time.
    assert_eq!(report.commits[0].committed.epoch, T_IN2);
    assert_eq!(report.commits[1].committed.epoch, T_IN1);

    // inside.txt: +2 then +2/-1. One file, touched twice.
    assert_eq!(report.churn.files, 1);
    assert_eq!(report.churn.insertions, 4);
    assert_eq!(report.churn.deletions, 1);
    assert_eq!(report.activity(), Activity::Active);
}

#[test]
fn an_until_bound_trims_the_top_of_the_window() {
    let fixture = Fixture::new("until");
    fixture.write(&fixture.repo, "a.txt", "a\n");
    fixture.commit_all_at(&fixture.repo, T_IN1, "inside");
    fixture.write(&fixture.repo, "b.txt", "b\n");
    fixture.commit_all_at(&fixture.repo, T_AFTER, "after the until");
    let git = git();

    let report = git.report(
        &id(&git, &fixture.repo),
        &window_between(T_SINCE, Some(T_UNTIL)),
    );
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(subjects(&report), vec!["inside"]);
}

#[test]
fn a_repository_whose_work_is_all_older_than_the_window_is_quiet_not_broken() {
    let fixture = Fixture::new("quiet");
    fixture.write(&fixture.repo, "old.txt", "old\n");
    fixture.commit_all_at(&fixture.repo, T_OLD + 60, "long before the window");
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert!(report.commits.is_empty());
    assert_eq!(report.churn.files, 0);
    assert!(report.churn.is_zero());
    assert!(report.dirty.is_clean());
    assert_eq!(report.activity(), Activity::Quiet);
}

// ---------------------------------------------------------------------------
// Generated and vendored paths
// ---------------------------------------------------------------------------

/// Lines are a proxy for effort, and a regenerated lockfile destroys it. The
/// files are still counted — the commit really did touch them, which is the same
/// treatment a binary file has always had — and they contribute no lines.
#[test]
fn generated_and_vendored_paths_are_counted_as_files_and_not_as_lines() {
    let fixture = Fixture::new("generated");
    let (real, generated) = fixture.generated_and_vendored_commit(T_IN1);
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(
        report.churn.files, 3,
        "every touched file is still a touched file"
    );
    assert_eq!(
        report.churn.excluded, 2,
        "the lockfile and the vendored tree"
    );
    assert_eq!(
        (report.churn.insertions, report.churn.deletions),
        (real, 0),
        "only the hand-written lines; the {generated} generated ones are noise"
    );
}

/// The list is configurable, and an empty one turns the exclusion off. Somebody
/// who wants the raw diff numbers is entitled to ask for them.
#[test]
fn an_empty_ignore_list_counts_every_line() {
    let fixture = Fixture::new("generated-off");
    let (real, generated) = fixture.generated_and_vendored_commit(T_IN1);
    let git = Git::new(TIMEOUT).ignoring(Ignored::new(Vec::new()));

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert_eq!(report.churn.excluded, 0);
    assert_eq!(
        (report.churn.insertions, report.churn.deletions),
        (real + generated, 0)
    );
}

/// A pattern a reader added themselves is honoured, and one they did not is not.
#[test]
fn a_configured_pattern_replaces_the_defaults_rather_than_adding_to_them() {
    let fixture = Fixture::new("generated-custom");
    fixture.generated_and_vendored_commit(T_IN1);
    // Only the vendored tree named, so the lockfile's lines come back.
    let git = Git::new(TIMEOUT).ignoring(Ignored::new(vec!["node_modules/".to_string()]));

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert_eq!(report.churn.excluded, 1, "the vendored tree only");
    assert_eq!(
        (report.churn.insertions, report.churn.deletions),
        (412, 0),
        "12 hand-written lines plus the 400-line lockfile"
    );
}

// ---------------------------------------------------------------------------
// Unpushed
// ---------------------------------------------------------------------------

/// The third state, and the stake behind it.
///
/// A checkout holding commits that are on no remote is not quiet: removing the
/// directory takes them with it. The second half of this test is the reason the
/// state exists at all — it removes the checkout the way closing a workspace
/// does, and shows that what the digest named is then reachable from nothing.
#[test]
fn unpushed_work_is_named_and_a_removed_checkout_takes_it_with_it() {
    let fixture = Fixture::new("unpushed-removal");
    fixture.fake_origin();
    let path = fixture.unmerged_worktree("flying", "flying");
    let oid = fixture.head_oid(&path);
    let git = git();

    let report = git.report(&id(&git, &path), &window());
    assert_eq!(
        report.unpushed,
        Unpushed::Commits { count: 1 },
        "{:?}",
        report.problems
    );

    // What closing a finished workspace does: the worktree goes, and the branch
    // that only it was using goes with it.
    fixture.git(
        &fixture.repo,
        &["worktree", "remove", "--force", path.to_str().unwrap()],
    );
    fixture.git(&fixture.repo, &["branch", "-D", "flying"]);

    let containing = fixture.git(
        &fixture.repo,
        &[
            "for-each-ref",
            &format!("--contains={oid}"),
            "--format=%(refname)",
        ],
    );
    assert!(
        containing.is_empty(),
        "the commit the digest warned about is still reachable from {containing}"
    );
}

/// Work that is on a remote is not at risk, even when it is not on the trunk and
/// not on this branch's own upstream. The question is every remote, not one.
#[test]
fn work_that_reached_a_remote_is_not_counted_as_unpushed() {
    let fixture = Fixture::new("unpushed-pushed");
    fixture.fake_origin();
    let path = fixture.unmerged_worktree("flying", "flying");
    // Published under a different name than the branch, which is what a fork or
    // a second remote looks like from here: the sha is on a remote either way.
    fixture.publish("elsewhere", "flying");
    let git = git();

    let report = git.report(&id(&git, &path), &window());
    assert_eq!(
        report.unpushed,
        Unpushed::Commits { count: 0 },
        "{:?}",
        report.problems
    );
}

/// A repository with no remote has nowhere to have pushed to, so it is not
/// holding unpushed work — it is holding all of its work, which is a different
/// sentence and not this one. Counting it would file every local-only scratch
/// repository under "at risk" and bury the case worth reading.
#[test]
fn a_repository_with_no_remote_is_not_holding_unpushed_work() {
    let fixture = Fixture::new("unpushed-no-remote");
    fixture.write(&fixture.repo, "local.txt", "local\n");
    fixture.commit_all_at(&fixture.repo, T_OLD + 60, "never going anywhere");
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert_eq!(report.unpushed, Unpushed::NoRemote, "{:?}", report.problems);
    assert_eq!(report.activity(), Activity::Quiet);
}

/// An unborn branch has committed nothing, so it is holding nothing — and
/// `rev-list` would fail on the HEAD it does not have.
#[test]
fn an_unborn_branch_has_nothing_unpushed_and_no_problem_about_it() {
    let fixture = Fixture::new("unpushed-unborn");
    fixture.fake_origin();
    let path = fixture.unborn_worktree("fresh", "fresh");
    let git = git();

    let report = git.report(&id(&git, &path), &window());
    assert_eq!(report.unpushed, Unpushed::Commits { count: 0 });
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

/// Unpushed work from before the window still counts. The window measures what
/// came out of a day; this measures what would be lost with the directory, which
/// has no window — and being filed under "quiet" is exactly how it stayed
/// invisible.
#[test]
fn unpushed_work_older_than_the_window_is_not_quiet() {
    let fixture = Fixture::new("unpushed-older");
    fixture.fake_origin();
    let path = fixture.worktree("stale", "stale");
    fixture.write(&path, "stale.txt", "committed long ago, never pushed\n");
    fixture.commit_all_at(&path, T_OLD + 60, "long before the window");
    let git = git();

    let report = git.report(&id(&git, &path), &window());
    assert!(report.commits.is_empty(), "nothing in the window");
    assert!(report.dirty.is_clean(), "nothing uncommitted");
    assert_eq!(report.unpushed, Unpushed::Commits { count: 1 });
    assert_eq!(
        report.activity(),
        Activity::Unpushed,
        "a checkout that would lose work is not a quiet one"
    );
}

#[test]
fn a_merge_is_a_commit_that_contributes_no_churn() {
    let fixture = Fixture::new("merge");
    fixture.merge_commit(T_IN2);
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert!(report.problems.is_empty(), "{:?}", report.problems);

    let merge = report
        .commits
        .iter()
        .find(|c| c.subject == "merge side into main")
        .expect("the merge is in the digest");
    assert!(merge.is_merge);
    assert_eq!((merge.insertions, merge.deletions), (0, 0));
    assert!(merge.files.is_empty());

    // The two ordinary commits are still counted, and only they contribute.
    assert_eq!(report.commits.len(), 3);
    assert_eq!(report.commits.iter().filter(|c| c.is_merge).count(), 1);
    assert_eq!(report.churn.files, 2);
    assert_eq!(report.churn.insertions, 2);
    assert_eq!(report.churn.deletions, 0);
}

#[test]
fn a_binary_file_counts_as_a_file_and_an_awkward_path_survives_intact() {
    let fixture = Fixture::new("awkward");
    let awkward = fixture.awkward_paths_commit(T_IN1);
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.commits.len(), 1);

    let commit = &report.commits[0];
    assert_eq!(commit.files.len(), 2, "{:?}", commit.files);
    assert!(commit.files.iter().any(|f| f == "blob.bin"));

    // The awkward path keeps its space and its newline; a byte that is not
    // valid UTF-8, where the filesystem let one be written at all, becomes a
    // replacement character, which is the only thing a `String` can do with it.
    let lossy = String::from_utf8_lossy(&awkward).into_owned();
    assert!(
        commit.files.contains(&lossy),
        "awkward path mangled: {:?} does not contain {lossy:?}",
        commit.files
    );
    assert!(lossy.contains(' ') && lossy.contains('\n'));
    if awkward.contains(&0xff) {
        assert!(lossy.contains('\u{fffd}'), "{lossy:?}");
    }

    // The binary file adds a file and no lines: only the awkward file's single
    // line is counted.
    assert_eq!(commit.insertions, 1);
    assert_eq!(commit.deletions, 0);
    assert_eq!(report.churn.files, 2);
    assert_eq!(report.churn.insertions, 1);
}

#[test]
fn a_subject_full_of_separators_does_not_desynchronise_the_log_parser() {
    let fixture = Fixture::new("subject");
    let subject = fixture.tricky_subject_commit(T_IN1);
    fixture.write(&fixture.repo, "after.txt", "after\n");
    fixture.commit_all_at(&fixture.repo, T_IN2, "an ordinary commit after it");
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(
        subjects(&report),
        vec!["an ordinary commit after it", subject.as_str()]
    );
    assert_eq!(report.commits[1].files, vec!["tricky.txt".to_string()]);
    assert_eq!(report.churn.files, 2);
}

/// The experiment the brief asked for, and its answer.
///
/// `--max-age` is a **traversal cutoff**: git stops walking at the first commit
/// older than the window, so an out-of-order committer timestamp hides every
/// in-window commit behind it. Measured on git 2.53.0 against
/// [`Fixture::skewed_history`]: `--max-age` reports **one** of the two
/// in-window commits.
///
/// `--since-as-filter` (git 2.37+) applies the same comparison as a filter
/// without pruning, and reports both. The collector uses it, and this test pins
/// both halves — the collector's answer, and the fact that git still exhibits
/// the disagreement — so the regression test cannot quietly go vacuous.
#[test]
fn out_of_order_committer_timestamps_do_not_hide_in_window_commits() {
    let fixture = Fixture::new("skew");
    fixture.skewed_history();
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(
        subjects(&report),
        vec!["skew: in window, newest", "skew: in window, oldest"],
        "a backdated commit between them must not hide either one"
    );

    // The trap is still real: the same window expressed as the pruning
    // `--max-age` loses the older of the two.
    let pruned = fixture.git(
        &fixture.repo,
        &["log", &format!("--max-age={T_SINCE}"), "--format=%s"],
    );
    let pruned: Vec<&str> = pruned.lines().collect();
    assert_eq!(
        pruned,
        vec!["skew: in window, newest"],
        "git no longer prunes on --max-age; the fallback path and this comment need revisiting"
    );

    // And the filtering form is what the collector actually relies on.
    let filtered = fixture.git(
        &fixture.repo,
        &[
            "log",
            &format!("--since-as-filter=@{T_SINCE}"),
            "--format=%s",
        ],
    );
    assert_eq!(filtered.lines().count(), 2);
}

/// `--since` filters on **commit** date, so the digest must display commit date
/// too. A rebased commit displayed by its author date would be filed under a day
/// the window never covered.
#[test]
fn commits_are_dated_by_commit_time_not_author_time() {
    let fixture = Fixture::new("skew-author");
    fixture.write(&fixture.repo, "rebased.txt", "rebased\n");
    // Authored long before the window, committed inside it: exactly what a
    // rebase or a cherry-pick produces.
    fixture.commit_all_skewed(
        &fixture.repo,
        T_OLDER,
        T_IN1,
        "authored old, committed today",
    );
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert_eq!(
        report.commits.len(),
        1,
        "commit date is what --since filters on"
    );
    assert_eq!(report.commits[0].committed.epoch, T_IN1);
}

// ---------------------------------------------------------------------------
// HEAD
// ---------------------------------------------------------------------------

#[test]
fn a_branch_with_a_commit_is_named_with_its_object_id() {
    let fixture = Fixture::new("head-branch");
    let git = git();
    let report = git.report(&id(&git, &fixture.repo), &window());
    match &report.head {
        Head::Branch { name, oid } => {
            assert_eq!(name, "main");
            assert_eq!(*oid, fixture.head_oid(&fixture.repo));
            assert_eq!(oid.len(), 40);
        }
        other => panic!("expected a branch, got {other:?}"),
    }
}

#[test]
fn a_repository_with_no_commits_at_all_is_unborn() {
    let fixture = Fixture::empty("unborn");
    let git = git();
    let report = git.report(&id(&git, &fixture.repo), &window());

    assert_eq!(
        report.head,
        Head::Unborn {
            name: "main".to_string()
        }
    );
    assert!(
        report.problems.is_empty(),
        "an unborn branch is a normal state, not a failure: {:?}",
        report.problems
    );
    assert!(report.commits.is_empty());
    assert_eq!(report.tracking, Tracking::NotApplicable);
    assert!(matches!(report.landed, Landed::Unknown { .. }));
}

#[test]
fn an_unborn_linked_worktree_is_unborn_too() {
    let fixture = Fixture::new("unborn-wt");
    let path = fixture.unborn_worktree("fresh", "fresh");
    let git = git();
    let report = git.report(&id(&git, &path), &window());
    assert_eq!(
        report.head,
        Head::Unborn {
            name: "fresh".to_string()
        }
    );
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

/// The case that looks identical to unborn through `symbolic-ref` alone. The
/// discriminator is the worktree's own `logs/HEAD`.
#[test]
fn a_branch_deleted_underneath_a_checkout_is_named_as_deleted() {
    let fixture = Fixture::new("deleted-branch");
    let path = fixture.deleted_branch_worktree("orphaned", "doomed");
    let git = git();

    // The two states really are indistinguishable to symbolic-ref.
    assert_eq!(
        fixture.git(&path, &["symbolic-ref", "-q", "HEAD"]),
        "refs/heads/doomed"
    );
    assert!(fixture.git_dir(&path).join("logs/HEAD").exists());

    let report = git.report(&id(&git, &path), &window());
    assert_eq!(
        report.head,
        Head::BranchDeleted {
            name: "doomed".to_string()
        }
    );
    // The state is carried by `Head::BranchDeleted` alone. It is deliberately
    // *not* also pushed as a problem: the renderers print the head note loudly
    // on its own, and recording both printed the same warning twice in the live
    // output. `activity` is what keeps it sorted to the top regardless.
    assert!(
        !report
            .problems
            .iter()
            .any(|p| p.contains("deleted underneath")),
        "the deleted-branch state belongs to Head, not to problems: {:?}",
        report.problems
    );
    assert_eq!(report.activity(), Activity::Broken);
}

#[test]
fn a_detached_head_reports_its_commit() {
    let fixture = Fixture::new("detached");
    let path = fixture.detached_worktree("loose");
    let git = git();
    let report = git.report(&id(&git, &path), &window());

    match &report.head {
        Head::Detached { oid } => assert_eq!(*oid, fixture.head_oid(&fixture.repo)),
        other => panic!("expected a detached HEAD, got {other:?}"),
    }
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.tracking, Tracking::NotApplicable);
}

// ---------------------------------------------------------------------------
// Dirty
// ---------------------------------------------------------------------------

#[test]
fn uncommitted_work_is_counted_by_kind_and_by_lines() {
    let fixture = Fixture::new("dirty");
    fixture.dirty_up(&fixture.repo);
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    assert!(report.problems.is_empty(), "{:?}", report.problems);

    // A modified file, a staged addition, and a rename — the rename being the
    // `2` record whose *two* NUL fields a naive parser reads as two records.
    assert_eq!(report.dirty.tracked_changed, 3, "{:?}", report.dirty);
    // `dir with space/a file.txt` and `weird\nname.txt`. The two ignored files
    // are not in any of these counts.
    assert_eq!(report.dirty.untracked, 2, "{:?}", report.dirty);
    assert_eq!(report.dirty.conflicted, 0);
    assert!(!report.dirty.is_clean());

    // tracked.txt: +2/-1 unstaged. staged.txt: +1 staged. The rename moves no
    // lines.
    assert_eq!(report.dirty.insertions, 3, "{:?}", report.dirty);
    assert_eq!(report.dirty.deletions, 1, "{:?}", report.dirty);
}

#[test]
fn a_conflicted_worktree_counts_its_unmerged_paths() {
    let fixture = Fixture::new("conflict");
    let path = fixture.merge_in_progress_worktree("mid-merge");
    let git = git();

    let report = git.report(&id(&git, &path), &window());
    assert_eq!(report.dirty.conflicted, 1, "{:?}", report.dirty);
    assert!(!report.dirty.is_clean());
    // The conflict markers are still on disk, untouched.
    let body = std::fs::read_to_string(path.join("conflict.txt")).expect("read");
    assert!(body.contains("<<<<<<<"));
}

#[test]
fn a_clean_checkout_reports_nothing_uncommitted() {
    let fixture = Fixture::new("clean");
    let git = git();
    let report = git.report(&id(&git, &fixture.repo), &window());
    assert_eq!(report.dirty, standup::model::Dirty::default());
    assert!(report.dirty.is_clean());
}

// ---------------------------------------------------------------------------
// Tracking
// ---------------------------------------------------------------------------

#[test]
fn a_branch_with_no_upstream_says_so() {
    let fixture = Fixture::new("no-upstream");
    let git = git();
    let report = git.report(&id(&git, &fixture.repo), &window());
    assert_eq!(report.tracking, Tracking::NoUpstream);
}

#[test]
fn ahead_and_behind_are_counted_from_the_upstream() {
    let fixture = Fixture::new("ahead-behind");
    fixture.fake_origin();
    let path = fixture.worktree("published", "published");

    // Two commits the upstream has not seen.
    fixture.write(&path, "ahead-1.txt", "1\n");
    fixture.commit_all_at(&path, T_IN1, "ahead one");
    fixture.write(&path, "ahead-2.txt", "2\n");
    fixture.commit_all_at(&path, T_IN2, "ahead two");
    // The upstream is at the branch point, plus one commit of its own.
    fixture.publish("published", "main");
    fixture.write(&fixture.repo, "remote-move.txt", "moved\n");
    fixture.commit_all_at(&fixture.repo, T_IN3, "the remote moves on");
    fixture.publish("published", "main");

    let git = git();
    let report = git.report(&id(&git, &path), &window());
    assert_eq!(
        report.tracking,
        Tracking::Upstream {
            name: "origin/published".to_string(),
            ahead: 2,
            behind: 1,
        },
        "{:?}",
        report.problems
    );
}

#[test]
fn an_upstream_that_no_longer_resolves_is_missing_not_absent() {
    let fixture = Fixture::new("upstream-gone");
    fixture.fake_origin();
    let path = fixture.worktree("gone", "gone");
    fixture.write(&path, "gone.txt", "gone\n");
    fixture.commit_all_at(&path, T_IN1, "work on a branch whose remote is deleted");
    fixture.publish("gone", "main");
    fixture.unpublish("gone");

    let git = git();
    let report = git.report(&id(&git, &path), &window());
    assert_eq!(
        report.tracking,
        Tracking::UpstreamMissing {
            name: "origin/gone".to_string()
        },
        "a configured-but-dangling upstream must not read as `no upstream`"
    );
}

// ---------------------------------------------------------------------------
// Landed
// ---------------------------------------------------------------------------

#[test]
fn the_default_branch_reports_itself_rather_than_merging_into_itself() {
    let fixture = Fixture::new("is-default");
    fixture.fake_origin();
    let git = git();
    let report = git.report(&id(&git, &fixture.repo), &window());
    assert_eq!(
        report.landed,
        Landed::IsDefault {
            name: "origin/main".to_string()
        }
    );
}

#[test]
fn a_merged_branch_and_an_unmerged_one_are_told_apart() {
    let fixture = Fixture::new("landing");
    let landed = fixture.merged_worktree("landed", "landed");
    let flying = fixture.unmerged_worktree("flying", "flying");
    let git = git();

    let landed = git.report(&id(&git, &landed), &window());
    assert_eq!(
        landed.landed,
        Landed::Merged {
            into: "main".to_string()
        },
        "{:?}",
        landed.problems
    );

    let flying = git.report(&id(&git, &flying), &window());
    assert_eq!(
        flying.landed,
        Landed::NotMerged {
            into: "main".to_string()
        }
    );
}

#[test]
fn origin_head_wins_over_the_local_name_candidates() {
    let fixture = Fixture::new("origin-head");
    fixture.fake_origin();
    let path = fixture.unmerged_worktree("topic", "topic");
    let git = git();
    let report = git.report(&id(&git, &path), &window());
    assert_eq!(
        report.landed,
        Landed::NotMerged {
            into: "origin/main".to_string()
        }
    );
}

/// Never a bare `NotMerged` when the question could not be asked: "did not land"
/// and "we could not find the trunk" are opposite messages.
#[test]
fn a_repository_with_no_identifiable_default_branch_says_unknown() {
    let fixture = Fixture::new("no-default");
    // Rename the only branch to something none of the candidates match, and
    // give it no remote at all.
    fixture.git(&fixture.repo, &["branch", "-m", "main", "wandering"]);
    let git = git();

    let report = git.report(&id(&git, &fixture.repo), &window());
    match &report.landed {
        Landed::Unknown { reason } => assert!(
            reason.contains("no default branch"),
            "unhelpful reason: {reason}"
        ),
        other => panic!("expected Unknown, got {other:?}"),
    }
}

/// The three ways work can be on the trunk, told apart.
///
/// Containment is exact. A squash merge leaves only the branch's combined
/// patch; a rebase merge leaves every individual patch and not one sha. Before
/// the last two were detected they were both reported as not merged, which on a
/// repository that squash-merges is every branch that ever shipped.
#[test]
fn exact_squash_and_rebase_landings_are_told_apart() {
    let fixture = Fixture::new("landing-shapes");
    let exact = fixture.merged_worktree("exact", "exact");
    let squashed = fixture.squash_merged_worktree("squashed", "squashed");
    // Captured here, before the rebase fixture moves the trunk on again: this
    // is the sha a reader would find by hand, and the plugin has to name it.
    let squash_oid = fixture.git(&fixture.repo, &["rev-parse", "main"]);
    let rebased = fixture.rebase_merged_worktree("rebased", "rebased");
    let git = git();

    let exact = git.report(&id(&git, &exact), &window());
    assert_eq!(
        exact.landed,
        Landed::Merged {
            into: "main".to_string()
        },
        "an exact ancestor must not be downgraded to a patch match: {:?}",
        exact.problems
    );

    let squashed = git.report(&id(&git, &squashed), &window());
    assert_eq!(
        squashed.landed,
        Landed::Equivalent {
            into: "main".to_string(),
            how: Equivalence::Squashed { oid: squash_oid },
        },
        "a squash merge must be found, and by the trunk sha that carries it: {:?}",
        squashed.problems
    );

    let rebased = git.report(&id(&git, &rebased), &window());
    assert_eq!(
        rebased.landed,
        Landed::Equivalent {
            into: "main".to_string(),
            how: Equivalence::EveryCommit { commits: 2 },
        },
        "{:?}",
        rebased.problems
    );
}

/// A reader's own git config must not be able to hide a squash merge.
///
/// The two sides of the patch-id comparison are produced by different commands,
/// and the collector runs against the repository's real configuration.
/// `diff-tree` is plumbing and takes git's basic diff config; `log` is porcelain
/// and takes the UI config on top, so `diff.noprefix`, `diff.context` and
/// `diff.srcPrefix` move one side and not the other. Unpinned, every squash
/// merge on such a machine goes back to reading as not merged — this exact bug,
/// restored by a setting that has nothing to do with merging.
#[test]
fn a_hostile_diff_config_cannot_hide_a_squash_merge() {
    let fixture = Fixture::new("hostile-diff-config");
    let squashed = fixture.squash_merged_worktree("squashed", "squashed");
    let squash_oid = fixture.git(&fixture.repo, &["rev-parse", "main"]);
    // Repository-local, because that is the one layer the collector is
    // guaranteed to read: it scrubs `GIT_DIR` and friends but deliberately not
    // `HOME`, so a contributor's global config reaches it as well and this test
    // would otherwise be quietly machine-dependent.
    for (key, value) in [
        ("diff.noprefix", "true"),
        ("diff.context", "7"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.srcPrefix", "i/"),
        ("diff.dstPrefix", "w/"),
        ("diff.renames", "copies"),
        ("diff.algorithm", "histogram"),
        ("format.pretty", "oneline"),
    ] {
        fixture.git(&fixture.repo, &["config", key, value]);
    }
    let git = git();

    let report = git.report(&id(&git, &squashed), &window());
    assert_eq!(
        report.landed,
        Landed::Equivalent {
            into: "main".to_string(),
            how: Equivalence::Squashed { oid: squash_oid },
        },
        "{:?}",
        report.problems
    );
}

/// Half a branch on the trunk is not a landing, and neither probe may round it
/// up: `git cherry` prints one `-` and one `+`, and the branch's combined patch
/// matches nothing. Eagerness here would be worse than the bug being fixed —
/// "safe to delete" is what a reader takes from a landing.
#[test]
fn a_partly_cherry_picked_branch_has_not_landed() {
    let fixture = Fixture::new("partial-landing");
    let path = fixture.worktree("partial", "partial");
    fixture.write(&path, "picked.txt", "this one lands\n");
    fixture.commit_all_at(&path, T_IN1, "the commit that gets picked");
    fixture.write(&path, "left-behind.txt", "this one does not\n");
    fixture.commit_all_at(&path, T_IN2, "the commit that stays behind");

    let picked = fixture.git(&path, &["rev-parse", "HEAD~1"]);
    fixture.git_at(&fixture.repo, T_IN3, T_IN3, &["cherry-pick", &picked]);

    let git = git();
    let report = git.report(&id(&git, &path), &window());
    assert_eq!(
        report.landed,
        Landed::NotMerged {
            into: "main".to_string()
        },
        "{:?}",
        report.problems
    );
}

// ---------------------------------------------------------------------------
// Failure is data
// ---------------------------------------------------------------------------

/// A wedged repository must be a loud line in the digest, never a silently
/// empty one. A zero timeout kills every invocation, which is the only
/// deterministic way to produce that state.
#[test]
fn a_timed_out_invocation_becomes_a_problem_not_an_empty_result() {
    let fixture = Fixture::new("timeout");
    fixture.commits_around_the_window();
    let patient = git();
    let checkout = id(&patient, &fixture.repo);

    let impatient = Git::new(Duration::from_nanos(1));
    let report = impatient.report(&checkout, &window());

    assert!(
        !report.problems.is_empty(),
        "a repository that never answered was reported as clean and quiet"
    );
    assert!(
        report.problems.iter().any(|p| p.contains("timed out")),
        "the problems do not say what happened: {:?}",
        report.problems
    );
    assert_eq!(report.activity(), Activity::Broken);
}

#[test]
fn the_resolved_git_binary_is_named_in_errors() {
    let git = Git::new(TIMEOUT);
    assert!(git.program().to_string_lossy().contains("git"));
    assert_eq!(git.timeout(), TIMEOUT);
}
