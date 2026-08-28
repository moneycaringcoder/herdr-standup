use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

use standup::model::{
    CheckoutDigest, CheckoutReport, Churn, Commit, Digest, Dirty, Head, Landed, RepoDigest,
    RepoKey, Stamp, Tracking, Unpushed, Window, WindowSource, WorkspaceRef, SCHEMA_VERSION,
};
use standup::tui::view::{self, MouseMap};
use standup::tui::{
    adopt, advances_marker, apply, map_key_event, DigestPane, Intent, Key, WindowKind,
};

fn stamp(epoch: i64, local: &str) -> Stamp {
    Stamp {
        epoch,
        local: local.to_string(),
        zone: "UTC +0000".to_string(),
        offset_seconds: Some(0),
    }
}

struct CheckoutSpec<'a> {
    path: &'a str,
    workspace_number: u64,
    workspace: &'a str,
    branch: &'a str,
    subject: &'a str,
    committed: i64,
    landed: Landed,
    dirty: Dirty,
}

fn checkout(spec: CheckoutSpec<'_>) -> CheckoutDigest {
    let CheckoutSpec {
        path,
        workspace_number,
        workspace,
        branch,
        subject,
        committed,
        landed,
        dirty,
    } = spec;
    let churn = Churn {
        files: 2,
        excluded: 0,
        insertions: 14,
        deletions: 3,
    };
    CheckoutDigest {
        report: CheckoutReport {
            path: PathBuf::from(path),
            repo_key: RepoKey("/repo/.git".to_string()),
            repo_root: PathBuf::from("/repo"),
            is_linked_worktree: true,
            head: Head::Branch {
                name: branch.to_string(),
                oid: format!("{committed:040x}"),
            },
            commits: vec![Commit {
                oid: format!("{committed:040x}"),
                author: "Ada".to_string(),
                committed: stamp(committed, "2026-08-28 09:15"),
                subject: subject.to_string(),
                is_merge: false,
                insertions: churn.insertions,
                deletions: churn.deletions,
                files: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
            }],
            churn,
            dirty,
            tracking: Tracking::Upstream {
                name: format!("origin/{branch}"),
                ahead: 0,
                behind: 0,
            },
            landed,
            unpushed: Unpushed::Commits { count: 0 },
            problems: Vec::new(),
        },
        workspaces: vec![WorkspaceRef {
            workspace_id: format!("workspace-{workspace_number}"),
            label: workspace.to_string(),
            number: Some(workspace_number),
            paths: vec![PathBuf::from(path)],
            agents: Vec::new(),
            agent_status: Some("working".to_string()),
        }],
        agents: Vec::new(),
    }
}

fn digest() -> Digest {
    let merged = checkout(CheckoutSpec {
        path: "/repo/landed",
        workspace_number: 1,
        workspace: "landed-work",
        branch: "feat/landed",
        subject: "Ship digest pane",
        committed: 200,
        landed: Landed::Merged {
            into: "origin/main".to_string(),
        },
        dirty: Dirty::default(),
    });
    let unlanded = checkout(CheckoutSpec {
        path: "/repo/wip",
        workspace_number: 2,
        workspace: "work-in-progress",
        branch: "feat/wip",
        subject: "Keep working",
        committed: 100,
        landed: Landed::NotMerged {
            into: "origin/main".to_string(),
        },
        dirty: Dirty {
            tracked_changed: 1,
            insertions: 4,
            ..Dirty::default()
        },
    });
    Digest {
        schema: SCHEMA_VERSION,
        generated_at: stamp(300, "2026-08-28 12:34"),
        window: Window {
            since: stamp(0, "2026-08-28 00:00"),
            until: None,
            source: WindowSource::Default,
        },
        repos: vec![RepoDigest {
            repo_key: RepoKey("/repo/.git".to_string()),
            name: "example".to_string(),
            repo_root: PathBuf::from("/repo"),
            checkouts: vec![unlanded, merged],
            commits: 2,
            churn: Churn {
                files: 4,
                excluded: 0,
                insertions: 28,
                deletions: 6,
            },
            active_days: 1,
        }],
        notes: Vec::new(),
    }
}

struct DrawnPane {
    text: String,
    buffer: Buffer,
    mouse: MouseMap,
}

fn draw(pane: &DigestPane, width: u16, height: u16) -> DrawnPane {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let mut mouse = MouseMap::default();
    terminal
        .draw(|frame| mouse = view::render(frame, pane))
        .expect("digest frame");
    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for row in 0..buffer.area.height {
        for column in 0..buffer.area.width {
            text.push_str(
                buffer
                    .cell((column, row))
                    .expect("cell inside test buffer")
                    .symbol(),
            );
        }
        text.push('\n');
    }
    DrawnPane {
        text,
        buffer,
        mouse,
    }
}

fn text_position(buffer: &Buffer, needle: &str) -> (u16, u16) {
    let wanted = needle.chars().map(|c| c.to_string()).collect::<Vec<_>>();
    for row in 0..buffer.area.height {
        for column in 0..buffer.area.width {
            if column as usize + wanted.len() > buffer.area.width as usize {
                continue;
            }
            let found = (0..wanted.len())
                .map(|offset| {
                    buffer
                        .cell((column + offset as u16, row))
                        .expect("cell inside test buffer")
                        .symbol()
                })
                .zip(&wanted)
                .all(|(actual, expected)| actual == expected);
            if found {
                return (column, row);
            }
        }
    }
    panic!("text {needle:?} was not rendered");
}

#[test]
fn window_header_names_active_window_and_resolved_range() {
    let rendered = draw(&DigestPane::new(digest()), 120, 24).text;
    assert!(rendered.contains("today · 2026-08-28 00:00 UTC +0000 → 2026-08-28 12:34 UTC +0000"));
}

#[test]
fn workspace_rows_expand_commits_inline() {
    let pane = DigestPane::new(digest());
    let collapsed = draw(&pane, 120, 24).text;
    assert!(collapsed.contains("#1 landed-work"), "{collapsed}");
    assert!(collapsed.contains("#2 work-in-progress"), "{collapsed}");
    assert!(!collapsed.contains("Ship digest pane"), "{collapsed}");

    let expanded = apply(pane, Key::Enter);
    let rendered = draw(&expanded, 120, 24).text;
    assert!(rendered.contains("Ship digest pane"), "{rendered}");
    assert!(rendered.contains('▾'), "{rendered}");
}

#[test]
fn landed_and_unlanded_tags_are_bold_and_colored() {
    let drawn = draw(&DigestPane::new(digest()), 120, 24);
    for (label, color) in [("[MERGED]", Color::Green), ("[UNLANDED]", Color::Yellow)] {
        let position = text_position(&drawn.buffer, label);
        let cell = drawn.buffer.cell(position).expect("tag cell");
        assert_eq!(cell.fg, color, "{label}");
        assert!(cell.modifier.contains(Modifier::BOLD), "{label}");
    }
    let dirty = text_position(&drawn.buffer, "[DIRTY]");
    assert_eq!(
        drawn.buffer.cell(dirty).expect("dirty tag").fg,
        Color::Yellow
    );
}

#[test]
fn cursor_reverses_the_whole_workspace_row() {
    let drawn = draw(&DigestPane::new(digest()), 120, 24);
    let (_, row) = text_position(&drawn.buffer, "▸");
    for column in 1..drawn.buffer.area.width - 1 {
        assert!(
            drawn
                .buffer
                .cell((column, row))
                .expect("cursor row cell")
                .modifier
                .contains(Modifier::REVERSED),
            "column {column} was not reversed"
        );
    }
}

#[test]
fn narrow_view_keeps_workspace_volume_and_verdict_visible() {
    let rendered = draw(&DigestPane::new(digest()), 38, 16).text;
    assert!(rendered.contains("▸"), "{rendered}");
    assert!(rendered.contains("1c"), "{rendered}");
    assert!(rendered.contains("[MERGED]"), "{rendered}");
    assert!(rendered.contains("t/y/l"), "{rendered}");
}

#[test]
fn crossterm_keys_map_to_digest_events() {
    let cases = [
        (KeyCode::Up, KeyModifiers::NONE, Key::Up),
        (KeyCode::Down, KeyModifiers::NONE, Key::Down),
        (KeyCode::Char('k'), KeyModifiers::NONE, Key::Up),
        (KeyCode::Char('j'), KeyModifiers::NONE, Key::Down),
        (KeyCode::Enter, KeyModifiers::NONE, Key::Enter),
        (KeyCode::Esc, KeyModifiers::NONE, Key::Escape),
        (KeyCode::Char('q'), KeyModifiers::NONE, Key::Quit),
        (KeyCode::Char('t'), KeyModifiers::NONE, Key::Today),
        (KeyCode::Char('y'), KeyModifiers::NONE, Key::Yesterday),
        (KeyCode::Char('l'), KeyModifiers::NONE, Key::SinceLast),
        (KeyCode::Char('R'), KeyModifiers::NONE, Key::Refresh),
        (KeyCode::Char('c'), KeyModifiers::CONTROL, Key::Quit),
    ];
    for (code, modifiers, expected) in cases {
        assert_eq!(
            map_key_event(KeyEvent::new(code, modifiers)),
            Some(expected)
        );
    }
    let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    release.kind = KeyEventKind::Release;
    assert_eq!(map_key_event(release), None);
}

#[test]
fn mouse_hit_testing_focuses_rows_without_expanding_them() {
    let pane = DigestPane::new(digest());
    let drawn = draw(&pane, 120, 24);
    let (column, row) = text_position(&drawn.buffer, "▸");
    assert_eq!(drawn.mouse.cursor_at(column, row), Some(0));
    assert_eq!(drawn.mouse.cursor_at(0, 0), None);

    let focused = apply(pane, Key::Focus(1));
    assert_eq!(focused.cursor, 1);
    assert!(focused.expanded.is_empty());
}

#[test]
fn state_switches_windows_without_terminal_io() {
    let pane = DigestPane::new(digest());
    let requested = apply(pane, Key::Yesterday);
    assert_eq!(requested.intent, Intent::Load(WindowKind::Yesterday));

    let yesterday = adopt(requested, digest(), WindowKind::Yesterday);
    assert_eq!(yesterday.active, WindowKind::Yesterday);
    assert_eq!(
        apply(yesterday.clone(), Key::Yesterday).intent,
        Intent::None
    );
    assert_eq!(
        apply(yesterday, Key::SinceLast).intent,
        Intent::Load(WindowKind::SinceLast)
    );
}

#[test]
fn tui_marker_advances_only_when_entering_since_last() {
    assert!(advances_marker(Intent::Load(WindowKind::SinceLast)));
    assert!(!advances_marker(Intent::Load(WindowKind::Today)));
    assert!(!advances_marker(Intent::Load(WindowKind::Yesterday)));
    assert!(!advances_marker(Intent::Refresh));
    assert!(!advances_marker(Intent::Quit));
    assert!(!advances_marker(Intent::None));
}
