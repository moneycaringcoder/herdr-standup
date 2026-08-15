# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
- An agent's session id appears in the JSON output only.
