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
- A **generated or vendored** path is treated exactly like a binary one: counted
  as a file, contributing no lines. Lines are the number that means "effort",
  and a regenerated lockfile is tens of thousands of them nobody wrote. Which
  paths those are is configuration rather than plumbing — see `Ignored` in
  `src/config.rs` and the `ignore` key in the README — and the count of them is
  carried on `Churn::excluded` so the digest can say `3 files (2 generated)`
  rather than printing a line total quietly smaller than the diff.
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

## What exists only here

```sh
git -C <wt> --no-optional-locks remote
git -C <wt> --no-optional-locks rev-list --count HEAD --not --remotes
```

Reachable from HEAD, reachable from no remote-tracking ref: the commits a
`worktree remove` would take with it. This is a different question from the
`ahead` count above, and asking it needs saying why, because `ahead` looks like
the same number.

`ahead` is measured against the **one** configured upstream. It answers nothing
at all when there is no upstream — which is precisely the branch whose every
commit is only here, the case worth reporting most. `--remotes` spans every
remote instead, so a branch pushed to a fork, or to a second remote, or under a
different name, is correctly not counted as at risk. What is being asked is "is
this anywhere else", not "is this on its upstream".

The `remote` call first is not a formality. Without it, a repository with no
remote configured counts its whole history as unpushed — literally true and
useless, since there was never anywhere to push it. A digest that files every
local-only scratch repository under "work at risk" buries the case this exists
to surface, so that state is named separately and reported as nothing at risk.

A count that cannot be read is recorded **twice**: as the state's reason, and as
a problem on the report. That is not belt and braces. A checkout with no count
has nothing to put on its `unpushed:` line, so without the problem beside it the
checkout reads as quiet — and a quiet repository is summarised to its name, then
dropped entirely when quiet ones are excluded. The failure would be invisible in
exactly the digest that needed it.

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

### Exit 1 is not "did not land"

Containment is exact for a fast-forward and for a merge commit, and it is the
wrong question for a squash merge or a rebase merge: both rewrite the commit, so
the sha the checkout holds never appears on the trunk. Squash merging is the
default on a great many forges, so an exit of 1 is not the answer — it is where
two more probes start.

Measured on git 2.53.0, against a three-commit branch squash-merged onto a trunk
that had gained a commit of its own since the fork point, and against the same
branch replayed commit by commit:

| probe | squash merge | rebase merge |
|---|---|---|
| `merge-base --is-ancestor` | exit 1 | exit 1 |
| `git cherry main topic` | `+ + +` | `- -` |
| combined diff patch id | `450e3211…` → `6df5ff43…` on `main` | no match |

Neither probe alone covers both shapes, so both are run, cheapest first:

```sh
git -C <wt> --no-optional-locks merge-base <head> <default>
git -C <wt> --no-optional-locks cherry <default> <head>
git -C <wt> --no-optional-locks diff-tree -p -U3 --src-prefix=a/ --dst-prefix=b/ \
    --no-renames --no-textconv <base> <head> | git patch-id --stable
git -C <wt> --no-optional-locks log -p -U3 --src-prefix=a/ --dst-prefix=b/ \
    --no-renames --no-textconv --no-merges --format='commit %H' \
    <base>..<default> | git patch-id --stable
```

`git cherry` prints `-` for a commit whose patch is already upstream and `+` for
one that is not, so nothing but `-` means every commit arrived by another route.
That is what a rebase merge leaves, and what a squash of a single commit leaves.
A squash of more than one commit destroys every individual patch id, and the id
that does survive is the branch's **combined** diff against the fork point —
which is exactly the diff the squash commit carries. Unrelated histories have no
fork point, `merge-base` says so with exit 1, and neither probe runs.

### The diff options are pinned, and that is the load-bearing part

`git patch-id` hashes the diff it is handed, so two diffs of the same change
hash differently when they were produced with different options. The two
commands above do not read the same configuration: **`diff-tree` is plumbing**
and takes git's basic diff config, **`log` is porcelain** and takes the UI
config on top of it. Since standup deliberately does not scrub `HOME` or
`GIT_CONFIG_GLOBAL`, that is the reader's own `~/.gitconfig`.

Measured on git 2.53.0, replaying these exact invocations against a
three-commit branch squash-merged onto a moved-on trunk. With an empty global
config the squash is found. With any one of these in the reader's config, only
the `log` side moved, and every squash merge on that machine went back to
reading as "not merged" — this bug, restored by a setting that has nothing to do
with merging:

| config | unpinned verdict | pinned by |
|---|---|---|
| `diff.noprefix = true` | `NotMerged` | `--src-prefix`, `--dst-prefix` |
| `diff.srcPrefix`, `diff.dstPrefix` | `NotMerged` | `--src-prefix`, `--dst-prefix` |
| `diff.mnemonicPrefix = true` | `NotMerged` | `--src-prefix`, `--dst-prefix` |
| `diff.context = 5` | `NotMerged` | `-U3` |
| `diff.renames = copies` | `NotMerged` | `--no-renames` |
| `format.pretty` | ids attributed to nothing | `--format='commit %H'` |

Two more that are not about symmetry:

- **`--stable`** hashes each file independently, so an id does not depend on the
  order git happened to emit the files in. `diff-tree` and `log` need not agree
  on that order.
- **`--no-merges`**, because a merge commit prints a header and no diff under
  `log -p`, which would hand the next commit's patch to the wrong sha. It is the
  same hazard `--format` guards from the other side.

`tests/git_collect.rs` writes every one of those settings into a fixture's own
repository config and asserts the squash is still found, so the pinning cannot
be dropped without a red test — and the test uses forty-line files edited in the
middle, because a one-line file is shorter than any plausible `diff.context` and
would produce identical output either way.

### Read-only

`diff-tree` and `log -p` compare commits, so unlike `diff --shortstat` they
never read the working tree or refresh the index and they need no
`GIT_INDEX_FILE` copy. `patch-id` is a filter and reads only its stdin, which is
buffered and written to it rather than joined by a pipe — that keeps the deadline
and the pipe draining in one place, and it means the source's exit status is
checked *before* the sink ever runs. Piped, a source that died partway would
leave `patch-id` exiting 0 over a truncated stream, and "the ids I found do not
include yours" must never be mistaken for "the patch is not there".

`tests/read_only.rs` runs the whole pipeline under its fingerprint: the
kitchen-sink fixture carries a squash-merged worktree precisely for that.

Two known limits, both filed rather than fixed here, because both are about
cost or about a repository shape rather than about the answer:

- Buffering the diff is an allocation with no bound on it. `log -p` over a trunk
  range is the largest thing this module asks git for — roughly 175 KiB of patch
  text per commit, so an 800-commit range is 140 MB — and a stale branch on a
  busy trunk pays that per checkout. The natural fix is the caching in the
  roadmap's "Cache plumbing results" entry rather than a second pipe here.
- In a `--filter=blob:none` or treeless **partial clone** the blobs a diff needs
  are absent, and git's answer is to fetch them from the promisor remote and
  write them into `.git/objects` — measured on git 2.53.0, a blobless clone went
  from 8 object files to 12 after one `log -p` over a trunk range. That is a
  write and a network call from a plugin that promises neither. It is not new
  with these probes: `log --numstat` has always needed the same blobs. It has
  its own issue.

### Three answers, not two

A match is reported as its own state and never as `Merged`. Two commits with the
same diff have the same patch id, so this is strong evidence and not proof, and
the digest says which of the two it holds: `merged into main` for containment,
`on main by patch as <sha>, not by sha` for a squash, and `every commit is on
main by patch, not by sha` for a rebase.

And a probe that could not be *asked* is not an answer either. "The patch is not
on the trunk" and "the probe failed" are the same `NotMerged` if they are allowed
to share a return, which would make a shallow clone, a corrupt pack or a refused
promisor fetch render identically to work that genuinely never landed. So
anything but a clean exit from `cherry`, `diff-tree`, `log` or `patch-id`
becomes `merge status unknown` with the command and its stderr attached — the
same discipline the missing-default-branch case already had.

The cost is the range `git cherry` already walks; it builds a patch-id map of
the upstream side to do its own job, so the second probe is the same order of
work rather than a new one. Both are bounded by the module's per-invocation
deadline, which records a problem rather than guessing.

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
