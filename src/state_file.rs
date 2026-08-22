//! Replacing a file in the plugin's own state directory, atomically.
//!
//! Two callers, one rule, one explanation. The `--since-last` marker and the
//! plumbing cache are both small JSON files that a later run *parses*, and a
//! half-written one is worse than a missing one: the marker falls back to
//! today's window without saying why, and the cache would have to be thrown away
//! wholesale. So the bytes go to a temporary file beside the target, are put on
//! disk, and are then renamed over it.
//!
//! The `sync_all` is not decoration. `rename` is atomic with respect to a crash
//! only once the contents it names are durable; without it, a machine that loses
//! power mid-run can leave the new name pointing at a partially written file,
//! which is exactly the state this exists to make impossible.
//!
//! Nothing here ever touches a user's repository. These paths are the plugin's
//! own state directory, which is the only place standup writes at all.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Result;

/// Writes `bytes` to `path`, replacing it atomically, creating the directory.
///
/// `label` names the file in errors and in the temporary's own name, so two
/// files being replaced in one process cannot collide and a leftover temporary
/// says what it was.
pub fn replace(path: &Path, label: &str, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|err| {
        format!(
            "could not create the state directory {}: {err}",
            dir.display()
        )
    })?;

    let temp = temp_beside(dir, label);
    if let Err(err) = write_all(&temp, bytes) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not write {}: {err}", temp.display()).into());
    }
    if let Err(err) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not replace {}: {err}", path.display()).into());
    }
    Ok(())
}

fn temp_beside(dir: &Path, label: &str) -> PathBuf {
    dir.join(format!(
        ".{label}.{}.{}.tmp",
        std::process::id(),
        next_temp_id()
    ))
}

fn write_all(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    // The rename is only atomic with respect to a crash if the bytes are on
    // disk before it happens.
    file.sync_all()
}

/// Distinguishes the temporary files of two writes in one process, so a run that
/// replaces two files cannot have one write clobber the other's temporary.
fn next_temp_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
