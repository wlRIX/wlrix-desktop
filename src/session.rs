// SPDX-License-Identifier: GPL-3.0-or-later
//! Ending the session.
//!
//! `wlrix-session` starts the compositor and exits when it does -- "the compositor is the
//! session", as its own docs put it. So logging out means asking the compositor to stop, and
//! everything above it unwinds on its own: the session tears down, and `wlrix-greeter` comes
//! back up.
//!
//! The compositor drops its pid in a well-known file for exactly this sort of thing (its
//! `pidfile.rs`); the settings apps read the same file to send it a `SIGHUP` when a setting
//! changes. `SIGTERM` is the compositor's own "stop cleanly" signal -- it installs a handler
//! that ends its event loop, the same one greetd uses to take the greeter's compositor down.
//!
//! A stale pidfile is the awkward case: a crashed compositor leaves one behind, and signaling
//! whatever pid has since been recycled would be worse than doing nothing. So "no such
//! process" is reported as "not running" rather than trusted blindly.

use std::path::PathBuf;

/// The compositor's pidfile, beside its log under the per-user runtime directory.
///
/// Kept in step with `wlrix-compositor/src/pidfile.rs` by hand -- the two repos build
/// standalone, so there is no shared constant to point at.
const PID_NAME: &str = "wlrix-compositor.pid";

/// Where the pidfile lives: `$XDG_RUNTIME_DIR`, else the temp directory.
fn pidfile() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
        .join(PID_NAME)
}

/// Ask the compositor to stop, ending the session. Returns the pid that was signaled.
pub fn log_out() -> Result<i32, String> {
    let path = pidfile();
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    let pid =
        parse_pid(&text).ok_or_else(|| format!("{} does not contain a pid", path.display()))?;

    signal(pid, libc::SIGTERM)?;
    Ok(pid)
}

/// The pid a pidfile's contents name, if it is one.
fn parse_pid(text: &str) -> Option<i32> {
    text.trim().parse::<i32>().ok().filter(|pid| *pid > 1)
}

/// Send `signal` to `pid`, telling a stale pidfile apart from a real failure.
fn signal(pid: i32, signal: i32) -> Result<(), String> {
    // SAFETY: `kill` with a positive pid and a valid signal number. It either signals that
    // process or reports why it could not; nothing here depends on the process existing.
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        // The pidfile outlived the compositor that wrote it.
        Some(libc::ESRCH) => Err(format!(
            "no process {pid}; the compositor's pidfile is stale"
        )),
        Some(libc::EPERM) => Err(format!("not allowed to signal process {pid}")),
        _ => Err(format!("could not signal process {pid}: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pidfile_is_a_bare_number() {
        assert_eq!(parse_pid("1234"), Some(1234));
        // The compositor writes a trailing newline.
        assert_eq!(parse_pid("1234\n"), Some(1234));
        assert_eq!(parse_pid("  1234  \n"), Some(1234));
    }

    #[test]
    fn nonsense_in_the_pidfile_is_not_a_pid() {
        for text in ["", "\n", "not a pid", "12x", "-1", "3.5"] {
            assert_eq!(parse_pid(text), None, "{text:?}");
        }
    }

    #[test]
    fn pid_one_and_zero_are_refused() {
        // 0 signals the whole process group and 1 is init; a pidfile naming either is
        // corrupt, and acting on it would be far worse than doing nothing.
        assert_eq!(parse_pid("0"), None);
        assert_eq!(parse_pid("1"), None);
    }

    #[test]
    fn signaling_a_process_that_is_gone_says_the_file_is_stale() {
        // A pid that cannot be running: the maximum is well below this on Linux.
        let why = signal(0x7fff_fffe, 0).expect_err("should fail");
        assert!(why.contains("stale"), "{why}");
    }

    #[test]
    fn signaling_ourselves_with_signal_zero_succeeds() {
        // Signal 0 checks for existence without delivering anything, so this proves the
        // success path without stopping the test run.
        let me = std::process::id() as i32;
        assert!(signal(me, 0).is_ok());
    }

    #[test]
    fn the_pidfile_sits_beside_the_compositors_log() {
        // Kept in step with the compositor by hand, so a change there that is not mirrored
        // here would make Log Out silently stop working.
        assert!(pidfile().ends_with("wlrix-compositor.pid"));
    }
}
