# Contributing to standup

Contributions are genuinely welcome — bug reports, questions, documentation
fixes, and code. This document exists so you know what to expect before you
spend time on something, not to put obstacles in front of you.

The project is maintained by one person. That means review is attentive but not
instant, and it means every change is read carefully before it lands. Please
don't take questions on a pull request as resistance; they are how the
maintainer stays confident in code that runs against other people's
repositories.

## The rules that matter

**standup is read-only against user repositories.** It runs on machines full of
in-flight, uncommitted, unpushed agent work. A bug that loses someone's work is
categorically worse than a bug that prints a wrong number.

So any change touching `src/git.rs` must keep these true:

- every git invocation passes `--no-optional-locks`
- every git invocation runs with `GIT_NO_LAZY_FETCH=1`
- nothing is ever staged, and `GIT_INDEX_FILE` is never pointed at a real index
- no command creates an object, mutates a ref, or touches the working tree

And one thing that is not obvious: **`--no-optional-locks` does not cover `git
diff`.** Diff's index refresh is not optional, so it rewrites the index — and
takes `index.lock` — whenever a tracked file's stat data is stale. The
`--shortstat` calls run against a copy of the index for exactly this reason. If
you add a `diff`, it needs the same treatment. `docs/git-plumbing.md` has the
measurement.

A second one that is not obvious: **reading a partial clone writes to it.** A
`--filter=blob:none` clone has no blobs for the diff, and git fetches the
missing ones from the promisor remote and stores them — measured as 8 object
files becoming 24, from one `log --numstat`. `GIT_NO_LAZY_FETCH=1` refuses that,
at the cost of the line counts and the merge status on such a clone, which are
then reported as unavailable and never as zero or `not merged`.

`GIT_NO_LAZY_FETCH` arrived in git 2.37 and an older git ignores it, so on such
a git the diff must not be asked for at all rather than set-and-hope. That is
what `Git::unrefusable_promisor` is for, and why `commits` and `landing` both
take it.

`tests/read_only.rs` enforces it by fingerprinting the index bytes, working
tree, refs, reflogs and object count before and after a full run. If your change
makes that test fail, the test is right and the change is wrong.

**A wrong number must be impossible to mistake for a quiet day.** This plugin's
whole failure mode is the silent one. `git rev-parse --since=nonsense` exits 0
and answers "now"; a digest built on that is empty, correctly formatted, and a
lie. Every degradation in this codebase is therefore either a loud error or a
rendered note — never a fallback that resembles a normal empty result. Keep it
that way.

**The JSON shape is an interface.** `--json` and `--diff --json` are documented
for scripting, so their shape is somebody else's dependency.
`docs/json-schema.md` says what counts as a breaking change and what does not,
and `tests/schema.rs` pins every path and every `kind` both documents can
produce. If that test fails, you changed the interface — deliberately or in
passing — and the failure message says which paperwork the change needs. A
version bumped in code without the documentation and changelog to match fails
too, on purpose: a version nobody can look up is not a version.

## Getting set up

```sh
git clone https://github.com/moneycaringcoder/herdr-standup
cd herdr-standup
cargo build --release
herdr plugin link .          # note: `link` does NOT run the build step
```

Rebuild by hand after every change, since `herdr plugin link` deliberately
skips the `[[build]]` hook.

You can run the binary directly, with or without herdr:

```sh
./target/release/standup                              # today, every workspace
./target/release/standup --markdown --since yesterday
./target/release/standup --offline --path . --json    # no socket at all
```

Before opening a pull request:

```sh
cargo fmt --all
cargo clippy --all-targets --locked -- -D warnings
cargo test --all
```

CI runs these on a matrix of five rows: Linux and macOS, arm64 and x86_64, and
three different runner images, plus one row that installs git from
`ppa:git-core/ppa` rather than taking the image's. If your local Rust is older
than CI's, clippy will pass locally and fail there — `rustup update stable`
first if in doubt.

**Every row currently runs git 2.55.0**, measured, including the PPA one: the
images have converged on the newest stable release. So the matrix buys operating
system and architecture coverage today, not git-version coverage. The PPA row is
there to *diverge* the moment a newer git ships, which is when it starts paying
for itself — git 2.55 redefined `--since today` from the current instant to local
midnight, and no distro shipped a git new enough to have caught that in advance.
Covering an *older* git, which is where the `--since-as-filter` fallback and the
pre-2.55 reading of `today` live, needs a pinned build rather than a runner
image; there is an issue for it.

**When a matrix row goes red, read which step failed first.** `tests/git_contract.rs`
runs on its own, before everything else, and asserts what *git* does with no
standup code in the way. A failure there means the environment changed under the
plugin, and the place to start is `docs/git-plumbing.md`, which is where the
claim it broke is written down. A failure in any later step, with the contract
green, means the plugin is wrong. Working that out from a red build used to cost
more than fixing either.

Two rules for anything added to `git_contract.rs`: no standup code, and every
assertion message names what depends on the behaviour, so the failure tells the
next reader where to look rather than only that a number moved.

No test requires a running herdr. The fixtures build throwaway git repositories
in a temp directory, and the socket tests replay a captured real snapshot.

## What makes a change easy to merge

**A test that fails before your fix and passes after it.** This matters more
here than in most projects, because the bugs this plugin attracts are
*invisible* ones: a wrong answer with no error, which looks exactly like a
correct answer.

**Tests built from observed behaviour, not assumed behaviour.** A sibling
plugin's socket client once passed its whole suite while being wrong, because
the test's fake server replied with the shape the client expected rather than
the shape herdr actually sends. If you are testing against herdr or git, capture
real output first — `herdr api snapshot`, a real fixture repository — and encode
that. `tests/snapshots/` holds a structurally faithful capture with the personal
details replaced; keep new fixtures the same way.

**Numbers a reader can check by hand.** Every figure in the digest should be
reproducible with one git command. If you add one, say in a comment which
command it corresponds to.

**Verification against something real.** If a change affects what a user sees,
run it against a live herdr session with several workspaces and say what you
observed. A passing suite is necessary and not sufficient.

**Comments that say why, not what.** The code is full of small, load-bearing
decisions that look arbitrary until explained — why the window is resolved to an
absolute epoch before any `log` runs, why "merged" means the default branch
rather than the upstream, why `--json` does not advance the `--since-last`
marker. If your change encodes a decision like that, leave the reason behind.

## What to expect from review

- Small fixes — a typo, a clear bug with a test, a documentation correction —
  are usually merged quickly and without ceremony.
- Changes to the output get discussed, because the output is the product. Paste
  a before and after.
- Larger features are best raised as an issue first, so you don't build
  something the project then declines. "Would you take a PR that does X?" is
  always a fine question, and a fast answer is more useful to you than a
  thorough review of work that was never going to land.

The maintainer reviews every pull request personally and may make small edits
on merge rather than sending a change back for a one-line fix.

## Scope

standup deliberately does a narrow thing well: it reports facts about what came
out of a time window, and leaves the interpreting to whoever reads it.

In scope: correctness against unusual repository states, clearer output, more
useful windows, performance on sessions with many workspaces, better
documentation, Linux and macOS.

Out of scope, and why:

- **Writing to repositories in any way.** The read-only guarantee is the reason
  the plugin is safe to run against live agent work.
- **Anything requiring a network call.** No pull-request status, no CI results,
  no GitHub API, no telemetry, no update checks. Everything is local git and the
  herdr socket. This is what makes the digest instant and usable offline.
- **Summarising what the work *meant*.** No LLM calls, no generated prose.
  standup reports facts; a human or an agent reading the output can interpret
  them, and that separation is what makes the output trustworthy.
- **Windows.** Not refused on principle, but the socket layer and path handling
  are Unix-shaped and there is no way to test it here. A well-tested
  contribution would be considered.

## Reporting bugs

Please include the output of `standup --json`, your `herdr --version`, your
`git --version`, and what you expected to see instead.

Redact freely — paths, branch names and commit subjects can be sensitive, and a
report with `/home/you/repos/app` in it is just as useful.

## Security

Please don't open a public issue for a security problem. See
[SECURITY.md](SECURITY.md).

## Licence

By contributing, you agree that your contributions are licensed under the MIT
Licence, the same terms that cover the project.
