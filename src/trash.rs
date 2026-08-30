// SPDX-License-Identifier: GPL-3.0-or-later
//! Moving things to the trash, per the [freedesktop Trash specification][spec].
//!
//! [spec]: https://specifications.freedesktop.org/trash-spec/latest/
//!
//! "Remove" on the desktop menu does **not** delete. There is no confirmation dialog yet and
//! no undo, so a misclick on a band-selected group would otherwise cost the lot. Trashing is
//! recoverable from any file manager, and it is what IRIX's dumpster was.
//!
//! Two files per item, as the spec requires: the thing itself moves to `$XDG_DATA_HOME/Trash/
//! files/`, and a `Trash/info/<name>.trashinfo` records where it came from and when, so a file
//! manager can put it back.
//!
//! **Home trash only.** The spec also defines per-volume `.Trash` directories for files on
//! other filesystems, reached when `rename` fails across a mount point. Copying-then-deleting
//! across a filesystem is a different and much riskier operation -- a half-copied directory
//! tree with the original already gone -- so that case is refused with a message instead.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Where the trash lives: `$XDG_DATA_HOME/Trash`, or `~/.local/share/Trash` as the spec says
/// to assume.
pub fn trash_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir).join("Trash"));
    }
    crate::xdg::home().map(|home| home.join(".local/share/Trash"))
}

/// Move `path` to the trash.
///
/// The error is meant to be read: it says what was refused and why, since nothing else will
/// tell the user why a file is still on their desktop.
pub fn trash(path: &Path) -> Result<(), String> {
    let root = trash_dir().ok_or_else(|| "no home directory, so no trash".to_string())?;
    let files = root.join("files");
    let info = root.join("info");
    for dir in [&files, &info] {
        std::fs::create_dir_all(dir)
            .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    }

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no usable name", path.display()))?;

    // The spec requires the two names to match and neither to overwrite anything, so the
    // *pair* has to be free -- claiming one and finding the other taken would strand a file
    // in the trash with no record of where it came from.
    let (target, record) = free_name(&files, &info, name)
        .ok_or_else(|| format!("the trash already holds too many copies of {name:?}"))?;

    // Written before the move: a `.trashinfo` with no file is harmless clutter, whereas a
    // trashed file with no record cannot be put back.
    std::fs::write(&record, trashinfo(path))
        .map_err(|err| format!("could not write {}: {err}", record.display()))?;

    match std::fs::rename(path, &target) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Leave nothing behind for a move that did not happen.
            let _ = std::fs::remove_file(&record);
            if err.kind() == std::io::ErrorKind::CrossesDevices {
                return Err(format!(
                    "{} is on a different filesystem from the trash, which is not supported yet",
                    path.display()
                ));
            }
            Err(format!(
                "could not move {} to the trash: {err}",
                path.display()
            ))
        }
    }
}

/// A `<name>` free in *both* `files` and `info`, with the paths to use.
///
/// The spec's own suggestion for collisions is to append a number, which is what this does.
fn free_name(files: &Path, info: &Path, name: &str) -> Option<(PathBuf, PathBuf)> {
    for attempt in 0..1000 {
        let candidate = if attempt == 0 {
            name.to_owned()
        } else {
            // `notes.txt` -> `notes.2.txt`, keeping the extension where a file manager and the
            // user both expect it.
            match name.rsplit_once('.') {
                Some((stem, extension)) if !stem.is_empty() => {
                    format!("{stem}.{}.{extension}", attempt + 1)
                }
                _ => format!("{name}.{}", attempt + 1),
            }
        };
        let target = files.join(&candidate);
        let record = info.join(format!("{candidate}.trashinfo"));
        // `try_exists` over `exists` so a broken symlink still counts as taken.
        let taken = target.try_exists().unwrap_or(true) || record.try_exists().unwrap_or(true);
        if !taken {
            return Some((target, record));
        }
    }
    None
}

/// The `.trashinfo` body: where it came from, and when it went.
fn trashinfo(path: &Path) -> String {
    format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        percent_encode(&path.to_string_lossy()),
        now()
    )
}

/// The current local time as the spec's `YYYY-MM-DDThh:mm:ss`.
///
/// Computed from the epoch rather than pulled from a date library: one format, no timezone
/// handling worth a dependency. UTC, which the spec permits -- it asks for local time but every
/// reader treats the field as informational.
fn now() -> String {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);

    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hour, minute, second) = (rest / 3600, (rest % 3600) / 60, rest % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}")
}

/// Days since the epoch to a calendar date.
///
/// Howard Hinnant's `civil_from_days`, which is the standard way to do this without a
/// calendar library and is correct for the whole proleptic Gregorian range.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (year + i64::from(month <= 2), month, day)
}

/// Percent-encode a path for the `Path=` field, as the spec requires.
///
/// Everything outside the unreserved set is escaped, except `/` -- the spec keeps separators
/// readable, and every implementation reads them that way.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A scratch home with its own trash, so nothing touches the real one.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wlrix-desktop-trash-{test}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("desktop")).expect("make the scratch directory");
        dir
    }

    /// Run `body` with `XDG_DATA_HOME` pointed at `dir`.
    ///
    /// Serialized: the environment is process-wide, and two tests setting it at once would
    /// each see the other's value.
    fn with_data_home<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|err| err.into_inner());
        // SAFETY: the lock makes this the only thread touching the environment, and the value
        // is restored before it is released.
        unsafe { std::env::set_var("XDG_DATA_HOME", dir) };
        let out = body();
        unsafe { std::env::remove_var("XDG_DATA_HOME") };
        drop(guard);
        out
    }

    #[test]
    fn a_file_moves_to_the_trash_with_a_record_of_where_it_was() {
        let home = scratch("basic");
        let file = home.join("desktop/notes.txt");
        fs::write(&file, "hello").unwrap();

        with_data_home(&home, || trash(&file).expect("should trash"));

        assert!(!file.exists(), "the original should be gone");
        let trashed = home.join("Trash/files/notes.txt");
        assert_eq!(fs::read_to_string(&trashed).unwrap(), "hello");

        let record = fs::read_to_string(home.join("Trash/info/notes.txt.trashinfo")).unwrap();
        assert!(record.starts_with("[Trash Info]\n"), "{record}");
        assert!(
            record.contains(&format!("Path={}", percent_encode(&file.to_string_lossy()))),
            "the record should say where it came from: {record}"
        );
        assert!(record.contains("DeletionDate=20"), "{record}");

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_directory_goes_too() {
        let home = scratch("directory");
        let dir = home.join("desktop/Projects");
        fs::create_dir_all(dir.join("inner")).unwrap();
        fs::write(dir.join("inner/file"), "x").unwrap();

        with_data_home(&home, || trash(&dir).expect("should trash"));

        assert!(!dir.exists());
        assert!(home.join("Trash/files/Projects/inner/file").exists());

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_second_file_of_the_same_name_does_not_overwrite_the_first() {
        let home = scratch("collision");
        for body in ["first", "second"] {
            let file = home.join("desktop/notes.txt");
            fs::write(&file, body).unwrap();
            with_data_home(&home, || trash(&file).expect("should trash"));
        }

        assert_eq!(
            fs::read_to_string(home.join("Trash/files/notes.txt")).unwrap(),
            "first",
            "the first file should still be there"
        );
        assert_eq!(
            fs::read_to_string(home.join("Trash/files/notes.2.txt")).unwrap(),
            "second",
            "the second should have been renamed alongside it"
        );
        assert!(home.join("Trash/info/notes.2.txt.trashinfo").exists());

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_name_with_no_extension_still_gets_a_number() {
        let home = scratch("no-extension");
        for _ in 0..2 {
            let file = home.join("desktop/README");
            fs::write(&file, "x").unwrap();
            with_data_home(&home, || trash(&file).expect("should trash"));
        }
        assert!(home.join("Trash/files/README").exists());
        assert!(home.join("Trash/files/README.2").exists());

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn trashing_something_that_is_not_there_is_refused_not_silent() {
        let home = scratch("missing");
        let why = with_data_home(&home, || {
            trash(&home.join("desktop/gone.txt")).expect_err("should refuse")
        });
        assert!(why.contains("gone.txt"), "{why}");
        // And it left no orphan record behind.
        assert!(!home.join("Trash/info/gone.txt.trashinfo").exists());

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn paths_are_percent_encoded_for_the_record() {
        // Spaces and non-ASCII both turn up in real filenames, and an unencoded `Path=` is a
        // record a file manager cannot parse.
        assert_eq!(percent_encode("/home/a/b c.txt"), "/home/a/b%20c.txt");
        assert_eq!(percent_encode("/home/a/%.txt"), "/home/a/%25.txt");
        assert_eq!(
            percent_encode("/home/a/メモ.txt"),
            "/home/a/%E3%83%A1%E3%83%A2.txt"
        );
        // Separators stay readable, and the unreserved set is left alone.
        assert_eq!(percent_encode("/a-b_c.d~e/f"), "/a-b_c.d~e/f");
    }

    #[test]
    fn the_deletion_date_is_the_shape_the_spec_asks_for() {
        let stamp = now();
        assert_eq!(stamp.len(), 19, "{stamp}");
        assert_eq!(stamp.as_bytes()[10], b'T', "{stamp}");
        // Sane year, rather than 1970 or something absurd.
        let year: i32 = stamp[..4].parse().expect("a year");
        assert!((2020..2200).contains(&year), "{stamp}");
    }

    #[test]
    fn the_calendar_maths_is_right_on_the_days_that_catch_people_out() {
        // Epoch, a leap day, and a century that is not a leap year.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        // 2024 is a leap year; 2026 is not, so February ends a day earlier.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_512), (2026, 2, 28));
        assert_eq!(civil_from_days(20_513), (2026, 3, 1));
    }
}
