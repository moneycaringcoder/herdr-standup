//! Throwaway git fixtures for the integration tests.
//!
//! Everything lives under a unique temp directory that is removed on drop, and
//! every fixture repository gets its own local `user.name`, `user.email` and
//! neutralised `core.excludesFile`/`core.hooksPath`, so a CI runner with no git
//! identity and no global config still produces the same results as a developer
//! laptop. The fixture's own git invocations additionally run with `HOME`,
//! `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` pointed inside the temp tree.
//!
//! **Times are absolute, never relative.** A fixture built with "2 hours ago"
//! would drift across a slow CI run and turn a window assertion into a flake, so
//! every commit here is stamped with a fixed epoch and the tests build their
//! [`standup::model::Window`] out of the same constants.
//!
//! This file is included by the other test binaries with
//! `#[path = "fixtures.rs"] mod fixtures;`, so cargo also builds it as an
//! integration test target of its own with no tests in it. That is harmless.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use standup::model::{Stamp, Window, WindowSource};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Well before the window: 2026-07-24 00:00:00 UTC.
pub const T_OLD: i64 = 1_784_851_200;
/// A second old instant, further back still: 2026-07-20 00:00:00 UTC.
pub const T_OLDER: i64 = 1_784_505_600;
/// The window start: 2026-08-01 00:00:00 UTC.
pub const T_SINCE: i64 = 1_785_542_400;
/// Inside the window, in ascending order: 2026-08-02, -03 and -04.
pub const T_IN1: i64 = 1_785_628_800;
pub const T_IN2: i64 = 1_785_715_200;
pub const T_IN3: i64 = 1_785_801_600;
/// The window's `--until`, when a test sets one: 2026-08-05.
pub const T_UNTIL: i64 = 1_785_888_000;
/// After that bound: 2026-08-06.
pub const T_AFTER: i64 = 1_785_974_400;

/// The window the tests assert against: `T_SINCE` to now.
pub fn window() -> Window {
    window_between(T_SINCE, None)
}

pub fn window_between(since: i64, until: Option<i64>) -> Window {
    Window {
        since: standup::clock::stamp(since),
        until: until.map(standup::clock::stamp),
        source: WindowSource::Explicit {
            spec: format!("@{since}"),
        },
    }
}

pub fn stamp(epoch: i64) -> Stamp {
    standup::clock::stamp(epoch)
}

/// A temp directory containing one or more git repositories.
pub struct Fixture {
    root: PathBuf,
    /// The main worktree of the primary repository.
    pub repo: PathBuf,
}

impl Fixture {
    /// A repository on `main` with one base commit, dated outside the window.
    pub fn new(tag: &str) -> Fixture {
        let fixture = Fixture::empty(tag);
        fixture.write(&fixture.repo, ".gitignore", "ignored/\n*.log\n");
        fixture.write(&fixture.repo, "base.txt", "base\n");
        fixture.commit_all_at(&fixture.repo, T_OLD, "base");
        fixture
    }

    /// A repository that has been `git init`-ed and nothing more: an unborn
    /// branch, with no HEAD reflog.
    pub fn empty(tag: &str) -> Fixture {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "standup-fixture-{}-{tag}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).expect("create fixture root");
        // An empty global config file, so nothing on the host leaks in.
        std::fs::write(root.join("home/.gitconfig"), "").expect("write empty global config");
        std::fs::write(root.join("empty-excludes"), "").expect("write empty excludes");
        std::fs::create_dir_all(root.join("no-hooks")).expect("create empty hooks dir");

        let fixture = Fixture {
            root: root.clone(),
            repo: root.join("repo"),
        };
        fixture.init_repo(&fixture.repo);
        fixture
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates and configures a fresh repository at `path`.
    pub fn init_repo(&self, path: &Path) {
        std::fs::create_dir_all(path).expect("create repo dir");
        self.git(path, &["init", "-q", "-b", "main"]);
        self.configure_repo(path);
    }

    /// The configuration every fixture repository gets, whether it was created
    /// by `init` or by `clone`.
    pub fn configure_repo(&self, path: &Path) {
        // Local config only, so a runner with no identity still commits.
        self.git(path, &["config", "user.email", "fixture@example.invalid"]);
        self.git(path, &["config", "user.name", "standup fixture"]);
        self.git(path, &["config", "init.defaultBranch", "main"]);
        self.git(path, &["config", "commit.gpgsign", "false"]);
        self.git(path, &["config", "tag.gpgsign", "false"]);
        self.git(path, &["config", "gc.auto", "0"]);
        // Pinned rather than inherited: the plugin's own git runs with the
        // host's global config, so anything the tests assert exact numbers
        // about has to be nailed down in the repository itself.
        self.git(path, &["config", "diff.renames", "true"]);
        self.git(path, &["config", "status.renames", "true"]);
        self.git(path, &["config", "log.showSignature", "false"]);
        self.git(path, &["config", "core.autocrlf", "false"]);
        // Neutralise anything the host would otherwise contribute.
        self.git(
            path,
            &[
                "config",
                "core.excludesFile",
                self.root.join("empty-excludes").to_str().unwrap(),
            ],
        );
        self.git(
            path,
            &[
                "config",
                "core.hooksPath",
                self.root.join("no-hooks").to_str().unwrap(),
            ],
        );
    }

    /// Runs git, panicking with the stderr on failure. Returns trimmed stdout.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.try_git(cwd, args);
        assert_eq!(
            code,
            0,
            "git {} failed in {}: {stderr}",
            args.join(" "),
            cwd.display()
        );
        stdout
    }

    pub fn try_git(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        self.try_git_at(cwd, T_OLD, T_OLD, args)
    }

    /// Runs git with the author and committer clocks pinned, which is how every
    /// commit in these fixtures gets a deterministic date.
    pub fn try_git_at(
        &self,
        cwd: &Path,
        author: i64,
        committer: i64,
        args: &[&str],
    ) -> (i32, String, String) {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("GIT_CONFIG_GLOBAL", self.root.join("home/.gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", "standup fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "standup fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env("GIT_AUTHOR_DATE", format!("{author} +0000"))
            .env("GIT_COMMITTER_DATE", format!("{committer} +0000"))
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("spawn git");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    pub fn git_at(&self, cwd: &Path, author: i64, committer: i64, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.try_git_at(cwd, author, committer, args);
        assert_eq!(
            code,
            0,
            "git {} failed in {}: {stderr}",
            args.join(" "),
            cwd.display()
        );
        stdout
    }

    /// Writes a file, creating parent directories.
    pub fn write(&self, cwd: &Path, rel: &str, contents: &str) {
        self.write_raw(cwd, rel.as_bytes(), contents.as_bytes());
    }

    /// Writes a file whose *name* is raw bytes, so a path can carry a space, a
    /// newline and a byte that is not valid UTF-8.
    pub fn write_raw(&self, cwd: &Path, rel: &[u8], contents: &[u8]) {
        self.try_write_raw(cwd, rel, contents)
            .unwrap_or_else(|e| panic!("create {:?}: {e}", bytes_to_path(rel)));
    }

    /// [`Fixture::write_raw`], handing back the error instead of panicking.
    ///
    /// Only one caller needs this, and the reason is a real difference between
    /// filesystems rather than a flaky test: APFS enforces that a filename is
    /// valid UTF-8 and refuses one that is not with `EILSEQ`, where ext4 treats
    /// a name as arbitrary bytes with only `/` and NUL reserved.
    pub fn try_write_raw(&self, cwd: &Path, rel: &[u8], contents: &[u8]) -> std::io::Result<()> {
        let path = cwd.join(bytes_to_path(rel));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&path)?;
        file.write_all(contents)
    }

    /// Commits whatever is staged, at a fixed instant.
    pub fn commit_at(&self, cwd: &Path, epoch: i64, message: &str) {
        self.git_at(cwd, epoch, epoch, &["commit", "-q", "-m", message]);
    }

    pub fn commit_all_at(&self, cwd: &Path, epoch: i64, message: &str) {
        self.git_at(cwd, epoch, epoch, &["add", "-A"]);
        self.commit_at(cwd, epoch, message);
    }

    /// A commit whose author and committer clocks disagree. Used to prove which
    /// of the two `--since` actually filters on.
    pub fn commit_all_skewed(&self, cwd: &Path, author: i64, committer: i64, message: &str) {
        self.git_at(cwd, author, committer, &["add", "-A"]);
        self.git_at(cwd, author, committer, &["commit", "-q", "-m", message]);
    }

    pub fn head_oid(&self, cwd: &Path) -> String {
        self.git(cwd, &["rev-parse", "HEAD"])
    }

    pub fn git_dir(&self, cwd: &Path) -> PathBuf {
        PathBuf::from(self.git(cwd, &["rev-parse", "--path-format=absolute", "--git-dir"]))
    }

    pub fn common_dir(&self, cwd: &Path) -> PathBuf {
        PathBuf::from(self.git(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ))
    }

    /// `git <args> | git patch-id --stable`, as `(patch id, commit)` pairs.
    ///
    /// Only `git_contract.rs` needs this: it asserts that git itself agrees
    /// between the plumbing and porcelain diff forms, which is a claim about git
    /// rather than about the collector.
    pub fn patch_id(&self, cwd: &Path, args: &[&str]) -> Vec<(String, String)> {
        let diff = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_GLOBAL", self.root.join("home/.gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .output()
            .expect("spawn git");
        assert!(
            diff.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&diff.stderr)
        );

        let mut child = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["patch-id", "--stable"])
            .env("GIT_CONFIG_GLOBAL", self.root.join("home/.gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn git patch-id");
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(&diff.stdout)
            .expect("feed patch-id");
        let out = child.wait_with_output().expect("wait for patch-id");
        assert!(out.status.success(), "git patch-id failed");

        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                Some((fields.next()?.to_string(), fields.next()?.to_string()))
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Worktree shapes
    // -----------------------------------------------------------------------

    /// A linked worktree on a new branch off `main`.
    pub fn worktree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.root.join(name);
        self.git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap(),
                "main",
            ],
        );
        path
    }

    /// A linked worktree with a detached HEAD.
    pub fn detached_worktree(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        self.git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                path.to_str().unwrap(),
                "main",
            ],
        );
        path
    }

    /// A linked worktree whose branch has never had a commit.
    pub fn unborn_worktree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.root.join(name);
        self.git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "--orphan",
                "-b",
                branch,
                path.to_str().unwrap(),
            ],
        );
        path
    }

    /// A linked worktree whose branch was deleted underneath it. Byte-identical
    /// to the unborn case for `symbolic-ref`, which is exactly why it needs its
    /// own fixture: the only discriminator is that this one has a `logs/HEAD`.
    pub fn deleted_branch_worktree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.worktree(name, branch);
        self.write(&path, "deleted.txt", "work that outlived its branch\n");
        self.commit_all_at(&path, T_IN1, "work on a branch about to vanish");
        self.git(
            &self.repo,
            &["update-ref", "-d", &format!("refs/heads/{branch}")],
        );
        path
    }

    // -----------------------------------------------------------------------
    // History shapes
    // -----------------------------------------------------------------------

    /// Two commits before the window and two inside it, on `main`.
    ///
    /// The in-window pair touches one file twice, so a churn implementation that
    /// sums per-commit file counts instead of taking the union of paths reports
    /// two files instead of one.
    pub fn commits_around_the_window(&self) {
        self.write(&self.repo, "outside.txt", "one\n");
        self.commit_all_at(&self.repo, T_OLD + 3_600, "outside the window");
        self.write(&self.repo, "inside.txt", "one\ntwo\n");
        self.commit_all_at(&self.repo, T_IN1, "inside: two lines");
        self.write(&self.repo, "inside.txt", "one\nTWO\nthree\n");
        self.commit_all_at(&self.repo, T_IN2, "inside: edit the same file again");
    }

    /// A commit touching a binary file and a path containing a space, a newline
    /// and — where the filesystem allows it — a byte that is not valid UTF-8.
    /// Returns the awkward path's raw bytes as they were actually written.
    ///
    /// The non-UTF-8 byte is conditional because APFS refuses such a name
    /// outright with `EILSEQ`, so on macOS there is no way to put one on disk to
    /// read back. The space and the newline are exercised everywhere, and they
    /// are the two that the `-z` framing exists for; the invalid byte only ever
    /// tested that a `String` renders it as a replacement character.
    pub fn awkward_paths_commit(&self, epoch: i64) -> Vec<u8> {
        self.write_raw(&self.repo, b"blob.bin", b"\x00\x01\x02binary\x00\xff");

        let mut awkward: Vec<u8> = b"odd \xff\nname.txt".to_vec();
        if self
            .try_write_raw(&self.repo, &awkward, b"awkward\n")
            .is_err()
        {
            eprintln!(
                "note: this filesystem refuses a filename that is not valid UTF-8, so the \
                 awkward path is exercised with its space and its newline only"
            );
            awkward = b"odd \nname.txt".to_vec();
            self.write_raw(&self.repo, &awkward, b"awkward\n");
        }

        self.commit_all_at(&self.repo, epoch, "a binary file and an awkward path");
        awkward
    }

    /// A commit whose subject contains a tab, a pipe, backticks, a unit
    /// separator and a record separator — every byte the log parser frames on.
    pub fn tricky_subject_commit(&self, epoch: i64) -> String {
        let subject = "weird\tsubject |`x`| \u{1f} unit \u{1e} record end";
        self.write(&self.repo, "tricky.txt", "tricky\n");
        self.commit_all_at(&self.repo, epoch, subject);
        subject.to_string()
    }

    /// A merge commit on `main`, with one commit on each side. Returns the name
    /// of the merged-in branch.
    pub fn merge_commit(&self, epoch: i64) -> String {
        let branch = "side";
        self.git(&self.repo, &["switch", "-q", "-c", branch]);
        self.write(&self.repo, "side.txt", "side\n");
        self.commit_all_at(&self.repo, epoch - 200, "side work");
        self.git(&self.repo, &["switch", "-q", "main"]);
        self.write(&self.repo, "trunk.txt", "trunk\n");
        self.commit_all_at(&self.repo, epoch - 100, "trunk work");
        self.git_at(
            &self.repo,
            epoch,
            epoch,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "merge side into main",
                branch,
            ],
        );
        branch.to_string()
    }

    /// A history whose committer timestamps are out of order: two commits inside
    /// the window with a commit dated long before the window wedged between
    /// them.
    ///
    /// `--max-age` is a traversal cutoff rather than a filter, so it stops at the
    /// backdated commit and never reaches the in-window commits behind it. The
    /// test that uses this pins what git actually does.
    pub fn skewed_history(&self) {
        self.write(&self.repo, "skew-a.txt", "a\n");
        self.commit_all_at(&self.repo, T_IN1, "skew: in window, oldest");
        self.write(&self.repo, "skew-b.txt", "b\n");
        self.commit_all_at(&self.repo, T_OLDER, "skew: backdated, hides the rest");
        self.write(&self.repo, "skew-c.txt", "c\n");
        self.commit_all_at(&self.repo, T_IN2, "skew: in window, newest");
    }

    /// A commit that touches a hand-written file, a lockfile and a vendored
    /// tree, with the generated lines deliberately dwarfing the real ones — the
    /// shape that makes a line count meaningless.
    ///
    /// Returns `(real lines, generated lines)` so a test can assert against the
    /// numbers the fixture actually wrote rather than a copy of them.
    pub fn generated_and_vendored_commit(&self, epoch: i64) -> (u64, u64) {
        self.write(&self.repo, "src/main.rs", &lines(12, None));
        self.write(&self.repo, "Cargo.lock", &lines(400, None));
        self.write(
            &self.repo,
            "web/node_modules/react/index.js",
            &lines(600, None),
        );
        self.commit_all_at(&self.repo, epoch, "add a dependency and regenerate");
        (12, 1000)
    }

    // -----------------------------------------------------------------------
    // Remotes, upstreams and landing
    // -----------------------------------------------------------------------

    /// Gives the repository an `origin` whose remote-tracking refs are made by
    /// hand, so the fixtures never need a second repository or a network.
    ///
    /// `refs/remotes/origin/HEAD` points at `origin/main`, which is what a real
    /// clone has and what [`standup::git::Git`] looks for first.
    pub fn fake_origin(&self) {
        self.git(
            &self.repo,
            &[
                "config",
                "remote.origin.url",
                self.root.join("origin.git").to_str().unwrap(),
            ],
        );
        self.git(
            &self.repo,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        );
        let main = self.git(&self.repo, &["rev-parse", "refs/heads/main"]);
        self.git(
            &self.repo,
            &["update-ref", "refs/remotes/origin/main", &main],
        );
        self.git(
            &self.repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
    }

    /// A **blobless partial clone** of the primary repository, checked out on a
    /// local branch of `branch` and left tracking `origin/<branch>`.
    ///
    /// The one fixture here with a real promisor remote, which is the only way
    /// to exercise the objects a repository does *not* have. Everything else in
    /// this file is either a plain repository or one with hand-made
    /// remote-tracking refs ([`Fixture::fake_origin`]), and neither can be
    /// lazily filled in — so `tests/read_only.rs` could assert that standup
    /// writes nothing without ever putting it to the test.
    ///
    /// The remote is another directory in the same temp tree, reached over
    /// `file://` with `--no-local` so the transfer really runs `upload-pack`
    /// with a filter rather than hardlinking the object store. Nothing leaves
    /// the machine, and `uploadpack.allowFilter` on the source is what makes the
    /// filter legal.
    ///
    /// What ends up missing is the point: `--filter=blob:none` fetches no blobs,
    /// and the checkouts below then fetch exactly the ones the working tree
    /// needs. Every *superseded* version of a file — anything a diff of an
    /// earlier commit would need — stays absent, which is what a `--numstat`
    /// over a window and a `log -p` over a trunk range both walk into. Build the
    /// history with a file that gets rewritten, or there is nothing to miss.
    pub fn promisor_clone(&self, name: &str, branch: &str) -> PathBuf {
        // Without this the server refuses the filter and the clone is complete,
        // which would make every assertion about missing objects vacuous.
        self.git(&self.repo, &["config", "uploadpack.allowFilter", "true"]);

        let path = self.root.join(name);
        self.git(
            &self.root,
            &[
                "clone",
                "-q",
                "--filter=blob:none",
                // `file://` alone is still a local transport, and a local clone
                // hardlinks the whole object store — filter and all.
                "--no-local",
                &format!("file://{}", self.repo.display()),
                path.to_str().expect("clone path is utf-8"),
            ],
        );
        self.configure_repo(&path);
        self.git(
            &path,
            &["switch", "-q", "-c", branch, &format!("origin/{branch}")],
        );
        path
    }

    /// Publishes `branch` as `origin/<branch>` at the given revision, and points
    /// the local branch at it.
    pub fn publish(&self, branch: &str, revision: &str) {
        let oid = self.git(&self.repo, &["rev-parse", revision]);
        self.git(
            &self.repo,
            &["update-ref", &format!("refs/remotes/origin/{branch}"), &oid],
        );
        self.git(
            &self.repo,
            &["config", &format!("branch.{branch}.remote"), "origin"],
        );
        self.git(
            &self.repo,
            &[
                "config",
                &format!("branch.{branch}.merge"),
                &format!("refs/heads/{branch}"),
            ],
        );
    }

    /// Deletes a remote-tracking ref while leaving the branch configured to
    /// track it — the usual state after a merged branch is cleaned up on the
    /// forge, and the one `%(upstream:short)` keeps lying about.
    pub fn unpublish(&self, branch: &str) {
        self.git(
            &self.repo,
            &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")],
        );
    }

    /// A worktree whose branch has been merged into `main`.
    pub fn merged_worktree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.worktree(name, branch);
        self.write(&path, &format!("{branch}.txt"), "landed\n");
        self.commit_all_at(&path, T_IN1, "work that lands");
        self.git_at(
            &self.repo,
            T_IN2,
            T_IN2,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                &format!("merge {branch}"),
                branch,
            ],
        );
        path
    }

    /// A worktree whose branch has not been merged anywhere.
    pub fn unmerged_worktree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.worktree(name, branch);
        self.write(&path, &format!("{branch}.txt"), "still in flight\n");
        self.commit_all_at(&path, T_IN1, "work that has not landed");
        path
    }

    /// A worktree whose branch was **squash**-merged into `main`, which is the
    /// default on a great many forges.
    ///
    /// Three commits collapse into one, so not one of the branch's patch ids
    /// survives on the trunk and `git cherry` marks every commit `+`. The only
    /// thing left to match is the branch's *combined* diff against the fork
    /// point. The trunk also gains a commit of its own first, so that combined
    /// diff is not simply the whole trunk range — which would make the test
    /// pass for the wrong reason.
    ///
    /// The files are **forty lines long and edited in the middle**, which is
    /// load-bearing rather than decoration: a one-line file is shorter than any
    /// plausible `diff.context`, so every context setting produces byte-identical
    /// output and a test built on one cannot tell a pinned diff option from an
    /// inherited one.
    ///
    /// `main` is left pointing at the squash commit, so a test can name the sha
    /// the plugin is expected to find with `rev-parse main`.
    pub fn squash_merged_worktree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.worktree(name, branch);
        self.write(&path, &format!("{branch}-a.txt"), &lines(40, None));
        self.commit_all_at(&path, T_IN1, "a long file");
        self.write(&path, &format!("{branch}-a.txt"), &lines(40, Some(20)));
        self.commit_all_at(&path, T_IN1 + 60, "edit deep inside it");
        self.write(&path, &format!("{branch}-b.txt"), &lines(40, None));
        self.commit_all_at(&path, T_IN1 + 120, "a second long file");

        self.write(
            &self.repo,
            &format!("trunk-before-{branch}.txt"),
            &lines(40, Some(20)),
        );
        self.commit_all_at(&self.repo, T_IN1 + 180, "unrelated trunk work");
        // `--squash` stages the combined change without recording a merge, so
        // the commit that follows has one parent and no link to the branch.
        self.git(&self.repo, &["merge", "-q", "--squash", branch]);
        self.commit_at(&self.repo, T_IN2, &format!("squash {branch} (#12)"));
        path
    }

    /// A worktree whose branch was **rebase**-merged into `main`: every commit
    /// replayed onto the trunk, so each keeps its patch and none keeps its sha.
    ///
    /// The mirror image of the squash case. Here the individual patch ids are
    /// exactly what survives, and the combined diff of the trunk range does
    /// *not* match the branch, because the trunk moved on first.
    pub fn rebase_merged_worktree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.worktree(name, branch);
        self.write(&path, &format!("{branch}-a.txt"), "first\n");
        self.commit_all_at(&path, T_IN1, "first step");
        self.write(&path, &format!("{branch}-b.txt"), "second\n");
        self.commit_all_at(&path, T_IN1 + 60, "second step");

        self.write(&self.repo, &format!("trunk-before-{branch}.txt"), "trunk\n");
        self.commit_all_at(&self.repo, T_IN1 + 180, "unrelated trunk work");
        // Replaying onto the trunk without moving the branch is what a forge's
        // "rebase and merge" leaves behind locally: the checkout still holds
        // the original shas.
        self.git_at(
            &self.repo,
            T_IN2,
            T_IN2,
            &["cherry-pick", &format!("main..{branch}")],
        );
        path
    }

    // -----------------------------------------------------------------------
    // Working-tree shapes
    // -----------------------------------------------------------------------

    /// Uncommitted work of every kind `status --porcelain=v2` distinguishes:
    /// a modified tracked file, a staged addition, a rename (the `2` record that
    /// consumes two NUL fields), untracked files with awkward names, and ignored
    /// files that must not be counted.
    pub fn dirty_up(&self, cwd: &Path) {
        self.write(cwd, "tracked.txt", "one\ntwo\nthree\n");
        self.write(cwd, "renamed.txt", "movable\n");
        self.commit_all_at(cwd, T_IN1, "files to disturb");

        self.write(cwd, "tracked.txt", "one\nTWO\nthree\nfour\n");
        self.git(cwd, &["mv", "renamed.txt", "renamed-now.txt"]);
        self.write(cwd, "staged.txt", "staged\n");
        self.git(cwd, &["add", "staged.txt"]);
        self.write(cwd, "dir with space/a file.txt", "spaced\n");
        self.write_raw(cwd, b"weird\nname.txt", b"newline\n");
        self.write(cwd, "ignored/artifact.bin", "junk\n");
        self.write(cwd, "build.log", "noise\n");
    }

    /// Makes the index's stat cache stale for every tracked file, which is the
    /// state in which git wants to refresh and write the index back.
    ///
    /// This matters more than it looks. A checkout whose stat cache is *fresh*
    /// gives git nothing to write, so a read-only assertion made against one
    /// passes no matter what the code under test does — that is precisely how
    /// `diff --shortstat` rewriting the index went unnoticed. The two sleeps
    /// straddle git's one-second racy-clean window, so what is measured is a
    /// genuine refresh rather than the race.
    ///
    /// The bytes are rewritten identically: nothing about the *content* of the
    /// checkout changes, only the mtime git cached for it — an ordinary editor
    /// save is enough to produce this in real life.
    pub fn make_stat_cache_stale(&self, cwd: &Path) {
        // Start from an index git is happy with, so every later difference is
        // one the code under test caused.
        self.git(cwd, &["status", "--porcelain"]);
        std::thread::sleep(std::time::Duration::from_millis(1_100));

        let tracked = self.git(cwd, &["ls-files", "-z"]);
        for name in tracked.split('\0').filter(|n| !n.is_empty()) {
            let path = cwd.join(name);
            if let Ok(bytes) = std::fs::read(&path) {
                let _ = std::fs::write(&path, bytes);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1_100));
    }

    /// A worktree stopped mid-merge, with an unmerged index and conflict markers
    /// on disk.
    pub fn merge_in_progress_worktree(&self, name: &str) -> PathBuf {
        // A common ancestor for the file both sides will edit, so the conflict
        // is a modify/modify (`UU`) rather than an add/add. Dated outside the
        // window, because this fixture is about the working tree, not the log.
        self.write(&self.repo, "conflict.txt", "alpha\nbeta\ngamma\n");
        self.commit_all_at(
            &self.repo,
            T_OLD + 7_200,
            "a file for two sides to fight over",
        );

        let source = self.root.join(format!("{name}-source"));
        self.git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("{name}-source"),
                source.to_str().unwrap(),
                "main",
            ],
        );
        self.write(&source, "conflict.txt", "FROM-SOURCE\nbeta\ngamma\n");
        self.commit_all_at(&source, T_IN1, "source edits line 1");
        self.git(
            &self.repo,
            &["worktree", "remove", "--force", source.to_str().unwrap()],
        );

        let path = self.worktree(name, name);
        self.write(&path, "conflict.txt", "FROM-TARGET\nbeta\ngamma\n");
        self.commit_all_at(&path, T_IN1, "target edits line 1");
        let (code, _out, _err) = self.try_git_at(
            &path,
            T_IN2,
            T_IN2,
            &["merge", "--no-edit", &format!("{name}-source")],
        );
        assert_ne!(code, 0, "the fixture merge was supposed to conflict");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `count` numbered lines, with one of them marked.
///
/// Long enough that a diff of it has real context above and below the change,
/// which is what makes a fixture able to tell `-U3` from an inherited
/// `diff.context`. A one-line file cannot.
pub fn lines(count: usize, edited: Option<usize>) -> String {
    (1..=count)
        .map(|n| match edited {
            Some(marked) if marked == n => format!("line {n} EDITED\n"),
            _ => format!("line {n}\n"),
        })
        .collect()
}

#[cfg(unix)]
pub fn bytes_to_path(raw: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(raw))
}

#[cfg(not(unix))]
pub fn bytes_to_path(raw: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(raw).into_owned())
}
