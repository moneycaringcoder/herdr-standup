# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Tag-triggered release automation. Pushing `vX.Y.Z` runs the full suite on
  Linux and macOS and publishes the GitHub release with notes taken from that
  version's changelog section — but only after an identity gate has confirmed
  that the tag, `Cargo.toml`, `Cargo.lock` and `herdr-plugin.toml` all name the
  same version and that the changelog section for it exists and is not empty.
  The manifest version is the one the marketplace displays and the one easiest
  to forget, so it is checked explicitly.
- An advisory upstream canary. Once a day it resolves one exact herdr `master`
  commit, fetches the API schema herdr generates from its own types at that
  revision, and checks that the two methods standup calls and the snapshot
  fields it reads are all still there. It is scheduled and manual only, it is
  not a required check, and a red canary is a signal to read herdr's recent
  changes rather than a reason to hold a pull request.

### Changed

- `min_herdr_version` is now `0.8.0`, up from `0.7.5`, and the README badge
  agrees. The old floor was reasoned from when the `session.snapshot` fields
  standup reads first appeared; it was never exercised against a 0.7.x server.
  0.8.0 is the latest stable herdr and the only version standup has been
  developed and verified against, so the manifest now states a tested claim
  rather than an inferred one. **Installing on herdr 0.7.5 through 0.7.x, which
  the manifest previously permitted, will now be refused.** If you are on one of
  those and standup worked for you, say so on the issue tracker and the floor
  can come back down with evidence behind it.

## [0.1.0] - 2026-08-16

### Added

- First release. `standup` reports what came out of a time window across every
  herdr workspace: commits with local times, files and lines changed, the
  branch, upstream tracking, whether the work landed on the default branch, and
  uncommitted work still sitting in the checkout.
- Three outputs from one data structure — a terminal report, Markdown for
  pasting, and versioned JSON for scripting.
- Windows: `--since` accepting anything git accepts, `--until`, and
  `--since-last`, which starts from the last digest a human read.
- `--offline` and `--path`, so the digest works from a shell with no herdr
  running.
- Arguments are validated: anything that is not a verb, an option, or an
  option's value is refused by name rather than ignored.

### Fixed before the first release

Found by running the built binary against a live session and against hostile
peers. No released version ever carried these, but each one is recorded because
the reasoning is worth keeping.

- The two `diff --shortstat` calls that measure uncommitted line volume were
  rewriting the user's index. `--no-optional-locks` does not cover `git diff`,
  whose index refresh is not optional; they now run against a copy of the index.
  The read-only test could not catch it because it freshened the stat cache
  before fingerprinting, and a fresh cache is exactly the state in which the
  writeback does not happen.
- A herdr reply with no end-of-line was read until the process died. The framing
  is newline-delimited, so a peer that never sends a newline never stops; the
  real binary grew to 5.3 GB in thirteen seconds and was killed. Responses are
  now bounded at 4 MiB, which is roughly eighty times a live nineteen-pane
  snapshot, and going past it is a named transport failure.
- A workspace whose directory had been removed underneath it reported the same
  checkout twice, once with the kernel's `(deleted)` marker appended, and the
  digest printed the same "is not a git checkout" note twice. The marker is an
  annotation rather than part of the name, so it is stripped and the pair
  collapses.
- A last-run marker that existed but could not be read was announced as "no
  previous run on record" — word for word what a genuine first run says. A first
  run is normal and this is a fault, so it is now a warning in the digest that
  names the file to delete.
- `ensure_date_ref_repo` refused to create the date-reference repository when its
  parent was inside a git repository, which broke the plugin outright for anyone
  keeping their home directory in git: the default state directory then sits
  inside a checkout, and every run with no usable checkout to anchor date parsing
  died. The guard protected nothing — `git init --bare` writes only inside the
  directory it creates — and it is replaced by comparing the resolved git
  directory with the target path, so an enclosing repository can never be
  mistaken for this one.

### Notes on behaviour worth knowing

- Checkouts are discovered from pane working directories as well as herdr's own
  worktree records. Only a minority of workspaces carry a worktree record even
  when they are sitting in a repository, so the records alone are not enough.
- The window is resolved to an absolute instant before any `git log` runs.
  `git rev-parse --since=<garbage>` exits 0 and answers "now", which would
  otherwise render as a quiet day.
- Commits are collected with `--since-as-filter` rather than `--since`, because
  `--since` prunes the walk and loses commits in a history with out-of-order
  committer timestamps.
- "Merged" means merged into the default branch, not into the upstream tracking
  branch. Upstream ahead/behind is reported separately.
- `--since today` means different things on different gits, which is why
  `midnight` is the default and why `today` is answered with a warning. Through
  git 2.54 `today` resolves to *now*, so it asks for nothing; git 2.55 changed it
  to the local midnight. Both are handled, and neither is mistaken for an
  unparseable window.
- An agent's session id appears in the JSON output only.
