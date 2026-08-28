//! Thin Herdr client wrapper and domain response reduction.
//!
//! Socket transport, NDJSON framing, response bounds, retries, envelope
//! validation, and environment resolution are delegated to Crook.
//!
//! # What this plugin reads, and the trap in it
//!
//! One `session.snapshot`, and nothing else. The payload is
//! `{"type":"session_snapshot","snapshot":{...}}` and the arrays live one level
//! **down**, under `snapshot`; reading them off the result object yields no
//! workspaces at all, which looks exactly like an idle session. The result type,
//! snapshot object, and its `workspaces`, `panes`, and `agents` arrays are
//! therefore required contract fields, never optional empty fallbacks.
//!
//! The larger trap is `workspace.worktree`. It is present only for workspaces
//! herdr itself opened as a repository or a worktree. In a live ten-workspace
//! capture, five workspaces sitting in ordinary git checkouts had no `worktree`
//! key at all. Discovery must take the union of `worktree.checkout_path` and
//! every distinct user pane `cwd`, and let git decide which are checkouts.
//! Installed plugin panes are the exception: Herdr 0.8.2 runs them from the
//! plugin root, which is an implementation directory rather than user work.
//! The invocation context restores the focused user directory when that
//! workspace has no tracked checkout. Use `cwd`, never `foreground_cwd` — the
//! latter follows whatever the foreground process happens to be doing.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use crook::client::{Client, Error as ClientError, RetrySafety};
use crook::env::PluginEnv;
use serde_json::{json, Value};

use crate::config::{self, RepositoryScope};
use crate::model::{AgentRef, WorkspaceRef};
use crate::Result;

/// A herdr error envelope, carried as a real error type so callers can tell a
/// rejected request from a transport or response-contract failure.
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

/// Error code from a herdr error envelope, or `None` for transport and
/// response-contract failures.
pub fn error_code<'a>(err: &'a (dyn std::error::Error + 'static)) -> Option<&'a str> {
    err.downcast_ref::<HerdrError>().map(|e| e.code.as_str())
}

#[derive(Debug)]
struct HerdrContractError {
    method: String,
    message: String,
}

impl HerdrContractError {
    fn new(method: &str, message: impl Into<String>) -> Self {
        Self {
            method: method.to_string(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HerdrContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid herdr response contract for {}: {}",
            self.method, self.message
        )
    }
}

impl std::error::Error for HerdrContractError {}

#[derive(Debug)]
pub struct Herdr {
    client: Client,
}

impl Herdr {
    /// Dials the socket once so a missing server is reported here, with the
    /// path, rather than as a confusing failure inside the first call.
    pub fn connect() -> Result<Self> {
        let environment = PluginEnv::resolve(config::PLUGIN_ID);
        let client = Client::connect(environment.socket_path(), "standup")?;
        Ok(Self { client })
    }

    /// One `session.snapshot`, reduced to the workspaces and the directories
    /// they occupy. Workspaces with no usable directory are dropped; everything
    /// else, including non-repositories, is returned and left for git to judge.
    pub fn workspaces(&mut self) -> Result<Vec<WorkspaceRef>> {
        let result = self.request("session.snapshot", json!({}), RetrySafety::Idempotent)?;
        reduce_snapshot(&result)
    }

    /// The same snapshot, with installed-plugin invocation directories applied
    /// to repository discovery.
    pub fn workspaces_scoped(&mut self, scope: &RepositoryScope) -> Result<Vec<WorkspaceRef>> {
        let result = self.request("session.snapshot", json!({}), RetrySafety::Idempotent)?;
        reduce_snapshot_scoped(&result, scope)
    }

    pub fn notify(&mut self, title: &str, body: &str) -> Result<()> {
        self.request(
            "notification.show",
            json!({ "title": title, "body": body }),
            RetrySafety::Never,
        )?;
        Ok(())
    }

    fn request(&self, method: &str, params: Value, retry_safety: RetrySafety) -> Result<Value> {
        match self.client.request(method, params, retry_safety) {
            Ok(result) => Ok(result),
            Err(ClientError::Protocol { code, message }) => {
                Err(Box::new(HerdrError { code, message }))
            }
            Err(error) => Err(Box::new(error)),
        }
    }
}

// ---------------------------------------------------------------------------
// Reduction
// ---------------------------------------------------------------------------

/// Reduces a complete `session.snapshot` result to workspace references.
///
/// Validation deliberately lives here rather than only in [`Herdr::workspaces`]
/// so captured-response tests and any other direct callers cannot turn a
/// changed or partial protocol shape into an ordinary empty session.
pub fn reduce_snapshot(result: &Value) -> Result<Vec<WorkspaceRef>> {
    reduce_snapshot_scoped(result, &RepositoryScope::default())
}

/// Reduces a snapshot while keeping an installed plugin's own checkout out of
/// the repository candidates.
pub fn reduce_snapshot_scoped(
    result: &Value,
    scope: &RepositoryScope,
) -> Result<Vec<WorkspaceRef>> {
    let shape = validate_snapshot_result(result)?;

    let mut workspaces = Vec::new();
    for workspace in shape.workspaces {
        let Some(workspace_id) = text(workspace, "workspace_id") else {
            continue;
        };
        let here: Vec<&Value> = shape
            .panes
            .iter()
            .filter(|pane| text(pane, "workspace_id") == Some(workspace_id))
            .collect();

        let paths = paths_of(workspace_id, workspace, &here, scope);
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
            agents: agents_of(workspace_id, shape.agents, &here),
            agent_status: text(workspace, "agent_status").map(str::to_string),
        });
    }
    Ok(workspaces)
}

struct SnapshotShape<'a> {
    workspaces: &'a [Value],
    panes: &'a [Value],
    agents: &'a [Value],
}

fn validate_snapshot_result(
    result: &Value,
) -> std::result::Result<SnapshotShape<'_>, HerdrContractError> {
    if result.get("type").and_then(Value::as_str) != Some("session_snapshot") {
        return Err(snapshot_contract_error(
            result,
            "expected result `type` to be `session_snapshot`",
        ));
    }

    let Some(snapshot) = result.get("snapshot").filter(|value| value.is_object()) else {
        return Err(snapshot_contract_error(
            result,
            "required `snapshot` must be an object",
        ));
    };

    Ok(SnapshotShape {
        workspaces: required_snapshot_array(result, snapshot, "workspaces")?,
        panes: required_snapshot_array(result, snapshot, "panes")?,
        agents: required_snapshot_array(result, snapshot, "agents")?,
    })
}

fn required_snapshot_array<'a>(
    result: &Value,
    snapshot: &'a Value,
    name: &str,
) -> std::result::Result<&'a [Value], HerdrContractError> {
    snapshot
        .get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| {
            snapshot_contract_error(
                result,
                format!("required `snapshot.{name}` must be an array"),
            )
        })
}

fn snapshot_contract_error(result: &Value, message: impl Into<String>) -> HerdrContractError {
    let snapshot = result.get("snapshot");
    HerdrContractError::new(
        "session.snapshot",
        format!(
            "{}; available metadata: result type {}, snapshot version {}, snapshot protocol {}",
            message.into(),
            available_value(result.get("type")),
            available_value(snapshot.and_then(|value| value.get("version"))),
            available_value(snapshot.and_then(|value| value.get("protocol"))),
        ),
    )
}

fn available_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => format!("`{value}`"),
        Some(value) => format!("`{value}`"),
        None => "missing".to_string(),
    }
}

/// The directories a workspace occupies: its tracked checkout first, when it
/// has one, then every distinct user pane `cwd` in pane order.
///
/// `worktree` is present only for workspaces herdr itself opened as a repo or a
/// worktree — in the live capture, five of ten workspaces sitting in ordinary
/// git checkouts had no such key — so it is one source among two, never the
/// filter. For an installed invocation in such an untracked workspace, Crook's
/// focused-pane/workspace cwd is inserted before the pane list: the plugin pane
/// itself runs from the plugin root and is filtered below.
fn paths_of(
    workspace_id: &str,
    workspace: &Value,
    panes: &[&Value],
    scope: &RepositoryScope,
) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    // Order is carried by the vector; membership by the set, so a workspace with
    // a great many panes stays linear rather than quadratic.
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut push = |paths: &mut Vec<PathBuf>, path: PathBuf| {
        if seen.insert(path.clone()) {
            paths.push(path);
        }
    };

    let tracked_checkout = workspace
        .get("worktree")
        .filter(|worktree| worktree.is_object())
        .and_then(|worktree| text(worktree, "checkout_path"));
    if let Some(checkout) = tracked_checkout {
        push(&mut paths, tidy_path(checkout));
    } else if scope.workspace_id() == Some(workspace_id) {
        if let Some(cwd) = scope.invocation_cwd() {
            push(&mut paths, tidy_path(&cwd.to_string_lossy()));
        }
    }

    for pane in panes {
        // `cwd`, never `foreground_cwd`: the latter follows the foreground
        // process and was observed pointing at a pyright install for a pane
        // whose real cwd was a repository.
        if let Some(cwd) = text(pane, "cwd") {
            let path = tidy_path(cwd);
            if scope
                .plugin_root()
                .is_some_and(|plugin_root| path.starts_with(plugin_root))
            {
                continue;
            }
            push(&mut paths, path);
        }
    }
    paths
}

/// The agents herdr reports in one workspace, ordered by pane so the digest is
/// stable between runs.
///
/// Each one carries the directory it was sitting in, because a workspace is not
/// a place: its panes can be in different checkouts, and an agent list scoped
/// only to the workspace credits every agent to every one of them. The agent
/// rows in the live capture each carry their own `cwd`; the protocol marks it
/// optional, so the pane the row names is consulted second and the answer is
/// allowed to stay `None`. Guessing is what this exists to stop.
fn agents_of(workspace_id: &str, agent_rows: &[Value], panes: &[&Value]) -> Vec<AgentRef> {
    let mut agents: Vec<AgentRef> = Vec::new();
    let mut joined: HashSet<&str> = HashSet::new();
    for row in agent_rows {
        if text(row, "workspace_id") != Some(workspace_id) {
            continue;
        }
        let pane_id = text(row, "pane_id");
        if let Some(pane_id) = pane_id {
            joined.insert(pane_id);
        }
        agents.push(AgentRef {
            // The user's own label ("shear-classifier"), absent for agents they
            // did not name.
            name: text(row, "name").map(str::to_string),
            // The program: `claude`, `opencode`.
            program: text(row, "agent").map(str::to_string),
            session_id: session_id(row),
            pane_id: pane_id.unwrap_or_default().to_string(),
            status: text(row, "agent_status").map(str::to_string),
            cwd: agent_cwd(row, pane_id, panes),
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
        if !joined.insert(pane_id) {
            continue;
        }
        agents.push(AgentRef {
            name: None,
            program: Some(program.to_string()),
            session_id: session_id(pane),
            pane_id: pane_id.to_string(),
            status: text(pane, "agent_status").map(str::to_string),
            cwd: text(pane, "cwd").map(tidy_path),
        });
    }

    agents.sort_by(|a, b| natural_cmp(&a.pane_id, &b.pane_id));
    agents
}

/// Where an agent was working: its own `cwd`, else its pane's.
///
/// `cwd`, never `foreground_cwd`, for the same reason [`paths_of`] avoids it —
/// the latter follows the foreground process and was observed pointing at a
/// pyright install. `None` is a real answer and stays one.
fn agent_cwd(row: &Value, pane_id: Option<&str>, panes: &[&Value]) -> Option<PathBuf> {
    if let Some(cwd) = text(row, "cwd") {
        return Some(tidy_path(cwd));
    }
    let pane_id = pane_id?;
    panes
        .iter()
        .find(|pane| text(pane, "pane_id") == Some(pane_id))
        .and_then(|pane| text(pane, "cwd"))
        .map(tidy_path)
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

/// The suffix Linux appends to the working directory of a process whose
/// directory has been removed, as read back through `/proc/<pid>/cwd`.
const DELETED_MARKER: &str = " (deleted)";

/// Normalises a path herdr reports back at us.
///
/// Two annotations come off, and both matter because these paths are rendered in
/// the digest *and* used as the key that joins workspaces to checkouts, where a
/// difference of one character splits one directory into two.
///
/// 1. **A `.` component.** herdr echoes back whatever path a workspace was
///    created with, so one made with `--cwd .` arrives as
///    `/home/you/repos/app/.`.
/// 2. **A trailing `(deleted)` marker.** Observed live: a workspace reported
///    `worktree.checkout_path` as `…/fx/repo` while its only pane reported `cwd`
///    as `…/fx/repo (deleted)`, because the directory had been removed under the
///    running shell. That reduced to two candidate directories and printed the
///    same "is not a git checkout" note twice, with both lines truncated at the
///    same column so they read as an exact duplicate.
///
/// The marker is the kernel's annotation rather than part of the name, so it
/// comes off and the pair collapses. A directory genuinely named `… (deleted)`
/// would be probed under its unsuffixed name instead — the cheaper mistake, and
/// one the marker makes unavoidable for any such name anyway.
fn tidy_path(raw: &str) -> PathBuf {
    let raw = raw
        .strip_suffix(DELETED_MARKER)
        .filter(|stripped| !stripped.is_empty())
        .unwrap_or(raw);
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
    fn the_kernels_deleted_marker_is_not_part_of_the_name() {
        assert_eq!(
            tidy_path("/home/you/repo (deleted)"),
            PathBuf::from("/home/you/repo")
        );
        // Only as a suffix, and only the exact marker.
        assert_eq!(
            tidy_path("/home/you/repo (deleted)/src"),
            PathBuf::from("/home/you/repo (deleted)/src")
        );
        assert_eq!(
            tidy_path("/home/you/deleted"),
            PathBuf::from("/home/you/deleted")
        );
        // Stripping down to nothing would be worse than keeping the marker.
        assert_eq!(tidy_path(" (deleted)"), PathBuf::from(" (deleted)"));
    }

    #[test]
    fn an_empty_string_is_absent_not_a_value() {
        let value = json!({"label": "  ", "other": "x"});
        assert_eq!(text(&value, "label"), None);
        assert_eq!(text(&value, "other"), Some("x"));
    }
}
