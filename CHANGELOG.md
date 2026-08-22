# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A broader CI matrix, and a red row that says which kind of thing broke. Five
  rows now: Linux and macOS, arm64 and x86_64, and four different builds of git
  including one deliberately newer than any distro ships — git 2.55 redefined
  `--since today` from the current instant to local midnight, and the only way
  to meet that class of change before a user does is to run a git nobody has
  tried yet. `tests/git_contract.rs` is a new test target that asserts what
  **git** does, with no plugin code in the way: the `diff` index writeback that
  `--no-optional-locks` does not cover, `--max-age` pruning where
  `--since-as-filter` does not, `rev-parse --since=<garbage>` exiting 0 and
  answering now, `patch-id` agreeing between plumbing and porcelain under the
  pinned diff options, `rev-list --not --remotes`, the binary `--numstat`
  spelling, and `symbolic-ref` still being unable to tell an unborn branch from
  a deleted one. It runs first and on its own, so a failure there means the
  environment moved and a failure after it means the plugin is wrong — a
  distinction that cost more than fixing either the first time round. Each row
  also prints its git version and what that git makes of `today`, `midnight` and
  garbage, before anything can fail.
- **Generated and vendored paths no longer distort the line counts.** Lines added
  and removed are a proxy for effort, and one regenerated lockfile destroys it:
  a `pnpm-lock.yaml` churns tens of thousands of lines nobody wrote. Paths
  matching the new `ignore` list are still counted as **files touched** — the
  commit really did touch them, which is exactly how a binary file has always
  been treated — and contribute nothing to the line totals. The exclusion is
  shown rather than silently applied: the digest reads
  `3 files (2 generated), +12 −0`, and `--json` carries `churn.excluded` beside
  `churn.files` so a script knows the line totals are not the whole diff. The
  default list is the obvious cases and nothing clever — dependency lockfiles,
  `vendor/`, `node_modules/`, `third_party/`, `.yarn/`, `target/`, `dist/`,
  `build/`, `.next/`, `.svelte-kit/` — with no `*.json`, no `*.lock` and no
  "looks generated" heuristic, because a wrong exclusion is worse than a missing
  one: it silently shrinks a real number. An `ignore` list in the config file
  replaces the default rather than adding to it, so the defaults can be both
  extended and got rid of, and `"ignore": []` gives back the raw diff. Three
  pattern shapes, documented in the README: a bare name matches the basename at
  any depth, a trailing slash matches a directory at any depth, and `*` never
  crosses a `/`.
- **Committed but unpushed work is its own state.** The digest already separated
  "the agent did nothing" from "the agent did a day of work and never committed
  it"; the state between them — committed here, on no remote, gone with the
  directory — was left to be inferred from an `ahead` count that is not even
  reported when there is no upstream configured, and a checkout holding nothing
  else was filed as quiet, summarised down to its repository name, and dropped
  altogether when quiet repositories were excluded. It now reads as
  `unpushed: 2 commits on no remote`, on its own line beside `uncommitted:`,
  and such a checkout is never called quiet. The question asked is
  `git rev-list --count HEAD --not --remotes` — reachable from HEAD and from no
  remote-tracking ref — so work pushed to a fork or a second remote is correctly
  not counted as at risk, and a branch with no upstream is answered rather than
  skipped. A repository with **no remote at all** is reported as having nowhere
  to push rather than as holding its whole history at risk, because filing every
  local-only scratch repository under "at risk" would bury the case worth
  reading. A count that cannot be read is a named problem, never a reassuring
  zero.
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

### Fixed

- Work that shipped through a **squash merge or a rebase merge** is no longer
  reported as not landed. "Did it land?" was answered with
  `git merge-base --is-ancestor` alone, which is exact for a fast-forward and
  for a merge commit and asks the wrong question of a rewritten commit: both a
  squash and a rebase leave the trunk carrying a sha the checkout has never
  seen. Squash merging is the default on a great many forges, so on most
  repositories this was every branch that ever shipped. standup now looks for
  the patch as well — `git cherry` for a branch replayed commit by commit, and
  the patch id of the branch's combined diff against the fork point for a
  branch squashed into one, because a squash destroys every individual patch id
  and the combined one is all that survives. A matching patch is strong
  evidence and not proof, since two commits with the same diff share a patch
  id, so it is reported as its own state rather than folded into "merged": the
  digest reads `on main by patch as 6df5ff43, not by sha`, and `--json` carries
  a new `landed.kind` of `equivalent`, with `landed.how.kind` saying which probe
  answered and `landed.how.oid` naming the trunk commit that matched. Nothing
  that was already exact changed; a branch only partly cherry-picked onto the
  trunk still reads as not merged; and a probe that could not be *run* — a
  shallow clone, a missing object — is reported as `merge status unknown` with
  the command and its stderr, never as a verdict. The two diffs a patch id is
  computed from are produced with the diff options pinned explicitly, because
  `diff-tree` reads git's basic diff config while `log` also reads the UI config:
  unpinned, a reader's own `diff.noprefix`, `diff.context` or `diff.srcPrefix`
  silently reinstated the whole bug.
- Agents are credited to the checkout they actually worked in. herdr reports
  agents per workspace, and a workspace is not a place — its panes can sit in
  different checkouts — so a workspace-scoped roster credited every agent with
  work in every directory the workspace touched, and two agents in one window
  collapsed into one because attribution was deduplicated by display name.
  `agent` is `claude` on all but one row of a live nineteen-pane capture and
  `name` is absent on three of eighteen, so two agents reading as one was the
  normal case rather than the exotic one. Each agent now carries its own
  directory, from the `cwd` its row already had, and is placed by it; two agents
  in one checkout are two agents, and a repeated label is counted rather than
  repeated, as `claude ×2`. Where herdr does not say which directory an agent was
  in **and** its workspace spans more than one checkout, the answer is
  unknowable: it is credited to none of them and the digest says so, naming the
  workspace and the count. An agent with no directory whose workspace touches a
  single checkout is still placed there, because there is nowhere else it could
  have been. `--json` gains an `agents` array on each checkout, which is the
  attribution; the existing `workspaces[].agents` stays the workspace roster.

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
