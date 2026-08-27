# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- CI covers a git **older** than the two versions the collector's behaviour
  turns on. The matrix spans six rows, and the first run of the previous five
  measured all of them as git 2.55.0 — the images have converged on the newest
  stable release, so the version-sensitive code was tested on exactly one
  version and the branch that handles old git was dead weight as far as CI was
  concerned.

  No hosted image ships an old enough git any more, and `ubuntu-22.04` — which
  would have been 2.34 — begins deprecation on 2026-09-17, so the new row builds
  **git 2.36.6** from the kernel.org tarball and caches it by version. 2.36 sits
  below both lines that matter: 2.37 brought `--since-as-filter` and
  `GIT_NO_LAZY_FETCH`, and 2.55 redefined `--since today` from the current
  instant to the local midnight. One row therefore exercises the `--max-age`
  fallback, the problem it records, the partial-clone path that cannot rely on
  the environment variable, and the pre-2.55 reading of `today`, all of which
  were previously assumed.

  Running the suite against that git found two things and changed a third:

  - `Fixture::unborn_worktree` used `worktree add --orphan`, which is **git
    2.42**, putting a floor under the whole suite that nothing else needed. The
    same state is now built by hand — HEAD pointed at a branch that does not
    exist, no per-worktree `logs/HEAD`, and the 65-byte empty index — and
    verified against what `--orphan` produces on 2.53.0.
  - The tests whose *correct answer* differs either side of 2.37 now assert both
    answers rather than the modern one: below it the digest is the pruned lower
    bound and says so in a note, every walked checkout therefore reads as
    `Broken`, and a quiet day exits 0 with that note rather than 2 with
    "Nothing landed". `fixtures::git_version` is the one place that reads the
    version, and `since_as_filter` the one place that names 2.37.
  - `today_is_accepted_whichever_instant_this_git_thinks_it_means` branched on
    what git *did*, so a git that changed its mind would have silently taken the
    other branch and left this one untested. It now pins which reading the
    version owes, and both branches are live in CI.

### Changed

- Repository and by-agent rollups now use one set-based union of commit ids,
  local days, and touched paths, replacing quadratic membership scans in long
  windows while keeping every rendered and JSON total unchanged.

### Fixed

- Git timeouts now terminate the invocation's entire process group, so a
  credential helper, wrapper, or other descendant retaining git's output pipes
  cannot keep a report hung after the direct child is killed.

- Command-line parsing now rejects valueless flags carrying `=...`, refuses to
  consume another option as a missing value, and no longer accepts the
  undocumented no-op `--quiet`; malformed invocations cannot silently produce a
  plausible report for a different request.

- Herdr actions now open their matching visible overlay pane instead of sending
  the report only to the capped background-command log. All seven report
  formats remain one-shot and copyable from the pane.

- Herdr responses now fail closed unless their id matches the request and a
  `session.snapshot` carries the required workspace, pane, and agent arrays.
  Protocol drift can no longer look like an ordinary empty session.

- Atomic plugin-state replacement now syncs the containing directory after
  rename, so a power loss cannot discard the durable name of a completed
  `--since-last` marker or plumbing-cache write.

- Concurrent human-readable reports now serialize their `--since-last` marker
  updates and keep the greatest completed epoch, so a slower older run cannot
  move the next window backwards after a newer run finishes.

- Release publication now proves the tag commit is reachable from freshly
  fetched `main`, both in the identity job and immediately before publication;
  a version-consistent tag on an unmerged branch is refused.

- A **partial clone is no longer written to, or fetched from,** while reporting.
  In a `--filter=blob:none` or treeless clone the blobs a diff needs are not in
  the repository, and git's answer is to fetch them from the promisor remote and
  write them into `.git/objects`. Measured on git 2.53.0 against a real blobless
  clone: **8 object files before one `log --numstat`, 24 after** — a write to a
  user's repository and a network call, from a plugin whose two standing
  promises are that it does neither.

  This was not new with the landing probes; `git log --numstat` has wanted the
  same blobs since the first release. Every invocation now runs with
  `GIT_NO_LAZY_FETCH=1`, which refuses the fetch, and the same measurement shows
  zero objects written.

  **This makes two numbers unavailable on a partial clone, where they used to
  appear**, and someone will notice:

  - **Line counts.** The commits are still reported in full — a missing blob
    must not turn a busy day into an empty one — and the churn beside them reads
    zero with a problem naming the object git could not read.
  - **Merge status**, where the squash and rebase probes need a patch from the
    trunk range that the clone does not have. It reads
    `merge status unknown: …`, carrying git's own words, rather than
    `not merged` — which would be a wrong answer rather than an absent one.

  Everything else is unaffected: the commit list, the uncommitted counts and
  line volume, the tracking numbers and the at-risk count all read objects the
  checkout already has, and were measured writing nothing.

  `GIT_NO_LAZY_FETCH` is itself a git 2.37 feature, and an older git ignores it:
  measured on 2.36.6 against the same clone, with the variable set, **nine
  object files written**, exit 0, no warning. A guarantee that depends on the
  reader's git being new enough is not a guarantee, so below 2.37 the diff is
  not asked for at all on a partial clone — and the note then names the remote
  it declined to reach for, and why, since nothing was read to name. A version
  string that cannot be parsed counts as old, because the two ways of being
  wrong cost different things: a line count, or somebody's repository.

  `tests/read_only.rs` now enforces this instead of asserting it. Its fixtures
  had no promisor remote, so there was nothing to lazily fetch and the guarantee
  was untestable by construction; it now builds a real blobless clone of a
  second repository in the temp tree over `file://` with `--no-local`, so the
  transfer runs `upload-pack` with a filter, and fingerprints the clone before
  and after a full run. A negative control runs the same log *without*
  `GIT_NO_LAZY_FETCH` and asserts the fingerprint catches the pack arriving, so
  a clone that happened to be complete cannot make the test vacuous.

## [0.1.1] - 2026-08-22

### Added

- The JSON shape is a written, enforced contract:
  [`docs/json-schema.md`](docs/json-schema.md). It states what a consumer should
  do — read `schema` first, ignore unknown fields, tolerate an unknown `kind`,
  never parse the prose — and what counts as a breaking change: something
  removed, renamed, retyped, or quietly given a new meaning. That last one is
  what a schema version is really for, since nothing else can detect it.

  `standup --diff --json` now carries `schema` too. It did not, which meant half
  the output documented for scripting gave a consumer no way to refuse a shape it
  did not know. One counter for both documents, because they come from one binary
  and one model.

  The promise is kept mechanically rather than by good intentions.
  `tests/schema.rs` pins a literal inventory of every path and every `kind` both
  documents can produce, built as a union over a digest that exercises every
  variant, so a field renamed in passing fails the build with the rule in the
  message. Bumping the version without the documentation and changelog to match
  fails as well: a version nobody can look up is not a version.

  The version stays at 1. It has never been published, so no consumer can be
  holding an older shape — which is also why the additive changes above did not
  move it. The rule applies from the first release onward.

- Every timestamp in the JSON carries `offset_seconds`: the seconds east of UTC
  that its `local` string was rendered in. `zone` stays as it was — prose, for a
  header — and this is the same fact as a number, so a consumer no longer has to
  parse `CEST +0200` or assume.

  It is per timestamp rather than once per digest, because a window can span a
  daylight-saving change: a `--monthly` digest of March legitimately holds stamps
  at `+0100` and `+0200`, and a single zone field at the top would be wrong for
  half of them. `null` only where no local zone could be resolved at all, which
  is the same case that makes `local` read `epoch <n>` — an absent offset and an
  offset of zero are not the same answer.

  `epoch + offset_seconds` reproduces `local` exactly, asserted under six zones
  including the half-hour and three-quarter-hour ones, with the calendar
  arithmetic written a second time in the test so one bug cannot produce both
  sides of the comparison. The human digests are unchanged, which is also
  asserted. `SCHEMA_VERSION` stays at 1, as with the earlier additive changes,
  since the roadmap versions the schema last.

- The landing probes are cached, keyed on the shas that determine them. On a
  purpose-built repository — 800 trunk commits, twelve worktrees, 16.5 MB of
  patch text — a run went from 3.64 s to 1.49 s cold and 0.39 s warm, with a
  byte-identical digest each time.

  Two keys. The verdict is keyed on the head sha, the trunk's name and the
  trunk's sha; the expensive part — the patch ids of the trunk range, which is
  where `git log -p` spends 175 KiB per commit — is keyed on the fork point and
  the trunk sha, which is the same key for every worktree branched from one
  commit of a trunk that has not moved. That is why even the first run is faster:
  twelve worktrees share one walk instead of repeating it.

  There is no expiry and nothing to invalidate. Both probes are pure functions of
  shas given the pinned diff options, so anything that could change an answer
  moves a sha, and a moved sha is a different key: a checkout that moved cannot
  be served a stale verdict. A failed probe is never stored, because git failing
  once must not become permanent, and the file carries a version that is bumped
  whenever the probes change, so an answer produced by code that no longer exists
  is discarded rather than trusted.

  The cache is invisible: a hit and a miss produce the same report, which is
  asserted rather than assumed. The tests prove hits by seeding an answer git
  cannot produce for that repository and requiring the report to carry it, and
  prove misses the same way round.

- `--fail-if-empty`, which exits **2** when there is nothing to report, so a
  cron line can decline to post rather than sending an empty message. The digest
  still prints; the status is for the caller.

  2 rather than 1 because a failure already exits 1, and a caller that cannot
  tell the two apart will either stay silent on the day something breaks or page
  somebody about a quiet Sunday.

  "Nothing" is deliberately wider than "no commits": uncommitted work, an
  untracked file, a commit that exists only here, a branch deleted under a live
  checkout, a count that could not be read — none of those are silence, and all
  of them exit 0. They are the cases most worth posting. With `--diff` the
  comparison is what would be posted, so a comparison where nothing moved exits
  2 even when the digest underneath has commits in it. Asserted by running the
  real binary against throwaway repositories, one case per claim.

- `--by-agent`, which groups by agent rather than by repository. Opt-in, and it
  will stay that way: it interleaves unrelated projects, which is the hazard
  repository grouping exists to avoid, and the per-agent totals do not add up to
  the digest's.

  They cannot. Agents are placed per checkout and a commit's author identity is
  shared by every agent on the machine, so a commit that reaches two groups is
  counted in both. There are two routes: two agents sharing one checkout, and
  two checkouts of one repository landing in different groups, because worktrees
  share history — the second needs only one agent per checkout and was found by
  running the grouping against a live session rather than reasoned about. So the
  difference is measured against the digest's own total and printed as a number
  ("these totals add up to 53 commits more than the digest's") rather than
  described as a caveat, and both routes are covered by a test.

  Each group's numbers are recomputed over exactly the checkouts that agent
  occupied, by the union rule the ungrouped digest uses, never inherited from the
  repository. Work herdr could not attribute is reported as `no agent reported`
  rather than dropped. `--json` is unchanged by the flag: it carries a schema
  version, and a second arrangement wearing the same version is how a consumer
  gets quietly broken.

- `--slack` and `--html`, and a `--format` that accepts `slack` and `html`.
  Both carry the same numbers as the other formats, which is asserted rather
  than assumed: the totals, the churn, the uncommitted counts and the unpushed
  count are extracted from all four renderings and compared, because a format is
  a rendering and never a different answer.

  **Slack's mrkdwn is not Markdown.** Pasting the Markdown digest degraded in
  four specific ways, each verified against Slack's own documentation:
  `**bold**` renders literally because mrkdwn's bold is a single asterisk;
  `- item` renders literally because mrkdwn has no list syntax at all;
  `[text](url)` renders literally because mrkdwn links are `<url|text>`; and
  `&`, `<`, `>` are interpreted and have to arrive as entities. So `--slack`
  bolds with one asterisk, draws bullets with literal `•` and `◦`, and escapes
  exactly those three characters and **only** those three — mrkdwn has no escape
  character, so a backslash in front of ordinary punctuation is a backslash a
  reader sees, and `feat/re[factor]` arrives with its brackets intact.

  `--html` is written for an email client rather than a browser: every style
  inline, because Gmail and Outlook strip `<style>` and `<link>`; nothing to
  fetch, because remote content is blocked by default and a blocked resource is
  worse than an absent one; layout by table, which is what Outlook renders
  predictably; and `&`, `<`, `>` and `"` always escaped, the fourth because paths
  and branch names are interpolated into `style` attributes and a quote inside
  one ends the attribute. Both new formats also render `--diff` comparisons.
  The plugin manifest gains an action for each.
- `--diff <FILE>`: compares a digest saved by an earlier `--json` run with the
  one this run collects, and reads as a **comparison rather than a longer
  digest**. No churn, no line volume, no commit list — those are what a digest
  answers. This answers what moved: `3 new since`, `landed since`, `pushed
  since; 2 no longer only here`, `no new commits, still holding 2 unpushed and
  uncommitted work`, `gone, and was holding 4 that were only there`, `new here`.

  The stalled reading is the comparison's own finding and the reason it is worth
  having: each digest on its own reports that state plainly, and neither says it
  has not moved. Findings a reader has to act on are marked — `!` in the
  terminal, bold in Markdown — and new work sorts above them, because burying
  the commits somebody opened the report to see would be the same mistake as
  burying a busy repository under quiet ones.

  Checkouts are matched by **path**, the only identity a checkout keeps across
  two runs, since branches get renamed and `HEAD` moves constantly. Commits are
  matched by sha, so a rebase between the two runs reads as new work — which it
  is, in the sense that matters here. Nothing about a comparison touches git: it
  is a pure function of two digests, so it says exactly what they said and
  cannot quietly consult the disk for a third answer.

  The JSON is now an input as well as an output, so the model types read it back
  as well as write it — one shape rather than a second one maintained beside it.
  The saved digest's `schema` is checked before anything else and a version this
  binary does not know is refused by name, because a comparison built on a
  misread digest would be confidently wrong about what moved. `--diff` never
  advances the `--since-last` marker: what changed between two digests is not "a
  digest a human read".
- `--weekly` and `--monthly`: the same data, aggregated rather than listed. The
  window options answer "what happened today"; these answer "what happened this
  month", which is the version somebody forwards. The commit lines go, the totals
  stay, and each repository gains the number a long window needs — `over 4 active
  days`, because "37 commits" is a very different month depending on whether it
  was four days or one. A daily digest does not print it, where the answer is
  always one.

  Both boundaries are **calendar** rather than rolling: "the last thirty days" is
  a different question from "this month". The week starts on Monday, which is ISO
  8601's answer rather than the locale's, so the same command means the same
  window on two machines. Both are computed from `localtime_r` rather than handed
  to git's approxidate parser, which cannot be asked for a calendar boundary
  exactly, and both are printed as an absolute instant so the boundary can be
  checked rather than trusted. Verified across UTC, the half-hour zones, +14 and
  −3:30, and across a DST transition. A rollup sets its own window, so `--since`,
  `--until` and `--since-last` are refused alongside it by name rather than one
  of them quietly winning.
- A broader CI matrix, and a red row that says which kind of thing broke. Five
  rows now — Linux and macOS, arm64 and x86_64, three runner images — plus a row
  that takes git from `ppa:git-core/ppa` rather than the image, so a git newer
  than any distro ships is exercised the day it lands. git 2.55 redefined
  `--since today` from the current instant to local midnight and nothing in CI
  would have caught it in advance. Measured on the first run: all five rows are
  currently git 2.55.0, so the matrix buys operating-system and architecture
  coverage today rather than git-version coverage, and the PPA row is there to
  diverge when the next release appears.
- `tests/git_contract.rs`, a test target that asserts what **git** does with no
  plugin code in the way: the `diff` index writeback that `--no-optional-locks`
  does not cover, `--max-age` pruning where `--since-as-filter` does not,
  `rev-parse --since=<garbage>` exiting 0 and answering now, `patch-id` agreeing
  between plumbing and porcelain under the pinned diff options,
  `rev-list --not --remotes`, the binary `--numstat` spelling, and
  `symbolic-ref` still being unable to tell an unborn branch from a deleted one.
  Every one is a claim `docs/git-plumbing.md` already made; this makes the notes
  executable. It runs first and on its own, so a failure there means the
  environment moved and a failure after it means the plugin is wrong — a
  distinction that cost more than fixing either the first time round. Each row
  also prints its git version and what that git makes of `today`, `midnight` and
  garbage before anything can fail.
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
