//! Pure state machine for the interactive digest pane.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::model::{CheckoutDigest, Digest};

/// Reporting windows available without leaving the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    Today,
    Yesterday,
    SinceLast,
}

impl WindowKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Yesterday => "yesterday and today",
            Self::SinceLast => "since last view",
        }
    }
}

/// Terminal input translated into policy-free pane events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Escape,
    Quit,
    Today,
    Yesterday,
    SinceLast,
    Refresh,
    Focus(usize),
    Other,
}

/// I/O requested by a state transition. The runtime performs it and adopts the
/// result; the state machine itself never scans git or writes the marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    None,
    Load(WindowKind),
    Refresh,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub error: bool,
}

/// Everything needed to render and drive one digest pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestPane {
    pub digest: Digest,
    pub active: WindowKind,
    pub cursor: usize,
    pub expanded: BTreeSet<PathBuf>,
    pub message: Option<Message>,
    pub intent: Intent,
}

impl DigestPane {
    pub fn new(digest: Digest) -> Self {
        Self {
            digest,
            active: WindowKind::Today,
            cursor: 0,
            expanded: BTreeSet::new(),
            message: None,
            intent: Intent::None,
        }
    }

    pub fn row_count(&self) -> usize {
        display_checkouts(&self.digest).len()
    }

    pub fn checkout(&self, index: usize) -> Option<&CheckoutDigest> {
        display_checkouts(&self.digest).get(index).copied()
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.checkout(self.cursor)
            .map(|checkout| checkout.report.path.as_path())
    }
}

/// Checkouts in exactly the grouping and ordering used by the current report:
/// repositories first, then their checkouts.
pub fn display_checkouts(digest: &Digest) -> Vec<&CheckoutDigest> {
    crate::render::sorted_repos(digest)
        .into_iter()
        .flat_map(crate::render::sorted_checkouts)
        .collect()
}

/// Applies one input event without performing I/O.
pub fn apply(mut pane: DigestPane, key: Key) -> DigestPane {
    let rows = pane.row_count();
    pane.cursor = if rows == 0 {
        0
    } else {
        pane.cursor.min(rows - 1)
    };

    match key {
        Key::Up => pane.cursor = pane.cursor.saturating_sub(1),
        Key::Down => {
            if rows > 0 {
                pane.cursor = (pane.cursor + 1).min(rows - 1);
            }
        }
        Key::Focus(index) if index < rows => pane.cursor = index,
        Key::Enter => toggle_current(&mut pane),
        Key::Escape => {
            let collapsed = pane
                .selected_path()
                .map(Path::to_path_buf)
                .is_some_and(|path| pane.expanded.remove(&path));
            if !collapsed {
                pane.intent = Intent::Quit;
            }
        }
        Key::Quit => pane.intent = Intent::Quit,
        Key::Today if pane.active != WindowKind::Today => {
            pane.intent = Intent::Load(WindowKind::Today)
        }
        Key::Yesterday if pane.active != WindowKind::Yesterday => {
            pane.intent = Intent::Load(WindowKind::Yesterday)
        }
        Key::SinceLast if pane.active != WindowKind::SinceLast => {
            pane.intent = Intent::Load(WindowKind::SinceLast)
        }
        Key::Refresh => pane.intent = Intent::Refresh,
        Key::Today
        | Key::Yesterday
        | Key::SinceLast
        | Key::Focus(_)
        | Key::Other => {}
    }
    pane
}

fn toggle_current(pane: &mut DigestPane) {
    let Some(path) = pane.selected_path().map(Path::to_path_buf) else {
        return;
    };
    if !pane.expanded.remove(&path) {
        pane.expanded.insert(path);
    }
}

/// Adopts a successful scan while keeping cursor and expansion by checkout path.
pub fn adopt(mut pane: DigestPane, digest: Digest, active: WindowKind) -> DigestPane {
    let selected = pane.selected_path().map(Path::to_path_buf);
    pane.digest = digest;
    pane.active = active;
    pane.intent = Intent::None;
    pane.message = None;

    let paths: BTreeSet<PathBuf> = display_checkouts(&pane.digest)
        .into_iter()
        .map(|checkout| checkout.report.path.clone())
        .collect();
    pane.expanded.retain(|path| paths.contains(path));
    pane.cursor = selected
        .as_deref()
        .and_then(|path| {
            display_checkouts(&pane.digest)
                .iter()
                .position(|checkout| checkout.report.path == path)
        })
        .unwrap_or(0);
    pane
}

/// Keeps the last good digest visible when a scan or marker write fails.
pub fn fail(mut pane: DigestPane, message: impl Into<String>) -> DigestPane {
    pane.intent = Intent::None;
    pane.message = Some(Message {
        text: message.into(),
        error: true,
    });
    pane
}
