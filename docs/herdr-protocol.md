# herdr socket notes (0.8.0 floor; shape revalidated on stable 0.8.2)

Working notes for `src/herdr.rs`. The large capture is from a live 0.8.0 server
with ten workspaces and nineteen panes. The fields standup consumes were
revalidated live against stable 0.8.2: its snapshot JSON reports
`version: "0.8.2"` and `protocol: 20` with the same `session_snapshot` shape.
Herdr's separate client/server wire protocol is 21; that is not the snapshot
JSON's `protocol` field. Only the parts standup actually depends on are covered.

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

The request `id` must be a string. Every response must echo that exact string;
missing, non-string, or mismatched response ids are contract failures checked
before either `result` or `error` is interpreted. `params` is mandatory and must
be an object — send `{}` for methods that take no parameters, never `null`.

**The socket answers one request per connection and then sends EOF.** Every call
must be able to reconnect and retry once. That is the normal path rather than an
error path, and it is also what carries a client across `herdr update
--handoff`, where the first attempt lands on a socket the old server has just
unlinked. Retry only on a transport failure. A rejected request and a parsed
response that violates the contract are final: either may follow a completed
request, so retrying could duplicate side effects.

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
yields nothing at all, which looks exactly like an idle session. The client
therefore requires result type `session_snapshot`, an object `snapshot`, and
array-valued `workspaces`, `panes`, and `agents`. A missing or wrongly typed
required field is a named compatibility error, never an empty list; its message
includes every available result type and snapshot version/protocol value.

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

### A workspace is not a place

`agents[]` rows are keyed to a workspace, and a workspace's panes need not all
sit in the same checkout: `paths_of` returns the union of the tracked checkout
path and every distinct pane `cwd`, and that is regularly more than one
directory. So a workspace-scoped agent list credits every agent with work in
every checkout the workspace touched, which is a guess where the digest promises
a fact — and two agents in one window then collapse into whichever herdr
mentioned last.

Each `agents[]` row carries its own `cwd`, and in the live capture all eighteen
do. That is the field attribution is built on, with the row's own `pane_id`
resolving to a pane as the second source. `cwd` is **optional** here, so the
answer is allowed to be unknown: `standup.rs::place_agents` places an agent with
no directory only when its workspace touches exactly one checkout — where there
is nowhere else it could have been — and otherwise credits it to nothing and
says so in a note.

Note that the capture has **no** workspace whose panes straddle two directories:
all ten sit in one. The tests build that case by moving one pane and its agent
onto a sibling worktree, which is the only honest way to cover a shape the
capture does not contain.

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
