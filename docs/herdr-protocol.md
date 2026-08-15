# herdr socket notes (verified against herdr 0.8.0, protocol 19)

Working notes for `src/herdr.rs`. Everything here was checked against a live
0.8.0 server with ten workspaces and nineteen panes, not inferred from
documentation. Only the parts standup actually depends on are covered.

## Transport

`HERDR_SOCKET_PATH` is injected into every command herdr spawns. Fall back to
`$XDG_CONFIG_HOME/herdr/herdr.sock` only for hand invocation, and treat an
empty-string environment variable as unset — herdr injects empty strings for
absent context rather than omitting the variable.

Framing is newline-delimited JSON. Not length-prefixed. There is no `jsonrpc`
field.

```
request : {"id":"<string>","method":"<name>","params":{...}}\n
success : {"id":"<string>","result":{"type":"<snake_case>",...}}\n
failure : {"id":"<string>","error":{"code":"<string>","message":"<string>"}}\n
```

`id` must be a string. `params` is mandatory and must be an object — send `{}`
for methods that take no parameters, never `null`.

**The socket answers one request per connection and then sends EOF.** Every call
must be able to reconnect and retry once. That is the normal path rather than an
error path, and it is also what carries a client across `herdr update
--handoff`, where the first attempt lands on a socket the old server has just
unlinked. Retry only on a transport failure; retrying a rejected request just
gets it rejected again and double-counts against herdr's own error accounting.

## The one method standup calls

### `session.snapshot` — params `{}`

Returns flat sibling arrays joined by ID:

```
result.type          "session_snapshot"
result.snapshot      version, protocol,
                     focused_workspace_id, focused_tab_id, focused_pane_id,
                     workspaces[], panes[], agents[], tabs[], layouts[]
```

**The arrays are one level down, under `snapshot`.** Reading them off `result`
yields nothing at all, which looks exactly like an idle session. A missing
`snapshot` key is therefore a hard error in this client, never an empty list.

## The finding that shaped this plugin

`workspace.worktree` is **not** how you find the repositories in a session.

It is present only for workspaces herdr itself opened as a repository or as a
worktree. In the ten-workspace capture this plugin was built against, **nine
workspaces were sitting in git checkouts and only three carried a `worktree`
key**. The other six were plain directories the user had opened, which happen to
be repositories — including the one this plugin was written in. A digest built
from `worktree` alone would have reported one repository and silently omitted
two thirds of the day's work.

Reproduce it against your own session with `herdr api snapshot`: count the
workspaces with a `worktree` key, then run `git rev-parse --show-toplevel` in
each pane `cwd` and count again.

So candidate directories are the union of:

1. `workspaces[].worktree.checkout_path`, when present; and
2. every distinct `panes[].cwd` for that workspace.

and git decides which of them are checkouts. `identify` returning "not a repo"
is ordinary data, not a failure.

### Use `cwd`, never `foreground_cwd`

Both exist on a pane. `foreground_cwd` follows whatever the foreground process
is doing, and in the live capture it pointed at

```
/home/…/mise/installs/node/24.18.0/lib/node_modules/pyright/dist
```

for a pane whose actual working directory was a repository. `cwd` is the
pane's own directory and is the one to use.

### Fields, as observed

```
workspaces[]  workspace_id, number, label, focused, pane_count, tab_count,
              active_tab_id, agent_status, tokens?, worktree?
worktree      repo_key, repo_root, checkout_path, repo_name, is_linked_worktree
panes[]       pane_id, terminal_id, workspace_id, tab_id, focused, revision,
              agent_status, cwd?, foreground_cwd?, agent?, tokens?
agents[]      pane_id, tab_id, workspace_id, agent, agent_status,
              agent_session?, name?, cwd?, terminal_title?
```

`agents[].agent_session` is an **object**, not a string:

```json
{"agent":"claude","kind":"id","source":"herdr:claude",
 "value":"5e69d906-7bf6-4b0b-ba88-e51dc1f25e5b"}
```

The id is in `.value`. `agents[].name` is the user's own label for the agent
(`shear-classifier`); `agents[].agent` is the program (`claude`, `opencode`).
Both may be absent.

## What standup deliberately does not call

`worktree.list` returns `{source, worktrees[]}` and is the only place herdr
carries a **branch name**. standup does not use it, for two reasons:

1. It errors with `not_git_worktree` for any workspace herdr does not track as a
   repository — which, per the finding above, is most of them.
2. `git` already answers the question for every checkout, tracked or not, and is
   authoritative. In the same capture, a workspace whose directory was
   `.../orchard/fix-slow-fetch` was checked out on the branch
   `fix/tier-promotion-scope`. Never infer a branch from a directory name.

standup also sets no badge tokens and claims no panes. It is a report, not a
monitor: one snapshot per invocation, no daemon, nothing to enable or disable,
and nothing left behind in the sidebar.

## Plugin execution environment

Commands are argv arrays run with **no shell**, cwd = plugin root, and a minimal
`PATH` — `git` must be resolved explicitly rather than assumed. Plugins run on
the **server** host, so anything shelled out to must exist there.

`herdr plugin link .` does **not** run `[[build]]`; `herdr plugin install` does.
Build manually during local development.

`HERDR_PLUGIN_STATE_DIR` and `HERDR_PLUGIN_CONFIG_DIR` are injected. The
fallbacks in `src/config.rs` resolve to the same paths herdr uses, so a
`--since-last` marker written by a plugin action is the one a hand-run binary
reads. A fallback that pointed elsewhere would give the two invocations
different windows forever, with nothing to see.
