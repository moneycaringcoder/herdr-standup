//! Interactive standup digest pane.
//!
//! [`state`] is the pure state machine, [`view`] owns terminal layout, and
//! [`run`] is the only module allowed to scan, write the since-last marker, or
//! change terminal modes.

mod run;
pub mod state;
pub mod view;

pub use run::{map_key_event, run_digest};
pub use state::{adopt, advances_marker, apply, DigestPane, Intent, Key, WindowKind};
