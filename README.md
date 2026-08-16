<div align="center">

<img src="docs/img/logo.svg" alt="" width="96" height="96">

# standup

**A digest of what your agents actually did. One command, one readable summary of every workspace
over a time window — commits, change volume, branch, and whether the work landed anywhere.**

[![CI](https://github.com/moneycaringcoder/herdr-standup/actions/workflows/ci.yml/badge.svg)](https://github.com/moneycaringcoder/herdr-standup/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![herdr](https://img.shields.io/badge/herdr-%E2%89%A5%200.7.5-8b949e.svg)](https://herdr.dev)
[![read-only](https://img.shields.io/badge/your%20repos-never%20written%20to-2da44e.svg)](#it-never-writes-to-your-repositories)

</div>

```
standup — since 2026-08-15 00:00 UTC +0000 (generated 23:15)

  6 commits  ·  5 files  +11 −1  across 4 repositories

  lanternfish                                      6 commits  ·  5 files  +11 −1
    wip/salvage                 ~/code/lanternfish/wt-salvage
      ! the branch wip/salvage was deleted underneath this checkout
      no commits in this window, merge status unknown: HEAD has no commit, so
        nothing can have landed on origin/main, no branch to track
      uncommitted: 4 files changed, 1 untracked, +6 −0
    fix/media-fetch-throughput  ~/code/lanternfish/wt-throughput
      agents: kestrel (opencode), wren (claude)
      4 commits, 4 files, +10 −1, not merged into origin/main, in sync with
        origin/fix/media-fetch-thro…
      uncommitted: 1 file changed, 1 untracked, +1 −0
        09:04  2eccdd14  Batch media fetches behind a semaphore
        08:31  9c4fdd36  Add a throughput regression test
        07:58  da24c3f2  Drop the per-item retry loop
        06:12  6952a747  Bump the media timeout to 30s
    main                        ~/code/lanternfish
      authors: Ada Bexley
      3 commits (1 merge), 4 files, +6 −0, on the default branch origin/main, 2
        ahead of origin/main
        06:12  6952a747  Bump the media timeout to 30s
        05:40  30eb8d7f  Merge chore/deps
        05:40  641ab749  Pin serde to 1.x
    spike/av1                   ~/code/lanternfish/wt-spike
      authors: Ada Bexley
      1 commit, 3 files, +5 −0, merged into origin/main, no upstream
        06:12  6952a747  Bump the media timeout to 30s

  quiet: brambleway, quillmark, tidepool
```

Four things in that screen are the reason the plugin exists. `wip/salvage` has a
branch that was **deleted underneath a live checkout**, with six lines of
uncommitted work still in it. `fix/media-fetch-throughput` is **in sync with its
own remote and still not merged** into the trunk — published, not landed.
`spike/av1` **has landed** and is safe to remove. And three repositories were
**quiet**, which is stated rather than left to inference.

## Why

After a day of running several agents across several worktrees, working out what came out of it means
visiting each workspace and running `git log` by hand — and the branches you most need to check are
the ones whose workspace you already closed.

`standup` answers it in one command. It asks herdr where the agents are, asks git what happened
there, and prints the result grouped by repository. It reports facts and stops: no pull-request
status, no CI, no generated prose. What the work *meant* is for whoever reads it.

## What it tells you, per checkout

- the commits in the window, with local times
- files touched, lines added and removed
- the branch, or that HEAD is detached, unborn, or pointing at a branch someone deleted underneath it
- whether it has an upstream, and how far ahead or behind
- **whether the work landed** on the repository's default branch
- uncommitted work still sitting there, which is the difference between "the agent did nothing" and
  "the agent did a day of work and never committed it"
- which agent was in the workspace, when herdr reports one

Every number is one git command away from being checked by hand, and none of them is a guess.

## Markdown you can paste

`standup --markdown` produces the same digest ready for a standup channel or a commit-day journal.
It is a nested bullet list rather than a table, because a `|` in a branch name breaks a Markdown
table and Slack does not render tables at all:

```markdown
**standup** — since 2026-08-15 00:00 UTC +0000 (generated 23:15)

6 commits, 5 files, +11 −1 across 4 repositories.

- **lanternfish** — 6 commits, 5 files, +11 −1
  - `wip/salvage` — `~/code/lanternfish/wt-salvage` — no commits in this window — merge status unknown: HEAD has no commit, so nothing can have landed on origin/main — no branch to track
    - **problem:** the branch wip/salvage was deleted underneath this checkout
    - uncommitted: 4 files changed, 1 untracked, +6 −0
  - `fix/media-fetch-throughput` — `~/code/lanternfish/wt-throughput` — 4 commits, 4 files, +10 −1 — not merged into origin/main — in sync with origin/fix/media-fetch-throughput
    - agents: kestrel (opencode), wren (claude)
    - `09:04` `2eccdd14` — Batch media fetches behind a semaphore
    - `08:31` `9c4fdd36` — Add a throughput regression test
  - `spike/av1` — `~/code/lanternfish/wt-spike` — 1 commit, 3 files, +5 −0 — merged into origin/main — no upstream
    - `06:12` `6952a747` — Bump the media timeout to 30s

Quiet: brambleway, quillmark, tidepool.
```

Branch names, paths and subjects are escaped or fenced on the way out, so a branch called
`feat/re[factor]` or a commit subject that is itself a Markdown heading still renders.

## JSON for scripting

`standup --json` emits the whole digest with a schema version, so a script can refuse a shape it does
not know. It is the only output that carries an agent's session id — the human and Markdown digests
deliberately leave it out, because they are written to be pasted somewhere shared.

```json
{
  "schema": 1,
  "generated_at": { "epoch": 1786835716, "local": "2026-08-15 23:15", "zone": "UTC +0000" },
  "window": {
    "since": { "epoch": 1786752000, "local": "2026-08-15 00:00", "zone": "UTC +0000" },
    "until": null,
    "source": { "kind": "explicit", "spec": "2026-08-15T00:00:00" }
  },
  "repos": [
    {
      "name": "lanternfish",
      "checkouts": [
        {
          "path": "~/code/lanternfish/wt-salvage",
          "is_linked_worktree": true,
          "head": { "kind": "branch_deleted", "name": "wip/salvage" },
          "commits": [],
          "dirty": { "tracked_changed": 4, "untracked": 1, "conflicted": 0,
                     "insertions": 6, "deletions": 0 }
        }
      ]
    }
  ]
}
```

Every timestamp is an `{epoch, local, zone}` triple, so a script gets the machine value and a human
gets an instant that cannot be misread as UTC.

## Windows

`--since` accepts anything git accepts, and the default is local midnight:

```sh
standup                          # since local midnight
standup --since yesterday
standup --since '3 days ago'
standup --since 2026-08-01 --until 2026-08-08
```

One thing worth knowing, because it catches people: git's date parser reads
`today` as *the current instant*, not as the start of the day. `midnight` is the
spelling that means 00:00 local, and it is the default. `--since today` gets a
warning rather than a blank digest.

`--since-last` is the one that gets used daily. It starts from the last digest you read, so nothing
is counted twice and nothing is missed:

```sh
standup --since-last
```

The marker is recorded by the human-readable and Markdown runs only. A `--json` run is a script
reading, and moving the marker there would silently steal the window out from under your next digest.
The first `--since-last`, with no marker on record, falls back to today's window **and says so** —
"you asked for today" and "this is the first run, so I fell back to today" are different answers to
"why is this empty".

Every window is resolved to an absolute instant before any `git log` runs, and the header states it
in local time with the zone spelled out. That is not decoration:

```
$ git rev-parse --since=bogusgarbage
--max-age=1786831294        # exit 0, and that number is "now"
```

git's date parser answers *now* for anything it cannot understand, with a successful exit status. A
digest built on that is empty, correctly formatted, and a lie. `standup` detects it and refuses.

## How it works

<img src="docs/img/pipeline.svg" alt="herdr's session snapshot and your checkouts feed a pipeline: candidate directories, identify, sibling worktrees, window resolution, per-checkout collection, grouping by repository, then the three renderers." width="100%">

One `session.snapshot` over the herdr socket, then read-only git per checkout. Two details are worth
knowing because they are where the obvious implementation goes wrong:

**Workspaces are found by pane directory, not by herdr's worktree records.** `workspace.worktree` is
present only for workspaces herdr itself opened as a repository or a worktree. In the live session
this plugin was built against, nine of ten workspaces were sitting in git checkouts and only three
carried that record — the rest were ordinary directories that happen to be repositories, including
the one this plugin was written in. So the candidate list is the union of tracked checkout paths and
every distinct pane `cwd`, and git decides which are checkouts.

**Sibling worktrees are included.** An agent that finished and had its workspace closed still produced
commits today, and that is exactly the work a strictly per-workspace view loses. `--no-siblings`
turns it off.

A checkout with nothing in the window is **summarised as quiet**, never dropped. A repository that
could not be read is reported loudly, at the top. The one thing this plugin must never do is let a
failure look like a slow day.

## It never writes to your repositories

Every git invocation is read-only and passes `--no-optional-locks`, so it cannot contend with an
agent's own git commands — plain `git status` takes `index.lock` to write back its stat cache, and
this never does. Nothing is staged, no object is created, no ref is touched.

`tests/read_only.rs` proves it rather than asserting it: it fingerprints the index bytes and mtime,
the working tree, every ref, the reflogs, the loose-object inventory and the pack list of a fixture
repository before and after a full run, and fails on any difference — including while another process
holds `index.lock`. It also carries a **negative control**: a sixth check runs a plain `git status`
*without* `--no-optional-locks` and asserts the fingerprint does fail, so the strict test cannot
quietly stop testing anything. That distinction is measurable — plain `status` advances the index
mtime while leaving its byte length identical, which is why mtime is fingerprinted separately.

There are also **no network calls at all** — no GitHub API, no telemetry, no update check. The digest
works on a plane.

## Install

```sh
herdr plugin install moneycaringcoder/herdr-standup
```

For local development:

```sh
git clone https://github.com/moneycaringcoder/herdr-standup
cd herdr-standup
cargo build --release
herdr plugin link .          # note: `link` does NOT run the build step
```

The plugin adds actions and two overlay panes; nothing is written to your herdr `config.toml`, and
there is no daemon to enable. It is a report, not a monitor.

| Action | What it does |
|---|---|
| Standup: today | Everything since local midnight |
| Standup: since the last one | Everything since the last digest you read |
| Standup: yesterday and today | A two-day window, for the morning after |
| Standup: Markdown to paste | The same digest as Markdown |
| Standup: JSON snapshot | Machine-readable; does not move the `--since-last` marker |

It also works fine from a shell, with or without herdr running:

```sh
standup --offline --path ~/repos/app --path ~/repos/api
```

## Options

```
--report              Human-readable digest (default)
--markdown            The same digest as Markdown
--json                The same digest as JSON

--since <WHEN>        Anything git accepts (default: midnight, local)
--until <WHEN>        End of the window (default: now)
--since-last          Start from the last digest you read

--path <DIR>          Also report this checkout, whether or not herdr knows it
--offline             Report only --path directories; never touch the socket
--busy                Hide repositories with nothing in the window
--no-siblings         Only checkouts a workspace is sitting in
--max-commits <N>     Commits listed per checkout before the rest are summarised
```

## Configuration

Optional, at `~/.config/herdr/plugins/config/moneycaringcoder.standup/config.json`. Every key is
optional and a malformed file is ignored with a warning rather than being fatal.

```json
{
  "since": "midnight",
  "format": "text",
  "max_commits": 10,
  "include_quiet": true,
  "git_timeout_seconds": 20
}
```

## Three decisions you might disagree with

**"Merged" means merged into the default branch**, not into the upstream tracking branch, even when
they differ. "Did it land?" is a question about the trunk; a topic branch pushed to its own remote
branch has been *published*, not landed, and that is reported separately as upstream tracking. When
no default branch can be identified the answer is "unknown, and here is why" — never a bare "not
merged", which reads as a verdict.

**The digest groups by repository, not by time.** Grouping by time would put two commits from one
branch on opposite sides of an unrelated project's commit. Time still orders everything within a
group.

**An agent's session id appears in the JSON only.** It is a useful pointer for following up, and it
has no business being pasted into a shared channel. The agent's *name* appears everywhere, because
"shear-classifier landed three commits" is the sentence that makes a digest worth reading. No
transcript content is ever read or printed.

## Development

```sh
cargo fmt --all
cargo clippy --all-targets --locked -- -D warnings
cargo test --all
```

No test needs a running herdr: the fixtures build throwaway git repositories in a temp directory, and
the socket tests replay a captured real snapshot. See [CONTRIBUTING.md](CONTRIBUTING.md), and
[`docs/herdr-protocol.md`](docs/herdr-protocol.md) and
[`docs/git-plumbing.md`](docs/git-plumbing.md) for the verified behaviour both layers are built on.

## Licence

MIT. See [LICENSE](LICENSE).
