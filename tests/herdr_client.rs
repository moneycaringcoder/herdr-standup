//! Wire-level tests for the herdr socket client.
//!
//! Two things are being pinned here, and they fail in different ways.
//!
//! **The transport.** Every test stands up a real Unix socket server in a temp
//! directory and asserts the bytes the client puts on the wire, because the
//! parts of this protocol that bite — mandatory `{}` params, a string `id`, one
//! request per connection — are invisible from the Rust API alone.
//!
//! **The shape.** The snapshot replies come from `tests/snapshots/`, which is an
//! anonymised copy of a real `herdr api snapshot` capture: ten workspaces,
//! nineteen panes, eighteen agents, with every optional field present or absent
//! exactly as the live server sent it. Paths, labels, agent names, terminal
//! titles and session ids are invented; nothing else was touched. This matters
//! more than it looks: a reply hand-written in the shape the client *expects*
//! cannot catch the client reading the wrong level of the document, which is
//! precisely the bug that shipped past a sibling plugin's 130-test suite.
//!
//! No running herdr is required, and nothing here touches the user's state.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use standup::herdr::{error_code, reduce_snapshot, reduce_snapshot_scoped, Herdr};
use standup::model::WorkspaceRef;

/// An anonymised `herdr api snapshot` response, structurally identical to the
/// live capture it was made from. Stored pretty-printed so it can be reviewed;
/// re-serialised onto one line before it goes on the wire. Only the envelope id
/// is replaced, because it is request-specific and must echo the client's id.
const CAPTURE: &str = include_str!("snapshots/session_snapshot.json");
const INSTALLED_0_8_2: &str = include_str!("snapshots/herdr_0_8_2_installed_plugin.json");

/// The `foreground_cwd` of two panes in the capture, both of which had a real
/// `cwd` in a repository at the time. Reading the wrong field puts this in the
/// digest.
const FOREGROUND_TRAP: &str =
    "/home/dev/.local/share/mise/installs/node/24.18.0/lib/node_modules/pyright/dist";

/// Comfortably past the client's `MAX_RESPONSE_BYTES`, which is 4 MiB. Kept as a
/// number here rather than imported, so that raising the ceiling in the client
/// without thinking about this test shows up as a failure.
const OVER_THE_CEILING: usize = 5 * 1024 * 1024;

fn captured() -> Value {
    serde_json::from_str(CAPTURE).expect("the captured fixture is JSON")
}

fn installed_0_8_2() -> Value {
    serde_json::from_str(INSTALLED_0_8_2).expect("the Herdr 0.8.2 fixture is JSON")
}

/// The whole response envelope, framed as one line the way herdr frames it.
fn captured_line() -> String {
    let mut capture = captured();
    capture["id"] = json!("standup:1");
    serde_json::to_string(&capture).expect("re-serialise")
}

/// Just the inner snapshot object, for driving `reduce_snapshot` with no socket.
fn captured_snapshot() -> Value {
    captured()["result"]["snapshot"].clone()
}

/// Drives the public reducer with a complete result while keeping mutation
/// tests focused on the inner snapshot they are exercising.
fn reduce(snapshot: &Value) -> Vec<WorkspaceRef> {
    reduce_snapshot(&json!({
        "type": "session_snapshot",
        "snapshot": snapshot,
    }))
    .expect("valid session.snapshot result")
}

fn workspace<'a>(workspaces: &'a [WorkspaceRef], id: &str) -> &'a WorkspaceRef {
    workspaces
        .iter()
        .find(|w| w.workspace_id == id)
        .unwrap_or_else(|| panic!("no workspace {id} in {workspaces:#?}"))
}

/// `HERDR_SOCKET_PATH` is process-global, so the tests that set it have to run
/// one at a time even though cargo runs them on separate threads.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct ScopedEnv {
    key: &'static str,
    previous: Option<OsString>,
}

impl ScopedEnv {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

/// What the server does with one connection.
#[derive(Clone)]
enum Reply {
    /// Answer, then close — the real server's behaviour.
    Line(String),
    /// Read the request and close without answering, which is what a client
    /// sees when it lands on a socket the server is tearing down.
    Eof,
    /// One line that is over the client's ceiling, newline and all.
    Oversize,
    /// Bytes with no newline, for as long as the client keeps reading. A peer
    /// stuck like this once took the process to 5.3 GB and got it killed.
    Endless,
}

fn captured_reply() -> Reply {
    Reply::Line(captured_line())
}

struct TestServer {
    path: PathBuf,
    dir: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    fn start(replies: Vec<Reply>) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "standup-wire-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        // Kept short: a Unix socket path is capped at ~108 bytes.
        let path = dir.join("s.sock");

        let listener = UnixListener::bind(&path).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut replies = replies.into_iter();
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).expect("blocking");
                            let mut line = String::new();
                            let mut reader = BufReader::new(&stream);
                            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                continue;
                            }
                            requests.lock().expect("requests").push(line);
                            match replies.next() {
                                Some(Reply::Line(reply)) => {
                                    let mut stream = &stream;
                                    let _ = stream.write_all(reply.as_bytes());
                                    let _ = stream.write_all(b"\n");
                                    let _ = stream.flush();
                                }
                                Some(Reply::Oversize) => {
                                    let mut stream = &stream;
                                    let padding = vec![b'x'; 64 * 1024];
                                    let _ = stream.write_all(b"{\"id\":\"standup:1\",\"pad\":\"");
                                    for _ in 0..(OVER_THE_CEILING / padding.len()) {
                                        if stream.write_all(&padding).is_err() {
                                            break;
                                        }
                                    }
                                    let _ = stream.write_all(b"\"}\n");
                                    let _ = stream.flush();
                                }
                                Some(Reply::Endless) => {
                                    // No newline, ever. Stops only when the
                                    // client gives up and closes on us, which is
                                    // the behaviour under test.
                                    let mut stream = &stream;
                                    let padding = vec![b'y'; 64 * 1024];
                                    let _ = stream.write_all(b"{\"pad\":\"");
                                    while !stop.load(Ordering::SeqCst) {
                                        if stream.write_all(&padding).is_err() {
                                            break;
                                        }
                                    }
                                }
                                // Exhausted or an explicit EOF: just close, the
                                // way herdr closes after answering.
                                Some(Reply::Eof) | None => {}
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };

        Self {
            path,
            dir,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn client(&self) -> Herdr {
        std::env::set_var("HERDR_SOCKET_PATH", &self.path);
        Herdr::connect().expect("connect")
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("requests").clone()
    }

    /// The single request, parsed, with its raw framing already asserted.
    fn only_request(&self) -> Value {
        let requests = self.requests();
        assert_eq!(requests.len(), 1, "expected one request, got {requests:?}");
        parse_framed(&requests[0])
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One line, newline-terminated, with no framing of its own.
fn parse_framed(raw: &str) -> Value {
    assert!(raw.ends_with('\n'), "request must be newline-terminated");
    assert_eq!(
        raw.matches('\n').count(),
        1,
        "one request per line, got {raw:?}"
    );
    serde_json::from_str(raw.trim_end()).expect("request is JSON")
}

/// Sends one invalid response followed by a valid response that would hide an
/// incorrect retry. Requiring an error also proves the invalid shape was not
/// reduced to an ordinary empty session.
fn contract_error(response: Value) -> String {
    let server = TestServer::start(vec![Reply::Line(response.to_string()), captured_reply()]);
    let mut client = server.client();

    let err = client
        .workspaces()
        .expect_err("an invalid response contract must fail");
    assert_eq!(
        server.requests().len(),
        1,
        "response-contract failures must not be retried"
    );
    let message = err.to_string();
    assert!(
        message.contains("invalid herdr response contract"),
        "the failure must be named: {message}"
    );
    message
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

#[test]
fn request_framing_is_a_single_json_line_with_object_params() {
    let _guard = env_lock();
    let server = TestServer::start(vec![captured_reply()]);
    let mut client = server.client();

    client.workspaces().expect("snapshot");

    let request = server.only_request();
    assert_eq!(request["method"], "session.snapshot");
    assert!(request["id"].is_string(), "id must be a string");
    // Mandatory and an object even when empty — never null, never absent.
    assert_eq!(request["params"], json!({}));
    assert!(request["params"].is_object());
    assert!(
        request.get("jsonrpc").is_none(),
        "this protocol has no jsonrpc field"
    );
}

#[test]
fn one_request_per_connection_is_survived_by_reconnecting() {
    let _guard = env_lock();
    // The first connection is read and closed without an answer, exactly as a
    // server that has just handed off behaves. The retry must land the call.
    let server = TestServer::start(vec![Reply::Eof, captured_reply()]);
    let mut client = server.client();

    let workspaces = client.workspaces().expect("the retry should succeed");

    assert_eq!(workspaces.len(), 10);
    assert_eq!(
        server.requests().len(),
        2,
        "the dropped connection must be retried on a fresh one"
    );
}

#[test]
fn a_server_that_never_answers_fails_after_exactly_one_retry() {
    let _guard = env_lock();
    let server = TestServer::start(vec![Reply::Eof, Reply::Eof, captured_reply()]);
    let mut client = server.client();

    let err = client.workspaces().expect_err("both attempts fail");

    assert_eq!(
        server.requests().len(),
        2,
        "one retry, not a loop: a third attempt would have been answered"
    );
    assert_eq!(
        error_code(&*err),
        None,
        "callers must be able to tell blindness from rejection"
    );
    assert!(
        err.to_string().contains("without answering"),
        "the message must say what happened: {err}"
    );
}

#[test]
fn an_error_envelope_is_a_typed_error_and_is_never_retried() {
    let _guard = env_lock();
    let server = TestServer::start(vec![
        Reply::Line(
            json!({
                "id": "standup:1",
                "error": {"code": "unknown_method", "message": "no such method"}
            })
            .to_string(),
        ),
        captured_reply(),
    ]);
    let mut client = server.client();

    let err = client
        .workspaces()
        .expect_err("an error envelope is a failure");

    assert_eq!(error_code(&*err), Some("unknown_method"));
    assert!(err.to_string().contains("no such method"));
    assert_eq!(
        server.requests().len(),
        1,
        "a rejected request is not a transport failure and must not be retried"
    );
}

#[test]
fn a_malformed_line_is_a_contract_failure_and_is_not_retried() {
    let _guard = env_lock();
    let server = TestServer::start(vec![
        Reply::Line("{\"id\": \"standup:1\", \"result\":".to_string()),
        captured_reply(),
    ]);
    let mut client = server.client();

    let err = client
        .workspaces()
        .expect_err("malformed JSON is not a transport failure");

    assert!(err.to_string().contains("malformed JSON"), "{err}");
    assert_eq!(server.requests().len(), 1);
    assert_eq!(error_code(&*err), None);
}

#[test]
fn a_response_with_neither_result_nor_error_is_a_contract_failure() {
    let _guard = env_lock();
    let message = contract_error(json!({"id": "standup:1"}));

    assert!(
        message.contains("neither `result` nor `error`"),
        "{message}"
    );
}

#[test]
fn response_id_must_be_present_string_and_exact_before_payload_is_read() {
    let _guard = env_lock();
    let valid_result = captured()["result"].clone();
    let cases = [
        (
            "missing",
            json!({"result": valid_result.clone()}),
            "missing required string `id`",
        ),
        (
            "non-string",
            json!({"id": 1, "result": valid_result.clone()}),
            "`id` must be a string",
        ),
        (
            "mismatched",
            json!({
                "id": "some-other-request",
                "error": {"code": "unknown_method", "message": "must not be interpreted"}
            }),
            "`id` did not match request `id`",
        ),
    ];

    for (case, response, expected) in cases {
        let message = contract_error(response);
        assert!(
            message.contains(expected),
            "{case} response id was not named: {message}"
        );
    }
}

/// The regression test for an out-of-memory kill. The framing is
/// newline-delimited, so a peer that never sends a newline is a peer that never
/// stops; reading that into an unbounded `String` grew the real binary to 5.3 GB
/// in thirteen seconds and had it killed by signal 9. It must be a bounded,
/// named failure instead.
#[test]
fn a_response_with_no_end_of_line_is_given_up_on_rather_than_read_forever() {
    let _guard = env_lock();
    let server = TestServer::start(vec![Reply::Endless, Reply::Endless]);
    let mut client = server.client();

    let err = client
        .workspaces()
        .expect_err("a peer that never stops talking must not be humoured");

    let message = err.to_string();
    assert!(message.contains("past the"), "{message}");
    assert_eq!(
        error_code(&*err),
        None,
        "this is the transport failing, not herdr refusing"
    );
}

#[test]
fn a_single_line_over_the_ceiling_is_refused_without_retrying() {
    let _guard = env_lock();
    let server = TestServer::start(vec![Reply::Oversize, captured_reply()]);
    let mut client = server.client();

    let err = client.workspaces().expect_err("over the ceiling");

    assert!(err.to_string().contains("past the"), "{err}");
    assert_eq!(
        server.requests().len(),
        1,
        "a deterministic response-size violation must not be retried"
    );
}

/// A reply that fits is still read whole — the ceiling must not be so eager
/// that it truncates a large but ordinary session.
#[test]
fn a_reply_well_under_the_ceiling_is_read_normally() {
    let _guard = env_lock();
    let server = TestServer::start(vec![captured_reply()]);
    let mut client = server.client();

    assert_eq!(client.workspaces().expect("snapshot").len(), 10);
}

#[test]
fn connect_reports_the_socket_path_when_there_is_no_server() {
    let _guard = env_lock();
    std::env::set_var("HERDR_SOCKET_PATH", "/nonexistent/standup-test.sock");

    let err = Herdr::connect().expect_err("no server listening");

    assert!(
        err.to_string().contains("/nonexistent/standup-test.sock"),
        "the message must name the path: {err}"
    );
}

#[test]
fn notify_sends_title_and_body() {
    let _guard = env_lock();
    // Not an `ok` envelope: this method reports whether the toast was shown.
    let server = TestServer::start(vec![Reply::Line(
        json!({
            "id": "standup:1",
            "result": {"type": "notification_show", "shown": true, "reason": "shown"}
        })
        .to_string(),
    )]);
    let mut client = server.client();

    client
        .notify("standup", "3 repos, 11 commits")
        .expect("notify");

    let request = server.only_request();
    assert_eq!(request["method"], "notification.show");
    assert_eq!(request["params"]["title"], "standup");
    assert_eq!(request["params"]["body"], "3 repos, 11 commits");
}

// ---------------------------------------------------------------------------
// Trap 1: the arrays live one level down
// ---------------------------------------------------------------------------

#[test]
fn the_live_0_8_0_capture_is_read_through_the_nested_snapshot_object() {
    let _guard = env_lock();
    let server = TestServer::start(vec![captured_reply()]);
    let mut client = server.client();

    let workspaces = client.workspaces().expect("snapshot");

    assert_eq!(
        workspaces.len(),
        10,
        "a ten-workspace session must not read as idle"
    );
    let atlas = workspace(&workspaces, "w15");
    assert_eq!(atlas.label, "atlas");
    assert_eq!(atlas.number, Some(6));
    assert_eq!(atlas.agent_status.as_deref(), Some("working"));
}

#[test]
fn stable_0_8_2_snapshot_metadata_is_shape_compatible() {
    let _guard = env_lock();
    // Live 0.8.2 snapshot JSON reports protocol 20. Herdr's wire protocol 21
    // is a separate client/server compatibility number, not this field.
    let server = TestServer::start(vec![Reply::Line(
        json!({
            "id": "standup:1",
            "result": {
                "type": "session_snapshot",
                "snapshot": {
                    "version": "0.8.2",
                    "protocol": 20,
                    "workspaces": [],
                    "panes": [],
                    "agents": []
                }
            }
        })
        .to_string(),
    )]);
    let mut client = server.client();

    let workspaces = client.workspaces().expect("0.8.2-compatible shape");

    assert!(workspaces.is_empty());
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn installed_0_8_2_context_excludes_the_plugin_checkout_but_keeps_user_repositories() {
    let _guard = env_lock();
    let fixture = installed_0_8_2();
    let context = fixture["plugin_context"].to_string();
    let plugin_root = fixture["plugin_root"].as_str().expect("plugin root");
    let _context = ScopedEnv::set("HERDR_PLUGIN_CONTEXT_JSON", context);
    let _root = ScopedEnv::set("HERDR_PLUGIN_ROOT", plugin_root);
    let config = standup::config::load_with_args(&[]).expect("installed plugin config");

    let workspaces =
        reduce_snapshot_scoped(&fixture["result"], config.repository_scope()).expect("snapshot");

    assert_eq!(
        workspace(&workspaces, "w-user").paths,
        vec![PathBuf::from("/home/dev/code/invoking-repository")],
        "the focused pane cwd replaces the plugin pane cwd when worktree metadata is absent"
    );
    assert_eq!(
        workspace(&workspaces, "w-global").paths,
        vec![PathBuf::from("/home/dev/code/global-repository")],
        "repository reporting remains global rather than narrowing to the invoking workspace"
    );
    assert_eq!(
        workspace(&workspaces, "w-tracked").paths,
        vec![PathBuf::from("/home/dev/code/tracked-repository")],
        "tracked user worktrees remain authoritative"
    );
    assert!(
        workspaces
            .iter()
            .flat_map(|workspace| &workspace.paths)
            .all(|path| !path.starts_with(plugin_root)),
        "the installed Standup checkout must not become a report repository: {workspaces:#?}"
    );

    let mut workspace_only = fixture["plugin_context"].clone();
    workspace_only
        .as_object_mut()
        .expect("plugin context object")
        .remove("focused_pane_cwd");
    std::env::set_var("HERDR_PLUGIN_CONTEXT_JSON", workspace_only.to_string());
    let fallback_config = standup::config::load_with_args(&[]).expect("workspace context fallback");
    let fallback = reduce_snapshot_scoped(&fixture["result"], fallback_config.repository_scope())
        .expect("snapshot");
    assert_eq!(
        workspace(&fallback, "w-user").paths,
        vec![PathBuf::from("/home/dev/code/workspace-fallback")],
        "workspace_cwd is the fallback when focused_pane_cwd is absent"
    );
}

#[test]
fn direct_reduction_without_plugin_context_keeps_pane_cwd_behavior() {
    let fixture = installed_0_8_2();

    let workspaces = reduce_snapshot(&fixture["result"]).expect("snapshot");

    assert!(
        workspace(&workspaces, "w-user")
            .paths
            .iter()
            .any(|path| path == Path::new(fixture["plugin_root"].as_str().unwrap())),
        "an unscoped direct invocation must not apply installed-plugin filtering"
    );
}

#[test]
fn malformed_plugin_context_fails_before_repository_discovery() {
    let _guard = env_lock();
    let _context = ScopedEnv::set("HERDR_PLUGIN_CONTEXT_JSON", "{");

    let error = standup::config::load_with_args(&[])
        .expect_err("malformed non-empty plugin context must fail");

    assert!(
        error
            .to_string()
            .contains("HERDR_PLUGIN_CONTEXT_JSON contains malformed JSON"),
        "the environment variable and parse failure must be clear: {error}"
    );
}

#[test]
fn session_snapshot_result_type_is_required_and_exact() {
    let _guard = env_lock();

    for (case, replacement, available) in [
        ("missing", None, "result type missing"),
        (
            "wrong",
            Some(json!("other_result")),
            "result type `other_result`",
        ),
    ] {
        let mut result = captured()["result"].clone();
        match replacement {
            Some(value) => result["type"] = value,
            None => {
                result
                    .as_object_mut()
                    .expect("result object")
                    .remove("type");
            }
        }

        let message = contract_error(json!({"id": "standup:1", "result": result}));
        assert!(message.contains("`session_snapshot`"), "{case}: {message}");
        assert!(message.contains(available), "{case}: {message}");
        assert!(
            message.contains("snapshot version `0.8.0`"),
            "{case}: {message}"
        );
        assert!(
            message.contains("snapshot protocol `19`"),
            "{case}: {message}"
        );
    }
}

#[test]
fn a_result_without_the_snapshot_key_is_an_error_not_an_idle_session() {
    let _guard = env_lock();
    // The arrays are present, but hoisted to the level a buggy client would
    // read them from. Returning an empty list here would be indistinguishable
    // from an idle session, which is exactly how this bug hides.
    let flattened = {
        let mut result = captured_snapshot();
        result["type"] = json!("session_snapshot");
        result
    };

    let message = contract_error(json!({"id": "standup:1", "result": flattened}));

    assert!(message.contains("`snapshot`"), "{message}");
    assert!(
        message.contains("result type `session_snapshot`"),
        "{message}"
    );
    assert!(message.contains("snapshot version missing"), "{message}");
    assert!(message.contains("snapshot protocol missing"), "{message}");
}

#[test]
fn a_snapshot_that_is_not_an_object_is_an_error_too() {
    let _guard = env_lock();
    let message = contract_error(json!({
        "id": "standup:1",
        "result": {"type": "session_snapshot", "snapshot": []}
    }));

    assert!(
        message.contains("`snapshot` must be an object"),
        "{message}"
    );
}

#[test]
fn workspaces_panes_and_agents_are_required_arrays() {
    let _guard = env_lock();

    for field in ["workspaces", "panes", "agents"] {
        let mut missing = captured()["result"].clone();
        missing["snapshot"]
            .as_object_mut()
            .expect("snapshot object")
            .remove(field);
        let message = contract_error(json!({"id": "standup:1", "result": missing}));
        assert!(message.contains(&format!("snapshot.{field}")), "{message}");
        assert!(message.contains("snapshot version `0.8.0`"), "{message}");
        assert!(message.contains("snapshot protocol `19`"), "{message}");

        let mut wrong = captured()["result"].clone();
        wrong["snapshot"][field] = json!({});
        let message = contract_error(json!({"id": "standup:1", "result": wrong}));
        assert!(message.contains(&format!("snapshot.{field}")), "{message}");
        assert!(message.contains("must be an array"), "{message}");
        assert!(
            message.contains("result type `session_snapshot`"),
            "{message}"
        );
    }
}

// ---------------------------------------------------------------------------
// Trap 2: `worktree` is not how you find repositories
// ---------------------------------------------------------------------------

#[test]
fn most_of_the_captured_workspaces_have_no_worktree_key_at_all() {
    // Guards the fixture itself. If a future capture happened to have a
    // `worktree` on every workspace, the tests below would still pass while
    // testing nothing.
    let snapshot = captured_snapshot();
    let workspaces = snapshot["workspaces"].as_array().expect("workspaces");
    let tracked = workspaces
        .iter()
        .filter(|w| w.get("worktree").is_some())
        .count();
    assert_eq!(workspaces.len(), 10);
    assert_eq!(
        tracked, 3,
        "seven of the ten captured workspaces have no worktree key; \
         a digest built only from `worktree` would omit most of the day"
    );
}

#[test]
fn a_workspace_with_no_worktree_key_still_reports_its_pane_cwd() {
    let workspaces = reduce(&captured_snapshot());

    // Five of the untracked workspaces are ordinary git checkouts.
    for (id, path) in [
        ("w15", "/home/dev/code/atlas"),
        ("w16", "/home/dev/code/beacon"),
        ("w1B", "/home/dev/code/cobalt"),
        ("w1C", "/home/dev/code/dynamo"),
        ("w1D", "/home/dev/code/ember"),
    ] {
        assert_eq!(
            workspace(&workspaces, id).paths,
            vec![PathBuf::from(path)],
            "{id} has no worktree key and would otherwise vanish from the digest"
        );
    }
}

#[test]
fn the_tracked_checkout_comes_first_and_duplicate_pane_cwds_collapse() {
    let workspaces = reduce(&captured_snapshot());

    // Two panes, both sitting in the workspace's own linked worktree: the
    // checkout path and both cwds are the same directory, and it is listed once.
    let tracked = workspace(&workspaces, "wE");
    assert_eq!(
        tracked.paths,
        vec![PathBuf::from(
            "/home/dev/.herdr/worktrees/orchard/fix-slow-fetch"
        )]
    );

    // Four panes, one directory, no worktree key.
    assert_eq!(workspace(&workspaces, "w15").paths.len(), 1);
}

#[test]
fn a_tracked_checkout_is_listed_before_a_pane_that_wandered_elsewhere() {
    // Synthesised from the capture rather than written from scratch: the only
    // change is where one pane's cwd points.
    let mut snapshot = captured_snapshot();
    snapshot["panes"][3]["cwd"] = json!("/home/dev/code/orchard");

    let workspaces = reduce(&snapshot);
    let tracked = workspace(&workspaces, "wE");

    assert_eq!(
        tracked.paths,
        vec![
            PathBuf::from("/home/dev/.herdr/worktrees/orchard/fix-slow-fetch"),
            PathBuf::from("/home/dev/code/orchard"),
        ],
        "the tracked checkout leads, the wandering pane still counts"
    );
}

#[test]
fn foreground_cwd_is_never_used() {
    let workspaces = reduce(&captured_snapshot());

    for workspace in &workspaces {
        for path in &workspace.paths {
            assert_ne!(
                path,
                &PathBuf::from(FOREGROUND_TRAP),
                "{} picked up a foreground_cwd",
                workspace.workspace_id
            );
        }
    }
    // The two panes whose foreground_cwd was inside a pyright install were both
    // really sitting somewhere useful.
    assert_eq!(
        workspace(&workspaces, "wM").paths,
        vec![PathBuf::from("/home/dev/code")]
    );
    assert_eq!(
        workspace(&workspaces, "w16").paths,
        vec![PathBuf::from("/home/dev/code/beacon")]
    );
}

#[test]
fn a_workspace_with_nowhere_to_look_is_dropped() {
    let mut snapshot = captured_snapshot();
    // herdr reports absent context as an empty string, not as a missing key.
    snapshot["panes"][0]["cwd"] = json!("");

    let workspaces = reduce(&snapshot);

    assert_eq!(workspaces.len(), 9);
    assert!(workspaces.iter().all(|w| w.workspace_id != "wM"));
}

#[test]
fn a_dot_component_in_a_reported_path_is_dropped() {
    // A workspace created with `--cwd .` comes back with the `.` still on it.
    let mut snapshot = captured_snapshot();
    snapshot["panes"][7]["cwd"] = json!("/home/dev/code/atlas/.");

    let workspaces = reduce(&snapshot);

    assert_eq!(
        workspace(&workspaces, "w15").paths,
        vec![PathBuf::from("/home/dev/code/atlas")],
        "the tidied path must also collapse with the other three panes' cwds"
    );
}

/// Found by running the real binary against the live session. A workspace whose
/// directory had been removed underneath it reported `worktree.checkout_path` as
/// the directory and its pane's `cwd` as the same directory plus the kernel's
/// `(deleted)` marker. Those reduced to two candidate paths, and the digest
/// printed the same "is not a git checkout" note twice — both lines truncated at
/// the same column, so they read as an exact duplicate.
#[test]
fn a_deleted_cwd_does_not_become_a_second_directory() {
    let mut snapshot = captured_snapshot();
    // wE is the workspace with a `worktree`; its two panes sit in the checkout.
    snapshot["panes"][3]["cwd"] =
        json!("/home/dev/.herdr/worktrees/orchard/fix-slow-fetch (deleted)");
    snapshot["panes"][4]["cwd"] =
        json!("/home/dev/.herdr/worktrees/orchard/fix-slow-fetch (deleted)");

    let workspaces = reduce(&snapshot);

    assert_eq!(
        workspace(&workspaces, "wE").paths,
        vec![PathBuf::from(
            "/home/dev/.herdr/worktrees/orchard/fix-slow-fetch"
        )],
        "the marker is the kernel's annotation, not a different directory"
    );
}

#[test]
fn a_deleted_cwd_is_still_reported_when_it_is_the_only_thing_there_is() {
    // No `worktree` key on w15, and every pane marked deleted: the digest must
    // still name the directory, under the name the user would recognise.
    let mut snapshot = captured_snapshot();
    for pane in 7..=10 {
        snapshot["panes"][pane]["cwd"] = json!("/home/dev/code/atlas (deleted)");
    }

    let workspaces = reduce(&snapshot);

    assert_eq!(
        workspace(&workspaces, "w15").paths,
        vec![PathBuf::from("/home/dev/code/atlas")]
    );
}

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

#[test]
fn agent_session_value_is_read_out_of_the_object() {
    let workspaces = reduce(&captured_snapshot());
    let atlas = workspace(&workspaces, "w15");

    let classifier = &atlas.agents[1];
    assert_eq!(classifier.pane_id, "w15:p2");
    assert_eq!(classifier.name.as_deref(), Some("atlas-classifier"));
    assert_eq!(classifier.program.as_deref(), Some("claude"));
    assert_eq!(
        classifier.session_id.as_deref(),
        Some("00000000-0000-0000-7777-7777d5cc8777"),
        "`agent_session` is an object on the wire; the id is its `value`"
    );
    assert_eq!(classifier.status.as_deref(), Some("working"));
}

#[test]
fn an_agent_the_user_never_named_still_reports_its_program() {
    let workspaces = reduce(&captured_snapshot());
    let overview = workspace(&workspaces, "wM");

    assert_eq!(overview.agents.len(), 1);
    let agent = &overview.agents[0];
    assert_eq!(agent.name, None, "absent, not an empty string");
    assert_eq!(agent.program.as_deref(), Some("claude"));
    assert_eq!(agent.display(), Some("claude"));
}

#[test]
fn an_agent_with_no_session_reports_none_rather_than_a_placeholder() {
    let workspaces = reduce(&captured_snapshot());
    let fetch = workspace(&workspaces, "wE");

    let opencode = &fetch.agents[0];
    assert_eq!(opencode.pane_id, "wE:p1");
    assert_eq!(opencode.program.as_deref(), Some("opencode"));
    assert_eq!(opencode.name.as_deref(), Some("slow-fetch"));
    assert_eq!(
        opencode.session_id, None,
        "the opencode row carries no agent_session at all"
    );
}

#[test]
fn a_pane_with_no_agent_is_not_counted_as_one() {
    let workspaces = reduce(&captured_snapshot());
    let cobalt = workspace(&workspaces, "w1B");

    // Two panes, one of them an empty shell with `agent_status: unknown`.
    assert_eq!(cobalt.agents.len(), 1);
    assert_eq!(cobalt.agents[0].pane_id, "w1B:p1");
}

#[test]
fn a_pane_with_an_agent_but_no_agents_row_still_counts() {
    // The join is on pane_id, so drop the row and leave the pane alone.
    let mut snapshot = captured_snapshot();
    let agents: Vec<Value> = snapshot["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .filter(|row| row["pane_id"] != json!("w15:p2"))
        .cloned()
        .collect();
    snapshot["agents"] = Value::Array(agents);

    let workspaces = reduce(&snapshot);
    let atlas = workspace(&workspaces, "w15");

    assert_eq!(atlas.agents.len(), 4, "the agent still ran");
    let synthesised = &atlas.agents[1];
    assert_eq!(synthesised.pane_id, "w15:p2");
    assert_eq!(synthesised.program.as_deref(), Some("claude"));
    assert_eq!(
        synthesised.name, None,
        "only the user's own label is genuinely unavailable"
    );
    assert_eq!(synthesised.display(), Some("claude"));
}

#[test]
fn agents_are_ordered_by_pane_so_the_digest_is_stable() {
    let mut snapshot = captured_snapshot();
    let mut agents = snapshot["agents"].as_array().expect("agents").clone();
    agents.reverse();
    snapshot["agents"] = Value::Array(agents);

    let workspaces = reduce(&snapshot);

    assert_eq!(
        workspace(&workspaces, "w15")
            .agents
            .iter()
            .map(|a| a.pane_id.as_str())
            .collect::<Vec<_>>(),
        ["w15:p1", "w15:p2", "w15:p3", "w15:p4"],
        "herdr's ordering must not leak into the digest"
    );
}

#[test]
fn every_captured_agent_lands_in_exactly_one_workspace() {
    let snapshot = captured_snapshot();
    let rows = snapshot["agents"].as_array().expect("agents").len();

    let workspaces = reduce(&snapshot);
    let mut seen: Vec<String> = workspaces
        .iter()
        .flat_map(|w| w.agents.iter().map(|a| a.pane_id.clone()))
        .collect();
    let total = seen.len();
    seen.sort();
    seen.dedup();

    assert_eq!(rows, 18);
    assert_eq!(total, 18, "no agent duplicated across workspaces");
    assert_eq!(seen.len(), 18, "no agent dropped");
}

/// Attribution needs a directory, not a workspace. Every agent row in the live
/// capture carries its own `cwd`, and that is what makes "this agent worked
/// here" a fact instead of an inference from the workspace it belongs to.
#[test]
fn every_captured_agent_carries_the_directory_it_worked_in() {
    let workspaces = reduce(&captured_snapshot());
    let atlas = workspace(&workspaces, "w15");

    assert_eq!(atlas.agents.len(), 4);
    for agent in &atlas.agents {
        assert_eq!(
            agent.cwd.as_deref(),
            Some(Path::new("/home/dev/code/atlas")),
            "{} lost its directory",
            agent.pane_id
        );
    }
}

/// The protocol marks `agents[].cwd` optional, so the pane the row names is the
/// second source — the same pane whose `cwd` already contributes to
/// `workspace.paths`.
#[test]
fn an_agent_row_with_no_cwd_falls_back_to_its_pane() {
    let mut snapshot = captured_snapshot();
    for row in snapshot["agents"].as_array_mut().expect("agents") {
        if row["pane_id"] == json!("w15:p2") {
            row.as_object_mut().expect("row").remove("cwd");
        }
    }

    let workspaces = reduce(&snapshot);
    let agent = &workspace(&workspaces, "w15").agents[1];

    assert_eq!(agent.pane_id, "w15:p2");
    assert_eq!(
        agent.cwd.as_deref(),
        Some(Path::new("/home/dev/code/atlas")),
        "the pane it names knows where it was"
    );
}

/// And when neither says, the answer is unknown. A directory invented here
/// would become a credit in the digest, which is the whole failure #19 is about.
#[test]
fn an_agent_with_no_directory_anywhere_reports_none() {
    let mut snapshot = captured_snapshot();
    for row in snapshot["agents"].as_array_mut().expect("agents") {
        if row["pane_id"] == json!("w15:p2") {
            row.as_object_mut().expect("row").remove("cwd");
        }
    }
    for pane in snapshot["panes"].as_array_mut().expect("panes") {
        if pane["pane_id"] == json!("w15:p2") {
            pane.as_object_mut().expect("pane").remove("cwd");
        }
    }

    let workspaces = reduce(&snapshot);
    let agent = &workspace(&workspaces, "w15").agents[1];

    assert_eq!(agent.pane_id, "w15:p2");
    assert_eq!(agent.cwd, None, "absent, never guessed");
}

/// A workspace whose panes straddle two checkouts is the shape the whole bug
/// lives in, and the live capture has none — all ten of its workspaces sit in a
/// single directory. So it is built here, from the capture rather than from
/// imagination: one of `w15`'s four panes, and the agent in it, moved to a
/// sibling worktree.
#[test]
fn a_workspace_can_straddle_two_directories_and_each_agent_keeps_its_own() {
    let mut snapshot = captured_snapshot();
    let moved = json!("/home/dev/code/atlas-wt");
    for row in snapshot["agents"].as_array_mut().expect("agents") {
        if row["pane_id"] == json!("w15:p4") {
            row["cwd"] = moved.clone();
        }
    }
    for pane in snapshot["panes"].as_array_mut().expect("panes") {
        if pane["pane_id"] == json!("w15:p4") {
            pane["cwd"] = moved.clone();
        }
    }

    let workspaces = reduce(&snapshot);
    let atlas = workspace(&workspaces, "w15");

    assert_eq!(
        atlas.paths,
        vec![
            PathBuf::from("/home/dev/code/atlas"),
            PathBuf::from("/home/dev/code/atlas-wt"),
        ],
        "both directories are candidates"
    );
    assert_eq!(
        atlas
            .agents
            .iter()
            .map(|a| a.cwd.as_deref().and_then(Path::to_str).unwrap_or("?"))
            .collect::<Vec<_>>(),
        [
            "/home/dev/code/atlas",
            "/home/dev/code/atlas",
            "/home/dev/code/atlas",
            "/home/dev/code/atlas-wt",
        ],
        "the roster is the workspace's; the directory is each agent's own"
    );
}
