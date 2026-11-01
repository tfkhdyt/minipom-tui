//! Best-effort single-instance guard.
//!
//! Two copies of the app share one JSON store and the last save wins, so an
//! unnoticed second instance would silently eat the first one's writes. The
//! lock is a small file holding the owner's pid, removed on exit. A crash
//! leaves it behind, which the next start detects by looking the pid up in
//! `/proc`: a dead pid means the lock is stale and is taken over.

use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process;

/// Tries to own `path` as this instance's lock.
///
/// Errors are meant for display, so they read as sentences.
pub fn acquire(path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => write_pid(&mut file),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => acquire_exists(path),
        Err(e) => Err(format!("cannot create {}: {e}", path.display())),
    }
}

fn write_pid(mut file: impl Write) -> Result<(), String> {
    let ours = process::id();
    writeln!(file, "{ours}").map_err(|e| format!("cannot record our pid: {e}"))
}

/// The lock file is already there. A live pid means another copy of the app
/// owns it; a dead or unreadable one means the file survived a crash and can
/// be taken over.
fn acquire_exists(path: &Path) -> Result<(), String> {
    let pid = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    if pid.is_some_and(is_alive) {
        return Err(format!(
            "another instance (pid {}) may already be running against this data",
            pid.unwrap()
        ));
    }
    // Stale, or the pid could not be read: take the lock over.
    std::fs::remove_file(path).ok();
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => write_pid(&mut file),
        Err(_) => Err(format!("cannot take the lock at {}", path.display())),
    }
}

/// Whether `pid` belongs to a running process. Reading the process's entry in
/// procfs is the std-only way; elsewhere the lock is always treated as held,
/// because a file alone cannot tell a live owner from a crashed one.
#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    if pid == process::id() {
        return true;
    }
    pid != 0 && std::fs::read(format!("/proc/{pid}/cmdline")).is_ok()
}

#[cfg(not(unix))]
fn is_alive(_pid: u32) -> bool {
    true
}

/// Drops this instance's lock.
pub fn release(path: &Path) {
    std::fs::remove_file(path).ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn lock_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("minipom-tui-lock-test-{}", process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn acquire_blocks_a_second_instance_and_release_restores_it() {
        let path = lock_dir().join("data.json.lock");
        let _ = std::fs::remove_file(&path);

        acquire(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            process::id().to_string(),
            "the lock holds the owner's pid"
        );

        let again = acquire(&path).unwrap_err();
        assert!(
            again.contains("another instance"),
            "the second start must be refused: {again}"
        );

        release(&path);
        acquire(&path).unwrap_or_else(|e| panic!("the lock frees up again: {e}"));
        release(&path);
    }

    #[test]
    fn a_stale_lock_is_taken_over() {
        let path = lock_dir().join("stale.lock");
        let _ = std::fs::remove_file(&path);

        // A pid that cannot exist, so the file reads as stale.
        std::fs::write(&path, b"0").unwrap();
        acquire(&path).unwrap_or_else(|e| panic!("a dead pid's lock is free: {e}"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            process::id().to_string(),
            "the takeover records the new owner"
        );
        release(&path);
    }

    #[test]
    #[cfg(unix)]
    fn a_live_pid_holds_the_lock() {
        assert!(is_alive(process::id()));
        assert!(!is_alive(0), "pid 0 is not a process");
        // Picking a pid outside the pid space is best effort; the test only
        // asserts what can be asserted on every machine.
    }
}
