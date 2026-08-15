<!--
Thanks for contributing. Nothing here is meant to be a hurdle — delete any
section that does not apply. A one-line typo fix needs a one-line description.
-->

## What this changes

<!-- What the change does, and why. If it fixes an issue, link it. -->

## How it was verified

<!--
Which of these you did. The suite passing is necessary but often not
sufficient: this plugin's characteristic bug is a wrong answer with no error,
which looks exactly like a quiet day until you check a number by hand.
-->

- [ ] `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings` are clean
- [ ] `cargo test --all` passes
- [ ] There is a test that fails without this change
- [ ] Ran against a live herdr session, with what I observed described below
- [ ] Checked at least one number in the output against the git command that
      produces it

<!-- If it changes what a user sees, paste the before and after. -->

## Read-only guarantee

<!--
Only relevant if you touched src/git.rs or anything that shells out to git.
Delete this section otherwise.
-->

- [ ] `tests/read_only.rs` still passes
- [ ] No new git invocation writes to the index, working tree, refs, or object
      store, and every one passes `--no-optional-locks`

## Silent failure

<!-- Delete if the change cannot fail partially. -->

- [ ] Anything that can go wrong produces a loud error or a rendered note, and
      never an empty result that resembles a quiet day
