# Roadmap

Ideas for future work, roughly in the order they would most improve the plugin.
None of this is committed to a release, and nothing here is a promise.

The standard everything below is held to is the one in the README: **every number
is one git command away from being checked by hand, and none of them is a guess.**
A feature that cannot meet that does not belong here.

## Correctness

### Exclude generated and vendored paths from line counts

Lines added and removed are a proxy for effort, and a regenerated lockfile or a
vendored directory destroys it. An ignore list, defaulting to the obvious cases,
would make the number mean what readers already assume it means.

## Windows and coverage

### Cover the paths the first cross-platform run exposed

The first CI run on macOS found three failures, none of them faults in the plugin,
and one of them a real upstream change: git 2.55 redefined `--since today` from
the current instant to local midnight. That class of thing is only found by
running somewhere new. Broadening the matrix is how the next one gets found before
a user does.

## More ways to read it

### Weekly and monthly rollups

The window options answer "what happened today". The same data answers "what
happened this month" if it is aggregated rather than listed, and that is the
version a person forwards to someone else.

### Diff two digests

`--since-last` starts from the last digest a human read. The natural next question
is what changed between two of them, which is a different report from a longer
window.

### Slack and HTML output

Markdown for pasting and JSON for scripting already exist. Slack's mrkdwn is not
Markdown, and the paste currently degrades. HTML would suit an emailed weekly
rollup.

### Optional grouping by agent

Grouping by repository is deliberate: grouping by time would put two commits from
one branch on opposite sides of an unrelated project's commits. Agent grouping has
the same hazard and should stay opt-in — but "what did shear-classifier do this
week" is a real question the data can answer.

## Operating it

### `--fail-if-empty`

For cron and CI use, where a digest with nothing in it should be a signal rather
than an empty message posted to a channel.

### Cache plumbing results

Large repositories re-walk the same history on every run. Caching by head sha
would make a repeated digest cheap without ever serving a stale answer for a
checkout that moved.

## Interfaces

### An explicit timezone in the JSON

Commit times are rendered in local time, which is right for a person reading a
digest and ambiguous for a machine consuming one. The JSON should carry the zone
rather than leave the consumer to assume.

### A versioned JSON schema

The JSON is documented as being for scripting. A schema version, and a changelog
entry when it moves, is what makes that safe to rely on.
