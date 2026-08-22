# The JSON contract

`standup --json` and `standup --diff --json` are documented as being for
scripting, which makes their shape an interface rather than an implementation
detail. This file is that interface.

Current version: **1**

Both documents carry it as a top-level `schema` field. One counter for both,
because they come from one binary and one model, and two independently drifting
versions would be a promise nobody could check.

## What a consumer should do

1. **Read `schema` first.** Refuse a value you do not know rather than
   deserialising a shape that happens to fit. standup does this itself: `--diff`
   rejects a saved digest from another schema by name rather than guessing.
2. **Ignore fields you do not recognise.** New fields arrive without a version
   bump, by the rule below.
3. **Tolerate a `kind` you do not recognise.** Every tagged union in the output
   is open to new arms, for the same reason: a new state is usually a *more
   precise* answer than the one it replaces, and a consumer that crashes on it is
   worse off than one that falls through to a default branch.
4. **Do not parse the prose.** `local` and `zone` are for people. `epoch` and
   `offset_seconds` are the same instant for a machine, and `epoch +
   offset_seconds` reproduces `local` exactly.

## What counts as a breaking change

Breaking, and so a version bump:

- removing a field, or renaming one;
- changing a field's type, including making a non-nullable field nullable;
- changing what a field *means* while leaving its name and type alone — the one
  that a schema version exists for, because nothing else can detect it;
- removing a `kind` arm, or changing what an existing arm means;
- changing the top-level shape of either document.

Not breaking, and so no bump:

- adding a field, whether to an object or to one arm of a union;
- adding a `kind` arm;
- adding a note, a problem string, or any other free-text content;
- changing the *order* of keys, or of items within an array whose order is not
  documented below.

Ordering that **is** part of the contract: `repos` is sorted busiest-first, then
alphabetically; `commits` within a checkout are newest first; `checkouts` within
a repository are sorted the same way as `repos`. A change to any of those is
breaking.

## How the version moves

`tests/schema.rs` holds a literal inventory of every path and every `kind` both
documents can produce, built as a union over a digest that exercises every
variant. Any change to the JSON — including one made by accident, in passing,
while doing something else — fails that test, and the failure states this rule.

So a shape change is always a deliberate act, and it takes:

1. updating the inventory in `tests/schema.rs`;
2. for a breaking change, bumping `model::SCHEMA_VERSION`, adding a row to the
   history table below, and adding a `CHANGELOG.md` entry — which is enforced:
   `a_bumped_version_has_to_be_announced` requires the changelog to mention the
   new version, and `the_documented_version_is_the_emitted_one` requires this
   file to agree with the code.

## History

| version | released in | what changed |
|---|---|---|
| 1 | unreleased | The first shape. |

Version 1 has never been published, which is why the additive changes made
before this file existed did not bump it: no consumer could have been holding an
older shape. The rule above applies from the first release onward.

## The documents

`standup --json` emits a **digest**:

```json
{
  "schema": 1,
  "generated_at": { "epoch": 1786835716, "local": "2026-08-15 23:15",
                    "zone": "UTC +0000", "offset_seconds": 0 },
  "window": { "since": { }, "until": null, "source": { "kind": "explicit", "spec": "yesterday" } },
  "repos": [ { "repo_key": "", "name": "app", "repo_root": "", "checkouts": [ ],
               "commits": 0, "churn": { }, "active_days": 0 } ],
  "notes": [ { "severity": "info", "message": "" } ]
}
```

`standup --diff <FILE> --json` emits a **comparison**, which is deliberately not
a digest: it answers "what changed between these two" rather than "what came out
of this window", and it carries no churn and no commit list.

```json
{
  "schema": 1,
  "before": { }, "after": { },
  "repos": [ { "repo_key": "", "name": "app", "commits": 0,
               "checkouts": [ ["/path", { "kind": "advanced", "commits": 2, "landed": false }] ] } ]
}
```

A checkout in a comparison is a `[path, movement]` pair. Path, because it is the
only identity a checkout keeps across two runs: branches get renamed and HEAD
moves constantly.

### The unions

| field | arms |
|---|---|
| `window.source` | `default`, `explicit`, `since_last`, `since_last_first_run`, `rollup` |
| `head` | `branch`, `detached`, `unborn`, `branch_deleted` |
| `tracking` | `not_applicable`, `no_upstream`, `upstream_missing`, `upstream` |
| `landed` | `is_default`, `merged`, `equivalent`, `not_merged`, `unknown` |
| `landed.how` | `every_commit`, `squashed` |
| `unpushed` | `no_remote`, `commits`, `unknown` |
| movement | `appeared`, `advanced`, `landed`, `pushed`, `stalled`, `gone`, `unchanged` |

Three of those distinctions are load-bearing rather than decorative, and a
consumer that collapses them is wrong in the direction that matters:

- `landed: merged` is containment, proved by `merge-base --is-ancestor`;
  `equivalent` is a matching patch, which is what survives a squash or a rebase
  merge. Strong evidence, not proof.
- `unpushed: unknown` is not `commits: 0`. One means "nothing is at risk", the
  other means "I could not find out".
- `landed: unknown` is not `not_merged`. One means "there was nothing to compare
  against", the other means "this did not land".

## Fields worth a note

- `repo_key` is git's `--git-common-dir`, absolute. It is the identity every
  linked worktree of one repository shares, and the right key for grouping.
- `churn.excluded` counts how many of `churn.files` contributed no lines because
  they matched the `ignore` list. A subset of `files`, never an addition to it.
- `session_id` appears in the JSON only. The human and Markdown digests leave it
  out on purpose, because they are written to be pasted somewhere shared.
- `problems` being non-empty means the numbers beside it are incomplete. Every
  renderer is obliged to show it, and a consumer that ignores it will report
  confident wrong totals.
- `offset_seconds` is `null` only where no local zone could be resolved at all,
  which is the same case that makes `local` read `epoch <n>`.
