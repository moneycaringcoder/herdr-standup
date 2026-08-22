# Roadmap

Ideas for future work, roughly in the order they would most improve the plugin.
None of this is committed to a release, and nothing here is a promise.

The standard everything below is held to is the one in the README: **every number
is one git command away from being checked by hand, and none of them is a guess.**
A feature that cannot meet that does not belong here.

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
