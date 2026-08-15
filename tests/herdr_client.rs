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

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use standup::herdr::{error_code, reduce_snapshot, Herdr};
use standup::model::WorkspaceRef;

/// An anonymised `herdr api snapshot` response, structurally identical to the
/// live capture it was made from. Stored pretty-printed so it can be reviewed;
/// re-serialised onto one line before it goes on the wire, which changes the
/// whitespace and nothing else.
const CAPTURE: &str = include_str!("snapshots/session_snapshot.json");

/// The `foreground_cwd` of two panes in the capture, both of which had a real
/// `cwd` in a repository at the time. Reading the wrong field puts this in the
/// digest.
const FOREGROUND_TRAP: &str =
    "/home/dev/.local/share/mise/installs/node/24.18.0/lib/node_modules/pyright/dist";

fn captured() -> Value {
    serde_json::from_str(CAPTURE).expect("the captured fixture is JSON")
}

/// The whole response envelope, framed as one line the way herdr frames it.
fn captured_line() -> String {
    serde_json::to_string(&captured()).expect("re-serialise")
}

/// Just the inner snapshot object, for driving `reduce_snapshot` with no socket.
fn captured_snapshot() -> Value {
    captured()["result"]["snapshot"].clone()
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

/// What the server does with one connection.
#[derive(Clone)]
enum Reply {
    /// Answer, then close — the real server's behaviour.
    Line(String),
    /// Read the request and close without answering, which is what a client
    /// sees when it lands on a socket the server is tearing down.
    Eof,
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
fn a_malformed_line_is_a_transport_failure_and_is_retried() {
    let _guard = env_lock();
    let server = TestServer::start(vec![
        Reply::Line("{\"id\": \"standup:1\", \"result\":".to_string()),
        captured_reply(),
    ]);
    let mut client = server.client();

    let workspaces = client.workspaces().expect("the retry should succeed");

    assert_eq!(workspaces.len(), 10);
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn two_malformed_lines_fail_and_say_so() {
    let _guard = env_lock();
    let garbage = Reply::Line("not json at all".to_string());
    let server = TestServer::start(vec![garbage.clone(), garbage]);
    let mut client = server.client();

    let err = client.workspaces().expect_err("both attempts are unusable");

    assert!(
        err.to_string().contains("malformed"),
        "the message must name the problem: {err}"
    );
    assert_eq!(error_code(&*err), None);
}

#[test]
fn a_response_with_neither_result_nor_error_is_a_transport_failure() {
    let _guard = env_lock();
    let empty = Reply::Line(json!({"id": "standup:1"}).to_string());
    let server = TestServer::start(vec![empty.clone(), empty]);
    let mut client = server.client();

    let err = client.workspaces().expect_err("nothing usable came back");

    assert!(
        err.to_string().contains("neither result nor error"),
        "{err}"
    );
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
fn the_live_capture_is_read_through_the_nested_snapshot_object() {
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
    let server = TestServer::start(vec![Reply::Line(
        json!({"id": "standup:1", "result": flattened}).to_string(),
    )]);
    let mut client = server.client();

    let err = client
        .workspaces()
        .expect_err("a missing `snapshot` object must not read as an idle session");

    assert!(
        err.to_string().contains("snapshot"),
        "the message must name what is missing: {err}"
    );
    assert!(
        err.to_string().contains("session_snapshot"),
        "the message must name the result type it did get: {err}"
    );
}

#[test]
fn a_snapshot_that_is_not_an_object_is_an_error_too() {
    let _guard = env_lock();
    let server = TestServer::start(vec![Reply::Line(
        json!({
            "id": "standup:1",
            "result": {"type": "session_snapshot", "snapshot": []}
        })
        .to_string(),
    )]);
    let mut client = server.client();

    let err = client.workspaces().expect_err("an array is not a snapshot");

    assert!(err.to_string().contains("snapshot"), "{err}");
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
    let workspaces = reduce_snapshot(&captured_snapshot());

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
    let workspaces = reduce_snapshot(&captured_snapshot());

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

    let workspaces = reduce_snapshot(&snapshot);
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
    let workspaces = reduce_snapshot(&captured_snapshot());

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

    let workspaces = reduce_snapshot(&snapshot);

    assert_eq!(workspaces.len(), 9);
    assert!(workspaces.iter().all(|w| w.workspace_id != "wM"));
}

#[test]
fn a_dot_component_in_a_reported_path_is_dropped() {
    // A workspace created with `--cwd .` comes back with the `.` still on it.
    let mut snapshot = captured_snapshot();
    snapshot["panes"][7]["cwd"] = json!("/home/dev/code/atlas/.");

    let workspaces = reduce_snapshot(&snapshot);

    assert_eq!(
        workspace(&workspaces, "w15").paths,
        vec![PathBuf::from("/home/dev/code/atlas")],
        "the tidied path must also collapse with the other three panes' cwds"
    );
}

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

#[test]
fn agent_session_value_is_read_out_of_the_object() {
    let workspaces = reduce_snapshot(&captured_snapshot());
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
    let workspaces = reduce_snapshot(&captured_snapshot());
    let overview = workspace(&workspaces, "wM");

    assert_eq!(overview.agents.len(), 1);
    let agent = &overview.agents[0];
    assert_eq!(agent.name, None, "absent, not an empty string");
    assert_eq!(agent.program.as_deref(), Some("claude"));
    assert_eq!(agent.display(), Some("claude"));
}

#[test]
fn an_agent_with_no_session_reports_none_rather_than_a_placeholder() {
    let workspaces = reduce_snapshot(&captured_snapshot());
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
    let workspaces = reduce_snapshot(&captured_snapshot());
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

    let workspaces = reduce_snapshot(&snapshot);
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

    let workspaces = reduce_snapshot(&snapshot);

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

    let workspaces = reduce_snapshot(&snapshot);
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
