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

use std::fmt;

use crate::model::WorkspaceRef;
use crate::Result;

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

#[derive(Debug)]
pub struct Herdr {
    _private: (),
}

impl Herdr {
    /// Dials the socket once so a missing server is reported here, with the
    /// path, rather than as a confusing failure inside the first call.
    pub fn connect() -> Result<Self> {
        unimplemented!("herdr::Herdr::connect — owned by the surface builder")
    }

    /// One `session.snapshot`, reduced to the workspaces and the directories
    /// they occupy. Workspaces with no usable directory are dropped; everything
    /// else, including non-repositories, is returned and left for git to judge.
    pub fn workspaces(&mut self) -> Result<Vec<WorkspaceRef>> {
        unimplemented!("herdr::Herdr::workspaces — owned by the surface builder")
    }

    pub fn notify(&mut self, title: &str, body: &str) -> Result<()> {
        let _ = (title, body);
        unimplemented!("herdr::Herdr::notify — owned by the surface builder")
    }
}

/// Reduces a `session.snapshot` result's inner `snapshot` object to workspace
/// references. Split out so tests can drive it from captured real output
/// without a socket.
pub fn reduce_snapshot(snapshot: &serde_json::Value) -> Vec<WorkspaceRef> {
    let _ = snapshot;
    unimplemented!("herdr::reduce_snapshot — owned by the surface builder")
}
