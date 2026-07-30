// SPDX-License-Identifier: GPL-3.0-or-later
//! Opening what a double-click landed on.
//!
//! Two paths. A `.desktop` launcher is run from its own `Exec=` line (see
//! [`crate::desktop_entry`]); everything else -- files, directories, and a `Type=Link`
//! entry's URL -- is handed to an opener, `xdg-open` by default, which consults the user's
//! MIME associations.
//!
//! ## Running a `.desktop` file requires the execute bit
//!
//! A `.desktop` file is a small program written in a text format, and one can arrive on the
//! desktop from anywhere -- a download, an archive, a shared folder -- looking like whatever
//! its `Name` and `Icon` say. Launching on a double-click alone would make "save this file to
//! your desktop" enough to run arbitrary commands under a convincing disguise.
//!
//! So the execute bit is the consent: a launcher the user (or their package manager) marked
//! executable runs, and one that merely appeared does not. This is the same rule GNOME and
//! KDE settle on, and it is why a refusal here says *why* rather than doing nothing quietly.
//! Falling back to the opener would defeat it, since `xdg-open` on a `.desktop` file launches
//! it too.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::config::Config;
use crate::desktop_entry::{DesktopEntry, EntryType};
use crate::entries::{Entry, Kind};

/// Terminal emulators to try when nothing says which to use, in order of preference.
const KNOWN_TERMINALS: &[&str] = &[
    "alacritty",
    "foot",
    "kitty",
    "wezterm",
    "xterm",
    "gnome-terminal",
    "konsole",
];

/// What opening something would actually run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The command and its arguments, ready for `execvp`.
    pub argv: Vec<String>,
    /// The working directory, from a launcher's `Path=`.
    pub directory: Option<PathBuf>,
}

/// Decide what opening `entry` should run, without running it.
///
/// Split out from [`open`] so the decision can be tested without spawning anything, and so
/// `examples/explain_open` shows the real answer rather than its own reconstruction of it --
/// a diagnostic that can disagree with the code it diagnoses is worse than none.
///
/// The error is meant to be read: it says what was refused and why, because a double-click
/// that does nothing is otherwise indistinguishable from a broken desktop.
pub fn plan(entry: &Entry, config: &Config) -> Result<Plan, String> {
    match entry.kind {
        Kind::Launcher => launcher_plan(&entry.path, config),
        // A directory or a plain file is the opener's business, not ours.
        _ => Ok(Plan {
            argv: opener_argv(config, &entry.path.to_string_lossy()),
            directory: None,
        }),
    }
}

/// Start whatever `entry` should open, and hand back the child to be reaped.
pub fn open(entry: &Entry, config: &Config) -> Result<Child, String> {
    let plan = plan(entry, config)?;
    spawn(&plan.argv, plan.directory.as_deref())
}

/// What running a `.desktop` file amounts to.
fn launcher_plan(path: &Path, config: &Config) -> Result<Plan, String> {
    let entry = DesktopEntry::from_path(path)
        .ok_or_else(|| format!("{} is not a valid desktop entry", path.display()))?;

    // A `Type=Link` entry is a bookmark, not a program: it has no Exec, and its URL goes to
    // the opener like any other target. No execute bit is needed, because nothing is run --
    // the opener decides what handles the scheme.
    if entry.entry_type == EntryType::Link {
        let url = entry
            .url
            .as_deref()
            .filter(|url| !url.is_empty())
            .ok_or_else(|| format!("{} is a Link with no URL", path.display()))?;
        return Ok(Plan {
            argv: opener_argv(config, url),
            directory: None,
        });
    }

    if !is_executable(path) {
        return Err(format!(
            "{} is not executable, so it will not be run; `chmod +x` it if you trust it",
            path.display()
        ));
    }

    let argv = entry
        .argv(path)
        .map_err(|why| format!("{}: {why}", path.display()))?;

    // `Terminal=true` says the program draws on a terminal rather than opening a window, so
    // one has to be put around it.
    let argv = if entry.terminal {
        let mut terminal = terminal_argv(config).ok_or_else(|| {
            format!(
                "{} needs a terminal and none was found; set `terminal` in desktop.toml",
                path.display()
            )
        })?;
        terminal.extend(argv);
        terminal
    } else {
        argv
    };

    Ok(Plan {
        argv,
        directory: entry.path,
    })
}

/// The command that opens a file or URL by its type.
fn opener_argv(config: &Config, target: &str) -> Vec<String> {
    let mut argv = config.open_command();
    argv.push(target.to_owned());
    argv
}

/// The command to wrap a `Terminal=true` program in.
///
/// The configured value wins. Failing that: `xdg-terminal-exec`, the freedesktop answer to
/// this question, which takes the command straight after it; then `$TERMINAL`; then whichever
/// of [`KNOWN_TERMINALS`] is installed. All but the first take `-e`, which is near-universal.
pub fn terminal_argv(config: &Config) -> Option<Vec<String>> {
    if let Some(configured) = config.terminal_command() {
        return Some(configured);
    }
    if on_path("xdg-terminal-exec") {
        return Some(vec!["xdg-terminal-exec".to_owned()]);
    }
    if let Some(from_env) = std::env::var("TERMINAL")
        .ok()
        .filter(|terminal| !terminal.is_empty())
    {
        return Some(vec![from_env, "-e".to_owned()]);
    }
    KNOWN_TERMINALS
        .iter()
        .find(|terminal| on_path(terminal))
        .map(|terminal| vec![(*terminal).to_owned(), "-e".to_owned()])
}

/// Whether `program` is runnable: an absolute path as-is, a bare name via `PATH`.
fn on_path(program: &str) -> bool {
    let candidate = Path::new(program);
    if candidate.is_absolute() {
        return candidate.is_file();
    }
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// Whether any of the three execute bits is set.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Start `argv`, optionally in `directory`.
///
/// stdin is closed and stdout/stderr go to ours, so a launched program's output lands in the
/// session log next to everything else rather than vanishing. Nothing waits on the child here
/// -- [`crate::ui`] reaps it -- so a program that outlives the double-click is fine.
fn spawn(argv: &[String], directory: Option<&Path>) -> Result<Child, String> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| "nothing to run".to_string())?;

    let mut command = Command::new(program);
    command.args(arguments).stdin(Stdio::null());
    if let Some(directory) = directory.filter(|directory| directory.is_dir()) {
        command.current_dir(directory);
    }
    command
        .spawn()
        .map_err(|err| format!("could not start {program}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wlrix-desktop-open-{test}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("make the scratch directory");
        dir
    }

    fn entry(path: &Path, kind: Kind) -> Entry {
        // Through the real classifier, so these carry whatever a scan would have carried --
        // including the parsed launcher, which is what the drawing side reads.
        Entry::at(path).unwrap_or(Entry {
            path: path.to_path_buf(),
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            kind,
            launcher: None,
        })
    }

    fn config(toml: &str) -> Config {
        toml::from_str(toml).expect("config should parse")
    }

    /// A `.desktop` file, executable or not.
    fn launcher_file(dir: &Path, name: &str, body: &str, executable: bool) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn a_desktop_file_without_the_execute_bit_is_refused_and_says_why() {
        // The security rule: a launcher that merely arrived on the desktop must not run.
        let dir = scratch("not-executable");
        let path = launcher_file(
            &dir,
            "evil.desktop",
            "[Desktop Entry]\nType=Application\nName=Innocent\nExec=/bin/true\n",
            false,
        );

        let why = open(&entry(&path, Kind::Launcher), &config("")).expect_err("should refuse");
        assert!(why.contains("not executable"), "{why}");
        assert!(why.contains("chmod"), "should say how to allow it: {why}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_executable_desktop_file_runs() {
        let dir = scratch("executable");
        let path = launcher_file(
            &dir,
            "ok.desktop",
            "[Desktop Entry]\nType=Application\nName=True\nExec=/bin/true\n",
            true,
        );

        let mut child = open(&entry(&path, Kind::Launcher), &config("")).expect("should run");
        let status = child.wait().expect("should finish");
        assert!(status.success());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_exec_line_is_not_run_through_a_shell() {
        // `/bin/true` with a shell-looking argument. If this went through `sh -c`, the
        // redirection would create the file; it must not.
        let dir = scratch("no-shell");
        let victim = dir.join("should-not-exist");
        let path = launcher_file(
            &dir,
            "shell.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=Shelly\nExec=/bin/true > {}\n",
                victim.display()
            ),
            true,
        );

        let mut child = open(&entry(&path, Kind::Launcher), &config("")).expect("should run");
        let _ = child.wait();
        assert!(!victim.exists(), "the Exec line reached a shell");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_desktop_file_is_refused_by_name() {
        let dir = scratch("broken");
        let path = launcher_file(&dir, "broken.desktop", "not a desktop file at all\n", true);

        let why = open(&entry(&path, Kind::Launcher), &config("")).expect_err("should refuse");
        assert!(why.contains("broken.desktop"), "{why}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_launcher_runs_in_its_configured_path() {
        let dir = scratch("workdir");
        let workdir = dir.join("work");
        fs::create_dir(&workdir).unwrap();
        let marker = workdir.join("here");
        let path = launcher_file(
            &dir,
            "cwd.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=Touch\nPath={}\nExec=/usr/bin/touch here\n",
                workdir.display()
            ),
            true,
        );

        let mut child = open(&entry(&path, Kind::Launcher), &config("")).expect("should run");
        let _ = child.wait();
        assert!(marker.exists(), "should have run inside Path=");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plain_file_goes_to_the_configured_opener() {
        let dir = scratch("opener");
        let file = dir.join("notes.txt");
        fs::write(&file, "hello").unwrap();
        let marker = dir.join("opened");

        // `touch` stands in for xdg-open: it records that it was called with the file.
        let opener = config(&format!(
            "open = [\"/usr/bin/touch\", \"{}\"]",
            marker.display()
        ));
        let mut child = open(&entry(&file, Kind::Plain), &opener).expect("should run");
        let _ = child.wait();
        assert!(marker.exists(), "the opener was not called");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_link_entry_opens_its_url_without_needing_the_execute_bit() {
        // Nothing is executed for a Link, so the execute bit has nothing to consent to.
        let dir = scratch("link");
        let marker = dir.join("opened");
        let path = launcher_file(
            &dir,
            "site.desktop",
            "[Desktop Entry]\nType=Link\nName=Site\nURL=https://example.invalid/\n",
            false,
        );

        let opener = config(&format!(
            "open = [\"/usr/bin/touch\", \"{}\"]",
            marker.display()
        ));
        let mut child = open(&entry(&path, Kind::Launcher), &opener).expect("should open");
        let _ = child.wait();
        assert!(marker.exists(), "the URL was not handed to the opener");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_configured_terminal_wins() {
        let config = config("terminal = [\"myterm\", \"--run\"]");
        assert_eq!(
            terminal_argv(&config),
            Some(vec!["myterm".to_owned(), "--run".to_owned()])
        );
    }

    #[test]
    fn a_terminal_entry_is_wrapped_in_the_terminal() {
        let dir = scratch("terminal");
        let marker = dir.join("wrapped");
        let path = launcher_file(
            &dir,
            "top.desktop",
            "[Desktop Entry]\nType=Application\nName=Top\nTerminal=true\nExec=/bin/true\n",
            true,
        );

        // `touch <marker>` as the "terminal": running it proves the wrapper was used, since
        // the entry's own Exec is `/bin/true` and would leave nothing behind.
        let with_terminal = config(&format!(
            "terminal = [\"/usr/bin/touch\", \"{}\"]",
            marker.display()
        ));
        let mut child = open(&entry(&path, Kind::Launcher), &with_terminal).expect("should run");
        let _ = child.wait();
        assert!(marker.exists(), "the terminal wrapper was not used");

        let _ = fs::remove_dir_all(&dir);
    }
}
