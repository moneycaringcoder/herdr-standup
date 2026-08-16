# git plumbing notes (verified on git 2.53.0, Linux)

Working notes for `src/git.rs`. Every command here was run against a real
repository or a purpose-built fixture, and the surprising results are recorded
with the experiment that produced them.

## Hard rules

1. Always pass `--no-optional-locks`. Plain `git status` takes
   `<gitdir>/index.lock` to write back its stat cache; with the flag it does
   not. standup runs against repositories where an agent may be mid-commit.
2. **`--no-optional-locks` does not cover `git diff`.** See below — this one cost
   a real bug.
3. Never stage anything, never point `GIT_INDEX_FILE` at a real index, never run
   a command that creates an object. The `--shortstat` copy below is the one
   sanctioned exception, and it is a copy precisely so that the rule holds.
4. Set `LC_ALL=C`. Several of the strings parsed below are only stable in the C
   locale.
5. Resolve the `git` binary explicitly. herdr runs plugin commands with **no
   shell and a minimal `PATH`**.

## The flag does not cover `diff`

`git diff` refreshes the index and writes it back, and that refresh is **not**
optional: neither `--no-optional-locks` nor `GIT_OPTIONAL_LOCKS=0` suppresses it.
Both only suppress git's *optional* writeback, which is what `status` does.

Measured on git 2.53.0 against one committed file, `touch`ed so its stat data is
stale and nothing else:

```
baseline index md5                                          791c22ab…
env GIT_OPTIONAL_LOCKS=0 git --no-optional-locks diff --shortstat
                                                            cb788797…   rewritten
env GIT_OPTIONAL_LOCKS=0 git --no-optional-locks status --porcelain=v2 -z -uall
                                                            unchanged
```

It fires whenever a tracked file's stat data is stale — an ordinary editor save
of identical content is enough, as is `sed -i`, a formatter, or a build step —
and it takes `index.lock`, so it can collide with an agent's own `git add` in
the same checkout. No data is lost, but the promise is broken and the lock
contention is real.

So the two `--shortstat` invocations in `Git::dirty` run with `GIT_INDEX_FILE`
pointed at a **copy** of the per-worktree index. The copy absorbs the writeback,
the real index stays byte-identical, and the copy is deleted afterwards. This is
the one sanctioned use of `GIT_INDEX_FILE` in the crate.

This was missed for a while because `tests/read_only.rs` freshened the stat cache
before fingerprinting, and a fresh cache is exactly the state in which the
writeback does not happen. The test now makes the cache **stale** on purpose.

## Repo identity

```sh
git -C <path> --no-optional-locks rev-parse --path-format=absolute \
    --show-toplevel --git-common-dir
```

Two lines from one process: the checkout root and the repository's common
directory. All worktrees of one repository share the common directory; each has
its own `--git-dir`. Canonicalize before comparing, since symlinked or
bind-mounted roots otherwise yield two identities for one repository.

Observed live, and the reason `--git-common-dir` is the identity rather than the
path:

```
/home/…/code/orchard                             -> /home/…/code/orchard/.git
/home/…/.herdr/worktrees/orchard/fix-slow-fetch  -> /home/…/code/orchard/.git
/home/…/code                                     -> fatal: not a git repository
```

A directory that is not a repository is ordinary data — most sessions have at
least one workspace in one. It is reported as a skipped line, never as an error
and never silently.

## Resolving a window

This is the single most dangerous thing the plugin does.

```sh
git rev-parse --since=<spec>     # prints --max-age=<epoch>
git rev-parse --until=<spec>     # prints --min-age=<epoch>
```

git's approxidate parser accepts `midnight`, `yesterday`, `2 days ago`,
`2026-08-01`, `@1700000000`, and much else. **It also accepts anything at all**:

```
$ git rev-parse --since=bogusgarbage
--max-age=1786831294         # exit 0 — and that is the current time
$ git rev-parse --since=
--max-age=1786831294         # exit 0 — likewise
```

There is a second, quieter surprise in the same parser: **through git 2.54,
`today` means *now*, not the start of the day.** Measured at 22:09 local on
2.53.0:

```
--since=today      -> --max-age=1786834145   # the current second
--since=midnight   -> --max-age=1786752000   # 00:00 local
--since=12am       -> --max-age=1786752000
```

**git 2.55 changed this**, and `today` there means the local midnight after all.
Caught by CI on a 2.55.0 macOS runner, where `today` came back 49137 seconds
before now — midnight to the second — while 2.54.0 on the same run still
answered with the current second.

So the spelling is ambiguous across the versions people actually have, which is
the whole reason `midnight` is standup's default and `--since today` is answered
with a warning rather than an empty digest: almost nobody typing it means
"nothing", and on half the gits in the wild that is exactly what it would mean.
Both readings keep `today` on `SPECS_MEANING_NOW` in `src/git.rs` — on 2.54 and
older because it does land on now, and on 2.55 and newer harmlessly, because it
no longer reaches the check that list guards.

An unparseable window silently becomes "since now", which produces a digest that
is empty, correctly formatted, and wrong. There is no error to notice and no
difference from a genuinely quiet day. `Git::resolve_date` therefore compares the
resolved epoch against the current time and rejects a spec that lands on "now"
unless the spec actually says so, and `tests/git_collect.rs` pins it.

Resolving eagerly has a second benefit: `git log` is given an absolute instant
rather than the user's fuzzy phrase, so the window stated in the header is
exactly the window git used.

`git rev-parse` **refuses to run outside a repository** (`fatal: not a git
repository`, exit 128), including with `--git-dir` pointed at a nonexistent path.
When a session has no checkouts at all, standup creates an empty bare repository
under its own state directory purely as a parsing context, so the window is
validated even then.

## `--since` prunes the walk, and the pruning loses commits

`--since` / `--max-age` is a **traversal cutoff, not a filter**. The walk stops
at the first commit older than the cutoff, so any history whose committer
timestamps are out of order hides everything behind that commit. That is not an
exotic state: a rebase, a cherry-pick with `--committer-date-is-author-date`, or
a machine whose clock was corrected all produce it, and an agent's day of work
frequently ends in a rebase.

Measured on a six-commit fixture with two backdated commits, three of which were
genuinely inside the window:

| flag | commits reported |
|---|---|
| `--max-age=<epoch>` | **1 of 3** |
| `--since-as-filter=<date>` | 3 of 3 |

`--since-as-filter` (git 2.37+) applies the same comparison without pruning.
standup uses it, and falls back to `--max-age` on older git with the degradation
recorded as a problem on the report — a digest that quietly drops two thirds of
the day is precisely the failure this plugin exists to prevent.

`--min-age` (`--until`) did not prune in any arrangement tested, so it is passed
as-is.

## Commits in the window

One invocation per checkout:

```sh
git -C <wt> --no-optional-locks log -z --since-as-filter=<since> [--min-age=<until>] \
    --numstat --no-renames \
    --format=%x1e%H%x1f%P%x1f%an%x1f%ct%x1f%s
```

- The window filters on **commit** date, so `%ct` is the field to display. Using
  `%at` would file a rebased commit under a day the window never covered.
- **Framing.** A `--format` record cannot be split on a chosen separator alone:
  a commit subject is arbitrary bytes, and a fixture subject containing a literal
  `\x1e` really does break a naive `\x1e`-split. `-z` terminates each format
  record and each numstat path with a NUL; `%x1e` at the *start* of the format
  then only has to distinguish a header field from a numstat field, and only the
  first byte of a NUL-delimited field is ever consulted. A separator inside a
  subject becomes harmless. `-z` also means paths arrive as raw bytes rather
  than C-quoted, so a filename containing a space, a newline or a non-UTF-8 byte
  survives.
- `--no-renames` keeps the numstat path column single-valued. Without it a
  rename prints a three-field form that a naive parser mangles.
- A **binary** file prints `-\t-\t<path>`: count the file, add nothing to the
  line totals.
- A **merge** commit prints no numstat at all. It is detected from the parent
  count in `%P`, counts toward the commit total, and contributes nothing to
  churn — a merge introduces no new work.
- `Churn.files` is the size of the **union** of touched paths. Summing
  per-commit file counts double-counts a file edited twice, which for an agent's
  day of small commits is not a rounding error.

Timestamps are rendered with `crate::clock`, which formats through
`localtime_r` — the same zone database git formats `--date=format-local`
against, so the two agree by construction.

## Working tree state

```sh
git -C <wt> --no-optional-locks status --porcelain=v2 -z --untracked-files=all
```

`-z` disables path quoting, so paths are raw bytes. The framing rule naive
parsers get wrong: a `2` (rename/copy) record consumes **two** NUL-terminated
fields — the new path, then the original path as the very next field. A `u`
record is an unmerged path; standup counts those separately, because a checkout
with a merge in progress is a state worth naming.

## Upstream tracking

```sh
git -C <wt> --no-optional-locks for-each-ref \
    --format='%(upstream:short)' refs/heads/<branch>
git -C <wt> --no-optional-locks rev-list --left-right --count <upstream>...HEAD
```

`rev-list --left-right --count` prints `behind<TAB>ahead` as two integers. Do
**not** parse `%(upstream:track)`: it is prose (`[ahead 2, behind 1]`) and not
something to build on.

An upstream that is configured but does not resolve — the usual cause is a
remote branch deleted after a merge — is reported as its own state, distinct
from having no upstream at all.

## Did it land

```sh
git -C <wt> --no-optional-locks symbolic-ref -q --short refs/remotes/origin/HEAD
git -C <wt> --no-optional-locks merge-base --is-ancestor <head> <default>
```

`merge-base --is-ancestor` exits 0 when HEAD is contained in the default branch
and 1 when it is not; anything else is an error and is reported as such. Where
`refs/remotes/origin/HEAD` is missing — common in a clone made without it, and in
every `git init` — the candidates in `config::DEFAULT_BRANCH_CANDIDATES` are
tried in order.

If no default branch can be identified, the verdict is **unknown, with the
reason**, never a bare "not merged". The two mean very different things to
somebody deciding whether a branch is safe to delete.

## Degenerate states

| case | detection |
|---|---|
| detached HEAD | `worktree list` reports `detached`; `symbolic-ref -q HEAD` fails |
| unborn branch | `HEAD` does not resolve **and** the worktree has no `logs/HEAD` |
| branch deleted underneath | `HEAD` does not resolve **but** the worktree has a non-empty `logs/HEAD` |
| not a repository | `rev-parse` exits 128 |

`symbolic-ref -q HEAD` does **not** distinguish unborn from deleted-branch: on
git 2.53.0 it exits 0 and prints the same ref name in both cases. The
discriminator that works is the worktree's own HEAD reflog — a checkout that ever
had a commit checked out has `logs/HEAD`, a freshly initialised one does not.
The two are worth telling apart because one is a brand-new repository and the
other is somebody's work about to be lost.

## Enumerating worktrees

```sh
git -C <wt> --no-optional-locks worktree list --porcelain -z
```

Records are separated by an empty NUL field. `worktree <abs-path>` is always
first; after that, do not assume ordering. `bare` records have no `HEAD` and no
`branch`, and `prunable` records may point at a directory that no longer exists.
Both are skipped. `locked` and `detached` are fine to report on.
