//! herdr socket client.
//!
//! Newline-delimited JSON over the socket at `HERDR_SOCKET_PATH`. The server
//! answers exactly one request per connection and then closes, so every call
//! must be able to reconnect and retry once — that is the hot path, not an edge
//! case, and it is also what carries the client across `herdr update --handoff`.
//!
//! Request and response framing:
//!
//! ```text
//! request : {"id":"<string>","method":"<name>","params":{...}}\n
//! success : {"id":"<string>","result":{"type":"<snake_case>",...}}\n
//! failure : {"id":"<string>","error":{"code":"...","message":"..."}}\n
//! ```
//!
//! `id` must be a string and `params` must be an object — `{}` for methods that
//! take none, never `null`.
//!
//! # What this plugin reads, and the trap in it
//!
//! One `session.snapshot`, and nothing else. The payload is
//! `{"type":"session_snapshot","snapshot":{...}}` and the arrays live one level
//! **down**, under `snapshot`; reading them off the result object yields no
//! workspaces at all, which looks exactly like an idle session. An absent
//! `snapshot` key is therefore an error, never an empty list.
//!
//! The larger trap is `workspace.worktree`. It is present only for workspaces
//! herdr itself opened as a repository or a worktree. In a live ten-workspace
//! capture, five workspaces sitting in ordinary git checkouts had no `worktree`
//! key at all. Discovery must take the union of `worktree.checkout_path` and
//! every distinct pane `cwd`, and let git decide which are checkouts. Use
//! `cwd`, never `foreground_cwd` — the latter follows whatever the foreground
//! process happens to be doing and was observed pointing into a node_modules
//! directory.

use std::cmp::Ordering;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::config;
use crate::model::{AgentRef, WorkspaceRef};
use crate::Result;

/// Long enough that a busy server is not mistaken for a dead one, short enough
/// that a digest can never park forever on a half-open socket.
const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// A herdr error envelope, carried as a real error type so callers can tell a
/// rejected request from a transport failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrError {
    pub code: String,
    pub message: String,
}

impl fmt::Display for HerdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "herdr {}: {}", self.code, self.message)
    }
}

impl std::error::Error for HerdrError {}

/// Error code from a herdr error envelope, or `None` for a transport failure.
pub fn error_code<'a>(err: &'a (dyn std::error::Error + 'static)) -> Option<&'a str> {
    err.downcast_ref::<HerdrError>().map(|e| e.code.as_str())
}

/// Split so that only transport failures are retried. Retrying a *rejected*
/// request would be rejected again, and would double-count against herdr's own
/// error accounting.
enum Failure {
    Transport(String),
    Protocol(HerdrError),
}

#[derive(Debug)]
pub struct Herdr {
    socket_path: PathBuf,
    next_id: u64,
}

impl Herdr {
    /// Dials the socket once so a missing server is reported here, with the
    /// path, rather than as a confusing failure inside the first call.
    pub fn connect() -> Result<Self> {
        let socket_path = socket_path()?;
        // The connection is dropped immediately: one request per connection
        // means there is nothing worth holding open.
        dial(&socket_path)?;
        Ok(Self {
            socket_path,
            next_id: 0,
        })
    }

    /// One `session.snapshot`, reduced to the workspaces and the directories
    /// they occupy. Workspaces with no usable directory are dropped; everything
    /// else, including non-repositories, is returned and left for git to judge.
    pub fn workspaces(&mut self) -> Result<Vec<WorkspaceRef>> {
        let result = self.call("session.snapshot", json!({}))?;
        // The arrays live one level down, under `snapshot`. Absent is an error
        // rather than a fallback: an empty workspace list is indistinguishable
        // from an idle session, so a protocol change here would make the plugin
        // quietly report nothing at all instead of failing.
        let snapshot = result
            .get("snapshot")
            .filter(|snapshot| snapshot.is_object())
            .ok_or_else(|| {
                format!(
                    "session.snapshot returned no `snapshot` object (result type `{}`)",
                    text(&result, "type").unwrap_or("missing")
                )
            })?;
        Ok(reduce_snapshot(snapshot))
    }

    pub fn notify(&mut self, title: &str, body: &str) -> Result<()> {
        self.call("notification.show", json!({ "title": title, "body": body }))?;
        Ok(())
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = format!("standup:{}", self.next_id);
        match self.call_once(&id, method, &params) {
            Ok(result) => Ok(result),
            Err(Failure::Protocol(err)) => Err(Box::new(err)),
            // One request per connection is the normal path, not an error path:
            // the server EOFs after answering, so the connection we would reuse
            // is already gone. The same retry carries the client across a
            // `herdr update --handoff`, where the first attempt lands on a
            // socket the old server has just unlinked.
            Err(Failure::Transport(first)) => match self.call_once(&id, method, &params) {
                Ok(result) => Ok(result),
                Err(Failure::Protocol(err)) => Err(Box::new(err)),
                Err(Failure::Transport(second)) => {
                    Err(format!("{method} failed twice: {first}; on retry: {second}").into())
                }
            },
        }
    }

    fn call_once(
        &self,
        id: &str,
        method: &str,
        params: &Value,
    ) -> std::result::Result<Value, Failure> {
        let stream = dial(&self.socket_path).map_err(|e| Failure::Transport(e.to_string()))?;

        // `params` is mandatory and must be an object — never null, `{}` when
        // empty.
        let params = if params.is_object() {
            params.clone()
        } else {
            Value::Object(Map::new())
        };
        let mut line = serde_json::to_string(&json!({
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|e| Failure::Transport(format!("could not encode request: {e}")))?;
        line.push('\n');

        (&stream)
            .write_all(line.as_bytes())
            .and_then(|()| (&stream).flush())
            .map_err(|e| Failure::Transport(format!("write to {method} failed: {e}")))?;

        let mut response = String::new();
        BufReader::new(&stream)
            .read_line(&mut response)
            .map_err(|e| Failure::Transport(format!("read of {method} response failed: {e}")))?;
        if response.trim().is_empty() {
            return Err(Failure::Transport(
                "server closed the connection without answering".into(),
            ));
        }

        let value: Value = serde_json::from_str(response.trim_end())
            .map_err(|e| Failure::Transport(format!("malformed response to {method}: {e}")))?;

        if let Some(err) = value.get("error") {
            return Err(Failure::Protocol(HerdrError {
                code: text(err, "code").unwrap_or("unknown_error").to_string(),
                message: text(err, "message").unwrap_or("no message").to_string(),
            }));
        }
        match value.get("result") {
            Some(result) => Ok(result.clone()),
            None => Err(Failure::Transport(format!(
                "response to {method} carried neither result nor error"
            ))),
        }
    }
}

fn dial(socket_path: &Path) -> Result<UnixStream> {
    let stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("cannot reach herdr at {}: {e}", socket_path.display()))?;
    // Without these a half-open socket parks the digest forever.
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(stream)
}

fn socket_path() -> Result<PathBuf> {
    // herdr injects this into everything it spawns; the fallback exists only
    // for hand invocation from a shell.
    if let Some(path) = config::non_empty_env("HERDR_SOCKET_PATH") {
        return Ok(PathBuf::from(path));
    }
    let config_home = config::non_empty_env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| config::non_empty_env("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or("HERDR_SOCKET_PATH is unset and neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(config_home.join("herdr").join("herdr.sock"))
}

// ---------------------------------------------------------------------------
// Reduction
// ---------------------------------------------------------------------------

/// Reduces a `session.snapshot` result's inner `snapshot` object to workspace
/// references. Split out so tests can drive it from captured real output
/// without a socket.
pub fn reduce_snapshot(snapshot: &Value) -> Vec<WorkspaceRef> {
    let panes = array(snapshot, "panes");
    let agent_rows = array(snapshot, "agents");

    let mut workspaces = Vec::new();
    for workspace in array(snapshot, "workspaces") {
        let Some(workspace_id) = text(workspace, "workspace_id") else {
            continue;
        };
        let here: Vec<&Value> = panes
            .iter()
            .filter(|pane| text(pane, "workspace_id") == Some(workspace_id))
            .collect();

        let paths = paths_of(workspace, &here);
        if paths.is_empty() {
            // Nowhere on disk to look. Not an error — there is simply nothing
            // for git to say about it.
            continue;
        }

        workspaces.push(WorkspaceRef {
            workspace_id: workspace_id.to_string(),
            label: text(workspace, "label").unwrap_or(workspace_id).to_string(),
            number: workspace.get("number").and_then(Value::as_u64),
            paths,
            agents: agents_of(workspace_id, agent_rows, &here),
            agent_status: text(workspace, "agent_status").map(str::to_string),
        });
    }
    workspaces
}

/// The directories a workspace occupies: its tracked checkout first, when it
/// has one, then every distinct pane `cwd` in pane order.
///
/// `worktree` is present only for workspaces herdr itself opened as a repo or a
/// worktree — in the live capture, five of ten workspaces sitting in ordinary
/// git checkouts had no such key — so it is one source among two, never the
/// filter.
fn paths_of(workspace: &Value, panes: &[&Value]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(worktree) = workspace.get("worktree").filter(|w| w.is_object()) {
        if let Some(checkout) = text(worktree, "checkout_path") {
            push_unique(&mut paths, tidy_path(checkout));
        }
    }
    for pane in panes {
        // `cwd`, never `foreground_cwd`: the latter follows the foreground
        // process and was observed pointing at a pyright install for a pane
        // whose real cwd was a repository.
        if let Some(cwd) = text(pane, "cwd") {
            push_unique(&mut paths, tidy_path(cwd));
        }
    }
    paths
}

/// The agents herdr reports in one workspace, ordered by pane so the digest is
/// stable between runs.
fn agents_of(workspace_id: &str, agent_rows: &[Value], panes: &[&Value]) -> Vec<AgentRef> {
    let mut agents: Vec<AgentRef> = Vec::new();
    for row in agent_rows {
        if text(row, "workspace_id") != Some(workspace_id) {
            continue;
        }
        agents.push(AgentRef {
            // The user's own label ("shear-classifier"), absent for agents they
            // did not name.
            name: text(row, "name").map(str::to_string),
            // The program: `claude`, `opencode`.
            program: text(row, "agent").map(str::to_string),
            session_id: session_id(row),
            pane_id: text(row, "pane_id").unwrap_or_default().to_string(),
            status: text(row, "agent_status").map(str::to_string),
        });
    }

    // A pane can carry an `agent` with no row in `agents[]` — a workspace
    // enumerated mid-spawn, say. That agent still ran, so it still counts;
    // everything the pane itself knows is carried over, and only the user's
    // name for it is genuinely unavailable.
    for pane in panes {
        let Some(pane_id) = text(pane, "pane_id") else {
            continue;
        };
        let Some(program) = text(pane, "agent") else {
            continue;
        };
        if agents.iter().any(|agent| agent.pane_id == pane_id) {
            continue;
        }
        agents.push(AgentRef {
            name: None,
            program: Some(program.to_string()),
            session_id: session_id(pane),
            pane_id: pane_id.to_string(),
            status: text(pane, "agent_status").map(str::to_string),
        });
    }

    agents.sort_by(|a, b| natural_cmp(&a.pane_id, &b.pane_id));
    agents
}

/// `agent_session` is an **object** on the wire — `{agent, kind, source,
/// value}` — not a string, and some agents have none at all. Anything treating
/// it as a string silently gets nothing.
fn session_id(row: &Value) -> Option<String> {
    row.get("agent_session")
        .filter(|session| session.is_object())
        .and_then(|session| text(session, "value"))
        .map(str::to_string)
}

/// Drops `.` components from a path herdr reports.
///
/// herdr echoes back whatever path a workspace was created with, so one made
/// with `--cwd .` arrives as `/home/you/repos/app/.`. The path still resolves,
/// but it is rendered in the digest and it is used as a dictionary key for
/// joining workspaces to checkouts, where the trailing `.` would split one
/// directory into two.
fn tidy_path(raw: &str) -> PathBuf {
    let path = Path::new(raw);
    let tidied: PathBuf = path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    if tidied.as_os_str().is_empty() {
        path.to_path_buf()
    } else {
        tidied
    }
}

/// Orders `w15:p2` before `w15:p10`, which a plain string comparison does not.
/// Pane ids are herdr's, not ours, and a digest whose agents reshuffle between
/// runs is a digest nobody can diff.
fn natural_cmp(left: &str, right: &str) -> Ordering {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        if left[i].is_ascii_digit() && right[j].is_ascii_digit() {
            let start_left = i;
            while i < left.len() && left[i].is_ascii_digit() {
                i += 1;
            }
            let start_right = j;
            while j < right.len() && right[j].is_ascii_digit() {
                j += 1;
            }
            let a = trim_zeros(&left[start_left..i]);
            let b = trim_zeros(&right[start_right..j]);
            // Longer run of significant digits is the larger number.
            match a.len().cmp(&b.len()).then_with(|| a.cmp(b)) {
                Ordering::Equal => {}
                other => return other,
            }
        } else {
            match left[i].cmp(&right[j]) {
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                other => return other,
            }
        }
    }
    (left.len() - i).cmp(&(right.len() - j))
}

fn trim_zeros(digits: &[u8]) -> &[u8] {
    let start = digits
        .iter()
        .position(|byte| *byte != b'0')
        .unwrap_or(digits.len());
    &digits[start..]
}

/// Non-empty string field. herdr injects empty strings for absent context, so
/// empty means absent rather than "a value that happens to be blank".
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value.get(key).and_then(Value::as_array).map_or(&[], |a| a)
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_ids_order_by_number_not_by_character() {
        let mut ids = vec!["w15:p10", "w15:p2", "w15:p1", "w9:p1"];
        ids.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(ids, ["w9:p1", "w15:p1", "w15:p2", "w15:p10"]);
    }

    #[test]
    fn a_dot_component_is_dropped_but_the_path_survives() {
        assert_eq!(
            tidy_path("/home/you/repo/."),
            PathBuf::from("/home/you/repo")
        );
        assert_eq!(tidy_path("/home/you/repo"), PathBuf::from("/home/you/repo"));
        // A path that is nothing but `.` still has to resolve to something.
        assert_eq!(tidy_path("."), PathBuf::from("."));
    }

    #[test]
    fn an_empty_string_is_absent_not_a_value() {
        let value = json!({"label": "  ", "other": "x"});
        assert_eq!(text(&value, "label"), None);
        assert_eq!(text(&value, "other"), Some("x"));
    }
}
