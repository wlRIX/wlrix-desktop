// SPDX-License-Identifier: GPL-3.0-or-later
//! Noticing that the desktop directory changed.
//!
//! An inotify watch on the directory, whose fd goes into the same `calloop` loop as the
//! Wayland connection -- so a file appearing and a pointer moving are handled the same way,
//! and nothing polls.
//!
//! The events themselves are thrown away: what arrives is "something changed", and the answer
//! is always to re-read the directory. Tracking individual creates and renames would mean
//! reimplementing the directory listing incrementally, for a directory with a few dozen files
//! in it. Saving a copy from `cp`, or unpacking an archive, produces a burst of events; the
//! loop drains all of them before reporting once, so one rescan covers the burst.
//!
//! The parent directory is watched *while the desktop directory does not exist*, so one that
//! has not been created yet -- or that gets deleted and recreated -- is picked up rather than
//! leaving a permanently empty desktop. The parent watch is dropped as soon as it has served
//! that purpose: it is usually `$HOME`, and leaving it on means every unrelated write there
//! costs a full rescan.

use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::Path;

use rustix::fs::inotify;

/// What we ask inotify for.
///
/// `CREATE`/`DELETE`/`MOVED_*` are the obvious ones. `ATTRIB` is here because chmod +x
/// changes which icon a file gets, and `CLOSE_WRITE` because a file being written finishes
/// long after it was created -- without it a copied file would keep the icon it had when it
/// was an empty placeholder.
const WATCHED: inotify::WatchFlags = inotify::WatchFlags::CREATE
    .union(inotify::WatchFlags::DELETE)
    .union(inotify::WatchFlags::DELETE_SELF)
    .union(inotify::WatchFlags::MOVED_FROM)
    .union(inotify::WatchFlags::MOVED_TO)
    .union(inotify::WatchFlags::MOVE_SELF)
    .union(inotify::WatchFlags::ATTRIB)
    .union(inotify::WatchFlags::CLOSE_WRITE);

/// What the parent watch asks for: just enough to notice the desktop directory appearing.
const WATCHED_PARENT: inotify::WatchFlags = inotify::WatchFlags::CREATE
    .union(inotify::WatchFlags::MOVED_TO)
    .union(inotify::WatchFlags::DELETE);

/// A watch on the desktop directory.
pub struct Watch {
    fd: OwnedFd,
    /// The watch on the directory itself. `None` while it does not exist -- the parent watch
    /// is what tells us to try again.
    directory: Option<i32>,
    /// The watch on the parent, held **only** while `directory` is `None`.
    ///
    /// The parent is usually `$HOME`, which is busy: shell history, editor scratch files and
    /// anything else writing there would each fire an event and cost a full rescan of the
    /// desktop. It is worth watching only for the one thing it can tell us that the directory
    /// watch cannot -- that the desktop directory has appeared -- so it is dropped the moment
    /// that happens, and taken again if the directory is removed.
    parent: Option<i32>,
}

impl Watch {
    /// Start watching `dir`.
    ///
    /// Fails only if inotify itself is unavailable. A missing desktop directory is not a
    /// failure: the parent is watched instead, and the directory is picked up when created.
    pub fn new(dir: &Path) -> std::io::Result<Self> {
        // Non-blocking: the loop reads until the fd is drained and must not stall there.
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)?;
        let mut watch = Self {
            fd,
            directory: None,
            parent: None,
        };
        watch.rewatch(dir);
        Ok(watch)
    }

    /// (Re)attach the watch on the directory, and keep the parent watch in step.
    ///
    /// Called on every change, because a directory that was deleted and recreated is a new
    /// inode and the old watch is dead. Cheap when the watch is already there: inotify
    /// returns the same descriptor for the same path.
    fn rewatch(&mut self, dir: &Path) {
        self.directory = inotify::add_watch(&self.fd, dir, WATCHED).ok();

        match (self.directory, self.parent) {
            // Watching the directory: the parent has nothing left to tell us.
            (Some(_), Some(parent)) => {
                let _ = inotify::remove_watch(&self.fd, parent);
                self.parent = None;
            }
            // No directory to watch, and not yet watching for one to appear.
            (None, None) => {
                // Best-effort. A missing home directory is a stranger problem than this.
                self.parent = dir
                    .parent()
                    .and_then(|parent| inotify::add_watch(&self.fd, parent, WATCHED_PARENT).ok());
            }
            _ => {}
        }
    }

    /// The fd to register with the event loop.
    pub fn as_fd(&self) -> &OwnedFd {
        &self.fd
    }

    /// Drain every pending event and say whether anything happened.
    ///
    /// Always drains fully, whatever it finds: a level-triggered loop source would spin
    /// forever on an fd left readable. The answer is a single bool because every event leads
    /// to the same rescan.
    pub fn drain(&mut self, dir: &Path) -> bool {
        let mut buffer = [MaybeUninit::<u8>::uninit(); 4096];
        let mut changed = false;
        let mut reader = inotify::Reader::new(&self.fd, &mut buffer);
        loop {
            match reader.next() {
                Ok(_) => changed = true,
                // Drained, or nothing there yet. (`AGAIN` is the same value on Linux.)
                Err(rustix::io::Errno::WOULDBLOCK) => break,
                Err(rustix::io::Errno::INTR) => continue,
                // Anything else means the fd is unusable; stop rather than spin on it.
                Err(_) => break,
            }
        }

        // The directory may have just been created, or replaced. Re-attaching is cheap and
        // the alternative is a watch pointing at a dead inode.
        if changed {
            self.rewatch(dir);
        }
        changed
    }

    /// Whether the desktop directory itself is currently watched.
    ///
    /// Only interesting to tests and to the log line at startup: a `false` here means the
    /// desktop directory does not exist, which is a legitimate state.
    pub fn is_watching_directory(&self) -> bool {
        self.directory.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wlrix-desktop-watch-{test}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make the scratch directory");
        dir
    }

    /// inotify delivers asynchronously, so give it a moment before deciding nothing happened.
    ///
    /// Generous on purpose: this is a "did it arrive at all" check, not a latency measurement,
    /// and a short budget turns into a flaky test on a loaded machine.
    fn settled_drain(watch: &mut Watch, dir: &Path) -> bool {
        for _ in 0..400 {
            if watch.drain(dir) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_new_file_is_noticed() {
        let dir = scratch("new-file");
        let mut watch = Watch::new(&dir).expect("start the watch");
        assert!(watch.is_watching_directory());

        std::fs::write(dir.join("notes.txt"), "hello").unwrap();
        assert!(settled_drain(&mut watch, &dir));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_removed_file_is_noticed() {
        let dir = scratch("removed-file");
        let file = dir.join("notes.txt");
        std::fs::write(&file, "hello").unwrap();
        let mut watch = Watch::new(&dir).expect("start the watch");
        // Clear the events from setting up.
        let _ = watch.drain(&dir);

        std::fs::remove_file(&file).unwrap();
        assert!(settled_drain(&mut watch, &dir));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_quiet_directory_reports_nothing() {
        // The loop must not be woken into a rescan when nothing has happened.
        let dir = scratch("quiet");
        let mut watch = Watch::new(&dir).expect("start the watch");
        let _ = watch.drain(&dir);
        assert!(!watch.drain(&dir));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn activity_in_the_parent_is_not_our_business() {
        // The parent is `$HOME` in a real session -- busy with shell history, editor scratch
        // files and the rest. Watching it once the desktop directory exists would cost a full
        // rescan for each of those. (It also made this test suite flaky, since the parent here
        // is the shared temp directory.)
        let dir = scratch("parent-noise");
        let mut watch = Watch::new(&dir).expect("start the watch");
        assert!(watch.is_watching_directory());
        let _ = watch.drain(&dir);

        // A sibling of the desktop directory, not inside it.
        let sibling = dir.parent().expect("a parent").join("wlrix-desktop-noise");
        std::fs::write(&sibling, "not on the desktop").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !watch.drain(&dir),
            "a sibling file should not wake the desktop"
        );

        let _ = std::fs::remove_file(&sibling);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_parent_watch_is_taken_and_dropped_with_the_directory() {
        let parent = scratch("parent-lifecycle");
        let dir = parent.join("Desktop");

        // No directory yet, so the parent is watched to hear about it.
        let mut watch = Watch::new(&dir).expect("start the watch");
        assert!(!watch.is_watching_directory());
        assert!(watch.parent.is_some(), "should be waiting on the parent");

        std::fs::create_dir(&dir).unwrap();
        assert!(settled_drain(&mut watch, &dir));
        assert!(watch.is_watching_directory());
        assert!(
            watch.parent.is_none(),
            "the parent watch should be released"
        );

        // And taken again when the directory goes away.
        std::fs::remove_dir(&dir).unwrap();
        assert!(settled_drain(&mut watch, &dir));
        assert!(!watch.is_watching_directory());
        assert!(
            watch.parent.is_some(),
            "should be waiting on the parent again"
        );

        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn a_directory_that_does_not_exist_yet_is_still_watchable() {
        let parent = scratch("late-directory");
        let dir = parent.join("Desktop");
        let mut watch = Watch::new(&dir).expect("start the watch");
        assert!(
            !watch.is_watching_directory(),
            "nothing to watch until it exists"
        );

        std::fs::create_dir(&dir).unwrap();
        // The parent watch fires, and draining re-attaches to the new directory.
        assert!(settled_drain(&mut watch, &dir));
        assert!(watch.is_watching_directory());

        let _ = std::fs::remove_dir_all(&parent);
    }
}
