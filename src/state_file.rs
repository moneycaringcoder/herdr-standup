//! Coordinating and atomically replacing files in the plugin's state directory.
//!
//! Marker updates lock the directory itself before their read/compare/replace
//! transaction. That provides one stable advisory lock object without creating
//! a lock file whose deletion could split concurrent writers into separate lock
//! domains. The descriptor owns the lock and releases it by RAII.
//!
//! Two callers, one rule, one explanation. The `--since-last` marker and the
//! plumbing cache are both small JSON files that a later run *parses*, and a
//! half-written one is worse than a missing one: the marker falls back to
//! today's window without saying why, and the cache would have to be thrown away
//! wholesale. So the bytes go to a temporary file beside the target, are put on
//! disk, and are then renamed over it.
//!
//! The two `sync_all` calls are not decoration. The temporary file is synced
//! before the rename so the bytes are durable; the containing directory is
//! synced afterwards so the new name is durable. Losing either half to a power
//! failure can leave the state missing, reverted, or partially written.
//!
//! Nothing here ever touches a user's repository. These paths are the plugin's
//! own state directory, which is the only place standup writes at all.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Result;

/// Runs `action` while holding an exclusive advisory lock on `dir`.
///
/// The directory itself is the lock object, so locking never creates a
/// persistent artifact that another process could delete while it is in use.
/// The descriptor releases the lock on every return path.
pub fn with_directory_lock<T, F>(dir: &Path, action: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    std::fs::create_dir_all(dir).map_err(|err| {
        format!(
            "could not create the state directory {}: {err}",
            dir.display()
        )
    })?;
    let directory = File::open(dir).map_err(|err| {
        format!(
            "could not open the state directory {}: {err}",
            dir.display()
        )
    })?;
    lock_exclusive(&directory).map_err(|err| {
        format!(
            "could not lock the state directory {}: {err}",
            dir.display()
        )
    })?;

    action()
}

fn lock_exclusive(file: &File) -> std::io::Result<()> {
    retry_interrupted(|| {
        // SAFETY: `file` owns this descriptor for the duration of the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    })
}

fn retry_interrupted<F>(mut operation: F) -> std::io::Result<()>
where
    F: FnMut() -> std::io::Result<()>,
{
    loop {
        match operation() {
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

/// Writes `bytes` to `path`, replacing it atomically, creating the directory.
///
/// `label` names the file in errors and in the temporary's own name, so two
/// files being replaced in one process cannot collide and a leftover temporary
/// says what it was.
pub fn replace(path: &Path, label: &str, bytes: &[u8]) -> Result<()> {
    replace_with_directory_sync(path, label, bytes, sync_containing_directory)
}

fn replace_with_directory_sync<F>(
    path: &Path,
    label: &str,
    bytes: &[u8],
    sync_directory: F,
) -> Result<()>
where
    F: FnOnce(&Path) -> std::io::Result<()>,
{
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
    if let Err(err) = sync_directory(dir) {
        return Err(format!(
            "could not make replacement of {} durable by syncing containing directory {}: {err}",
            path.display(),
            dir.display()
        )
        .into());
    }
    Ok(())
}

fn sync_containing_directory(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "standup-state-file-{tag}-{}-{}",
            std::process::id(),
            next_temp_id()
        ));
        std::fs::create_dir(&dir).expect("create test directory");
        dir
    }

    #[test]
    fn containing_directory_is_synced_after_rename() {
        let dir = test_dir("sync-order");
        let path = dir.join("state.json");
        std::fs::write(&path, b"old").expect("write old state");
        let mut synced = false;

        replace_with_directory_sync(&path, "state", b"new", |synced_dir| {
            assert_eq!(synced_dir, dir);
            assert_eq!(
                std::fs::read(&path).expect("read replacement"),
                b"new".as_slice()
            );
            synced = true;
            Ok(())
        })
        .expect("replace state");

        assert!(synced);
        std::fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn containing_directory_sync_failure_is_returned_after_rename() {
        let dir = test_dir("sync-failure");
        let path = dir.join("state.json");

        let error = replace_with_directory_sync(&path, "state", b"new", |_| {
            Err(std::io::Error::other("directory sync refused"))
        })
        .expect_err("directory sync should fail")
        .to_string();

        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains(&dir.display().to_string()));
        assert!(error.contains("durable by syncing containing directory"));
        assert!(error.contains("directory sync refused"));
        assert_eq!(
            std::fs::read(&path).expect("read replacement"),
            b"new".as_slice()
        );
        std::fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn advisory_lock_retries_interrupted_acquisition() {
        let mut attempts = 0;

        retry_interrupted(|| {
            attempts += 1;
            if attempts < 3 {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            } else {
                Ok(())
            }
        })
        .expect("lock eventually acquired");

        assert_eq!(attempts, 3);
    }
}
