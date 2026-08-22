//! What **git** does, asserted directly, with no standup logic in the way.
//!
//! This file exists so that a red CI row can be triaged at a glance. Every
//! assertion here is a claim about git that `docs/git-plumbing.md` makes and
//! that `src/git.rs` is built on. If something in this file fails, the
//! environment changed under the plugin — a new git version, a runner image
//! with different defaults — and the fix starts by re-reading the plumbing
//! notes. If everything here passes and something else fails, the plugin is
//! wrong.
//!
//! That distinction is not academic. The first cross-platform run found three
//! failures, none of them faults in the plugin, and one of them a real upstream
//! change: git 2.55 redefined `--since today` from the current instant to local
//! midnight. Telling those two apart cost more than fixing either.
//!
//! Two rules for anything added here:
//!
//! 1. **No standup code.** These run git and read its output. A test that goes
//!    through the collector belongs in `git_collect.rs`, because a failure there
//!    could be either thing.
//! 2. **Assert the claim, and say what depends on it.** Every message names the
//!    behaviour the plugin relies on, so whoever reads the failure knows what to
//!    go and look at rather than just that a number moved.

#[path = "fixtures.rs"]
mod fixtures;

use fixtures::{lines, Fixture, T_IN1, T_IN2, T_OLDER, T_SINCE};

/// The version of git these assertions were made against, for the record. Not
/// asserted — the point of this file is to run on gits nobody has tried yet.
fn git_version(fixture: &Fixture) -> String {
    fixture.git(fixture.root(), &["--version"])
}

/// `--no-optional-locks` does **not** cover `git diff`.
///
/// The whole `ScratchIndex` mechanism in `src/git.rs` exists for this: `diff`
/// refreshes the index as part of doing its job and writes the refresh back,
/// which `status` does only optionally. If git ever makes that writeback
/// optional too, the index copy becomes unnecessary — but until this test fails,
/// it is load-bearing and must not be removed.
#[test]
fn the_no_optional_locks_flag_still_does_not_cover_diff() {
    let fixture = Fixture::new("contract-diff-writeback");
    fixture.write(&fixture.repo, "tracked.txt", &lines(40, None));
    fixture.commit_all_at(&fixture.repo, T_IN1, "a file to disturb");
    // The writeback only happens when git has something to write, which means a
    // stale stat cache. A fresh one is exactly the state that hid this bug.
    fixture.make_stat_cache_stale(&fixture.repo);

    let index = fixture.git_dir(&fixture.repo).join("index");
    let before = std::fs::read(&index).expect("read the index");

    let (code, _out, err) = fixture.try_git(
        &fixture.repo,
        &["--no-optional-locks", "diff", "--shortstat"],
    );
    assert_eq!(code, 0, "{err}");

    let after = std::fs::read(&index).expect("read the index again");
    assert_ne!(
        before,
        after,
        "git {} no longer rewrites the index for `diff --shortstat` under \
         --no-optional-locks. If that is deliberate upstream, the ScratchIndex \
         copy in src/git.rs and the note in docs/git-plumbing.md can go — but \
         check `status` too before removing anything.",
        git_version(&fixture)
    );
}

/// `status` with the same flag does not, which is the other half of the pair and
/// the reason the flag is passed everywhere.
#[test]
fn the_no_optional_locks_flag_does_cover_status() {
    let fixture = Fixture::new("contract-status-writeback");
    fixture.write(&fixture.repo, "tracked.txt", &lines(40, None));
    fixture.commit_all_at(&fixture.repo, T_IN1, "a file to disturb");
    fixture.make_stat_cache_stale(&fixture.repo);

    let index = fixture.git_dir(&fixture.repo).join("index");
    let before = std::fs::read(&index).expect("read the index");

    fixture.git(
        &fixture.repo,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
        ],
    );

    let after = std::fs::read(&index).expect("read the index again");
    assert_eq!(
        before,
        after,
        "git {} now rewrites the index for `status` even under \
         --no-optional-locks. The plugin's read-only guarantee rests on this \
         flag; tests/read_only.rs will be failing too.",
        git_version(&fixture)
    );
}

/// `git rev-parse --since=<garbage>` exits **0** and answers *now*.
///
/// `Git::resolve_date` compares the resolved epoch against the current time and
/// rejects anything that lands on now unless the spec says so, purely because of
/// this. If git ever starts refusing unparseable input, that guard is no longer
/// the only thing standing between a typo and a digest that is empty, correctly
/// formatted and a lie.
#[test]
fn an_unparseable_since_still_exits_zero_and_answers_now() {
    let fixture = Fixture::new("contract-garbage-date");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_secs() as i64;

    let (code, out, err) = fixture.try_git(&fixture.repo, &["rev-parse", "--since=bogusgarbage"]);
    assert_eq!(
        code,
        0,
        "git {} now refuses an unparseable --since (stderr: {err}). That is an \
         improvement upstream, and it means the `SPECS_MEANING_NOW` guard in \
         src/git.rs is no longer the only defence against a typo rendering as a \
         quiet day.",
        git_version(&fixture)
    );

    let epoch: i64 = out
        .strip_prefix("--max-age=")
        .unwrap_or_else(|| panic!("git answered {out:?}, not --max-age=<epoch>"))
        .trim()
        .parse()
        .unwrap_or_else(|err| panic!("git answered {out:?}: {err}"));
    assert!(
        (epoch - now).abs() < 120,
        "git {} answered {epoch} for garbage, which is no longer the current \
         instant ({now}). resolve_date detects the silent failure by comparing \
         against now; if git has changed what it answers, that comparison needs \
         revisiting.",
        git_version(&fixture)
    );
}

/// `--since` / `--max-age` **prunes** the walk, and the pruning loses commits.
///
/// This is why the collector uses `--since-as-filter`. The fixture has two
/// in-window commits with an old one wedged between them; the pruning form stops
/// at the old one and never reaches what is behind it.
#[test]
fn max_age_still_prunes_the_walk_and_loses_commits() {
    let fixture = Fixture::new("contract-pruning");
    fixture.skewed_history();

    let pruned = fixture.git(
        &fixture.repo,
        &["log", &format!("--max-age={T_SINCE}"), "--format=%s"],
    );
    let pruned: Vec<&str> = pruned.lines().collect();

    assert_eq!(
        pruned,
        vec!["skew: in window, newest"],
        "git {} no longer prunes on --max-age. The fallback path in src/git.rs \
         and the measurement in docs/git-plumbing.md both assume it does, and \
         the problem it records on old git would now be a false alarm.",
        git_version(&fixture)
    );
}

/// `--since-as-filter` exists, and applies the comparison **without** pruning.
///
/// git 2.37 and newer. On anything older the collector falls back to `--max-age`
/// and records a problem, so a failure here on an old runner is expected and
/// tells you which half of that code path the row is exercising.
#[test]
fn since_as_filter_exists_and_does_not_prune() {
    let fixture = Fixture::new("contract-filtering");
    fixture.skewed_history();

    let (code, out, err) = fixture.try_git(
        &fixture.repo,
        &[
            "log",
            &format!("--since-as-filter=@{T_SINCE}"),
            "--format=%s",
        ],
    );
    assert_eq!(
        code,
        0,
        "git {} does not accept --since-as-filter (stderr: {err}). That is \
         expected below git 2.37: the collector falls back to --max-age and \
         records the degradation as a problem. Nothing to fix unless this row is \
         meant to be a modern git.",
        git_version(&fixture)
    );

    let mut reported: Vec<&str> = out.lines().collect();
    reported.sort_unstable();
    assert_eq!(
        reported,
        vec!["skew: in window, newest", "skew: in window, oldest"],
        "git {} no longer reports both in-window commits for \
         --since-as-filter. The collector relies on this being a filter rather \
         than a cutoff; a digest that silently drops commits is the failure the \
         plugin exists to prevent.",
        git_version(&fixture)
    );
}

/// `patch-id --stable` gives the same id for the same change whether the diff
/// came from `diff-tree -p` or `log -p`, **given the pinned options**.
///
/// The landing probes compare exactly those two. `diff-tree` is plumbing and
/// `log` is porcelain, so they do not read the same diff config, which is why
/// `-U3`, `--src-prefix`, `--dst-prefix` and `--no-renames` are passed
/// explicitly. A failure here means the pinned set is no longer sufficient and
/// every squash merge is about to read as "not merged".
#[test]
fn patch_id_agrees_between_plumbing_and_porcelain_under_the_pinned_options() {
    let fixture = Fixture::new("contract-patch-id");
    fixture.write(&fixture.repo, "long.txt", &lines(40, None));
    fixture.commit_all_at(&fixture.repo, T_IN1, "a long file");
    let base = fixture.head_oid(&fixture.repo);
    fixture.write(&fixture.repo, "long.txt", &lines(40, Some(20)));
    fixture.commit_all_at(&fixture.repo, T_IN2, "edit deep inside it");
    let head = fixture.head_oid(&fixture.repo);

    // A config that moves the porcelain side and not the plumbing side. Written
    // into the repository, which is the layer the plugin is guaranteed to read.
    for (key, value) in [
        ("diff.noprefix", "true"),
        ("diff.context", "7"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.renames", "copies"),
    ] {
        fixture.git(&fixture.repo, &["config", key, value]);
    }

    let options = [
        "-p",
        "-U3",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--no-renames",
        "--no-textconv",
    ];
    let mut tree_args = vec!["diff-tree"];
    tree_args.extend_from_slice(&options);
    tree_args.push(&base);
    tree_args.push(&head);

    let range = format!("{base}..{head}");
    let mut log_args = vec!["log"];
    log_args.extend_from_slice(&options);
    log_args.push("--no-merges");
    log_args.push("--format=commit %H");
    log_args.push(&range);

    let from_plumbing = fixture.patch_id(&fixture.repo, &tree_args);
    let from_porcelain = fixture.patch_id(&fixture.repo, &log_args);

    assert!(!from_plumbing.is_empty(), "diff-tree produced no patch id");
    assert_eq!(
        from_plumbing.first().map(|(id, _)| id.as_str()),
        from_porcelain.first().map(|(id, _)| id.as_str()),
        "git {} no longer agrees between `diff-tree -p` and `log -p` under the \
         options src/git.rs pins. Something else in the diff config is reaching \
         one side and not the other, and until it is pinned too every squash \
         merge on a machine with that setting will read as not merged.",
        git_version(&fixture)
    );
}

/// `rev-list --count HEAD --not --remotes` counts commits that are on no remote.
///
/// The unpushed state is this number. Asserted here because the meaning of
/// `--remotes` as a pseudo-ref, and of `--not` applying to what follows it, are
/// both git's to change.
#[test]
fn rev_list_not_remotes_counts_what_is_on_no_remote() {
    let fixture = Fixture::new("contract-unpushed");
    fixture.fake_origin();
    let path = fixture.worktree("flying", "flying");
    fixture.write(&path, "flying.txt", "only here\n");
    fixture.commit_all_at(&path, T_IN1, "not pushed");
    fixture.write(&path, "flying-2.txt", "also only here\n");
    fixture.commit_all_at(&path, T_IN2, "also not pushed");

    let counted = fixture.git(
        &path,
        &["rev-list", "--count", "HEAD", "--not", "--remotes"],
    );
    assert_eq!(
        counted,
        "2",
        "git {} counted {counted:?} commits outside every remote where two are \
         genuinely unpushed. `Unpushed::Commits` is exactly this number, and the \
         digest tells people what a `worktree remove` would destroy.",
        git_version(&fixture)
    );

    // And the trunk, which is on the remote, is holding nothing.
    let on_remote = fixture.git(
        &fixture.repo,
        &["rev-list", "--count", "HEAD", "--not", "--remotes"],
    );
    assert_eq!(
        on_remote,
        "0",
        "git {} counts pushed commits as unpushed, which would report every \
         checkout as holding work at risk.",
        git_version(&fixture)
    );
}

/// `symbolic-ref -q HEAD` does **not** tell an unborn branch from one deleted
/// underneath a live checkout.
///
/// Both answer 0 with the same ref name, which is why `src/git.rs` discriminates
/// on the worktree's own `logs/HEAD` instead. If git ever starts distinguishing
/// them, that indirection can go — and the two states matter, because one is a
/// brand-new repository and the other is somebody's work about to be lost.
#[test]
fn symbolic_ref_still_cannot_tell_unborn_from_deleted() {
    let fixture = Fixture::new("contract-head-states");
    let unborn = fixture.unborn_worktree("fresh", "fresh");
    let deleted = fixture.deleted_branch_worktree("doomed", "doomed");

    let (unborn_code, unborn_ref, _) = fixture.try_git(&unborn, &["symbolic-ref", "-q", "HEAD"]);
    let (deleted_code, deleted_ref, _) = fixture.try_git(&deleted, &["symbolic-ref", "-q", "HEAD"]);

    assert_eq!(
        (unborn_code, deleted_code),
        (0, 0),
        "git {} now fails `symbolic-ref -q HEAD` for one of unborn or \
         deleted-branch. That would be a *better* discriminator than the \
         logs/HEAD check in src/git.rs — see the degenerate-states table in \
         docs/git-plumbing.md.",
        git_version(&fixture)
    );
    assert_eq!(
        unborn_ref,
        "refs/heads/fresh",
        "git {} answered {unborn_ref:?} for an unborn branch",
        git_version(&fixture)
    );
    assert_eq!(
        deleted_ref,
        "refs/heads/doomed",
        "git {} answered {deleted_ref:?} for a deleted branch",
        git_version(&fixture)
    );

    // The discriminator the plugin actually uses, asserted so a change in *it*
    // is also caught here rather than in a collector test.
    let unborn_log = fixture.git_dir(&unborn).join("logs/HEAD");
    let deleted_log = fixture.git_dir(&deleted).join("logs/HEAD");
    assert!(
        !unborn_log.exists(),
        "an unborn worktree grew a HEAD reflog at {}; the discriminator in \
         src/git.rs no longer works",
        unborn_log.display()
    );
    assert!(
        deleted_log.exists(),
        "a worktree whose branch was deleted has no HEAD reflog at {}; the \
         discriminator in src/git.rs no longer works",
        deleted_log.display()
    );
}

/// A `--numstat` line for a **binary** file prints `-` for both counts.
///
/// The parser reads that as "count the file, add nothing to the lines", which is
/// also how generated and vendored paths are treated. A different spelling here
/// would silently turn binary files into zero-line text files.
#[test]
fn numstat_still_spells_a_binary_file_with_dashes() {
    let fixture = Fixture::new("contract-numstat-binary");
    fixture.write_raw(&fixture.repo, b"blob.bin", b"\x00\x01\x02binary\x00\xff");
    fixture.commit_all_at(&fixture.repo, T_IN1, "a binary file");

    let numstat = fixture.git(
        &fixture.repo,
        &["log", "-1", "--numstat", "--format=", "--no-renames"],
    );
    assert_eq!(
        numstat.trim(),
        "-\t-\tblob.bin",
        "git {} spells a binary numstat line differently now. parse_numstat in \
         src/git.rs reads the two dashes as \"no line counts\"; another spelling \
         would be parsed as zero lines, which is a different claim.",
        git_version(&fixture)
    );
}

/// An old commit reachable only behind a newer one is still found by `--all`,
/// and `for-each-ref --contains` reports nothing once no ref reaches it.
///
/// The unpushed state's promise is that removing a checkout loses the work. This
/// is the command that shows it, so it is asserted here rather than assumed.
#[test]
fn for_each_ref_contains_reports_nothing_for_an_unreachable_commit() {
    let fixture = Fixture::new("contract-unreachable");
    fixture.fake_origin();
    let path = fixture.worktree("doomed", "doomed");
    fixture.write(&path, "doomed.txt", "about to be lost\n");
    fixture.commit_all_at(&path, T_OLDER, "work nobody will find");
    let oid = fixture.head_oid(&path);

    let before = fixture.git(
        &fixture.repo,
        &[
            "for-each-ref",
            &format!("--contains={oid}"),
            "--format=%(refname)",
        ],
    );
    assert!(
        before.contains("refs/heads/doomed"),
        "git {} does not report the branch that contains a commit: {before:?}",
        git_version(&fixture)
    );

    fixture.git(
        &fixture.repo,
        &["worktree", "remove", "--force", path.to_str().unwrap()],
    );
    fixture.git(&fixture.repo, &["branch", "-D", "doomed"]);

    let after = fixture.git(
        &fixture.repo,
        &[
            "for-each-ref",
            &format!("--contains={oid}"),
            "--format=%(refname)",
        ],
    );
    assert!(
        after.is_empty(),
        "git {} still reaches a commit whose worktree and branch are both gone \
         via {after:?}. The unpushed state promises that removing a checkout \
         loses the work; if that is no longer true the promise needs rewording.",
        git_version(&fixture)
    );
}
