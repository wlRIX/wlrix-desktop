// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding the user's desktop directory, and the two wlRIX files that describe it.
//!
//! `XDG_DESKTOP_DIR` is not an environment variable in practice -- `xdg-user-dirs` writes it
//! into `~/.config/user-dirs.dirs`, a file of shell assignments that a login script sources.
//! A Wayland client started by the session never runs that script, so the file has to be read
//! directly. The environment is still checked first, since someone setting it by hand means it.
//!
//! The config and state paths follow the compositor's convention exactly (see its
//! `config.rs` and `outputs.rs`): a hand-edited file under `$XDG_CONFIG_HOME/wlrix/`, and a
//! machine-written one under `$XDG_STATE_HOME/wlrix/`.

use std::path::{Path, PathBuf};

/// Where `xdg-user-dirs` records the user's directory choices.
const USER_DIRS: &str = "user-dirs.dirs";
/// The hand-edited settings file, relative to a config directory.
pub const CONFIG_NAME: &str = "wlrix/desktop.toml";
/// The machine-written icon positions, relative to the state directory.
pub const STATE_NAME: &str = "wlrix/desktop-icons.toml";
/// Consulted when the user has no config of their own, as the compositor does.
const SYSTEM_CONFIG_DIR: &str = "/etc";

/// `$HOME`, or `None` when even that is unset.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// `$XDG_CONFIG_HOME`, or `~/.config` as the spec says to assume.
pub fn user_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    home().map(|home| home.join(".config"))
}

/// `$XDG_STATE_HOME`, or `~/.local/state` as the spec says to assume.
pub fn user_state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    home().map(|home| home.join(".local/state"))
}

/// Where to look for the settings file, most specific first.
pub fn config_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = user_config_dir() {
        dirs.push(dir.join(CONFIG_NAME));
    }
    dirs.push(Path::new(SYSTEM_CONFIG_DIR).join(CONFIG_NAME));
    dirs
}

/// Where the machine-written icon positions live.
pub fn state_path() -> Option<PathBuf> {
    user_state_dir().map(|dir| dir.join(STATE_NAME))
}

/// The directory whose files get icons.
///
/// `$XDG_DESKTOP_DIR` wins if it is set, then `user-dirs.dirs`, then `~/Desktop`. The result
/// is not required to exist: an empty desktop is a perfectly good desktop, and the directory
/// may well be created later -- the watch picks that up.
pub fn desktop_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DESKTOP_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }

    let home = home()?;
    let from_file = user_config_dir()
        .map(|dir| dir.join(USER_DIRS))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| parse_user_dirs(&text, "XDG_DESKTOP_DIR", &home));

    Some(from_file.unwrap_or_else(|| home.join("Desktop")))
}

/// Pull one directory out of a `user-dirs.dirs` file.
///
/// The format is shell assignments, e.g. `XDG_DESKTOP_DIR="$HOME/デスクトップ"`. Only the
/// two forms `xdg-user-dirs` actually writes are handled -- a `$HOME`-relative path and an
/// absolute one -- rather than pretending to be a shell. A line that is neither is skipped,
/// which falls back to `~/Desktop` rather than pointing at somewhere wrong.
fn parse_user_dirs(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    // Last assignment wins, as it would if a shell sourced the file.
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let value = line
                .strip_prefix(key)?
                .trim_start()
                .strip_prefix('=')?
                .trim();
            // Unquote. `xdg-user-dirs` always quotes, but an unquoted value is still a
            // value and is cheap to accept.
            let value = value
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .unwrap_or(value);
            if value.is_empty() {
                return None;
            }
            if let Some(rest) = value.strip_prefix("$HOME/") {
                return Some(home.join(rest));
            }
            if value == "$HOME" {
                return Some(home.to_path_buf());
            }
            value.starts_with('/').then(|| PathBuf::from(value))
        })
        .next_back()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/home/tester")
    }

    #[test]
    fn home_relative_paths_are_expanded() {
        let text = "XDG_DESKTOP_DIR=\"$HOME/Desktop\"\n";
        assert_eq!(
            parse_user_dirs(text, "XDG_DESKTOP_DIR", &home()),
            Some(PathBuf::from("/home/tester/Desktop"))
        );
    }

    #[test]
    fn absolute_paths_are_taken_as_they_are() {
        let text = "XDG_DESKTOP_DIR=\"/srv/shared/desk\"\n";
        assert_eq!(
            parse_user_dirs(text, "XDG_DESKTOP_DIR", &home()),
            Some(PathBuf::from("/srv/shared/desk"))
        );
    }

    #[test]
    fn a_localized_directory_name_survives() {
        // The dev machine runs a Japanese locale, where xdg-user-dirs writes exactly this.
        let text = "XDG_DESKTOP_DIR=\"$HOME/デスクトップ\"\n";
        assert_eq!(
            parse_user_dirs(text, "XDG_DESKTOP_DIR", &home()),
            Some(PathBuf::from("/home/tester/デスクトップ"))
        );
    }

    #[test]
    fn other_keys_and_comments_are_ignored() {
        let text = "# Created by xdg-user-dirs-update\n\
                    XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n\
                    XDG_DESKTOP_DIR=\"$HOME/Desktop\"\n\
                    XDG_MUSIC_DIR=\"$HOME/Music\"\n";
        assert_eq!(
            parse_user_dirs(text, "XDG_DESKTOP_DIR", &home()),
            Some(PathBuf::from("/home/tester/Desktop"))
        );
    }

    #[test]
    fn the_last_assignment_wins() {
        // What a shell sourcing the file would end up with.
        let text = "XDG_DESKTOP_DIR=\"$HOME/first\"\nXDG_DESKTOP_DIR=\"$HOME/second\"\n";
        assert_eq!(
            parse_user_dirs(text, "XDG_DESKTOP_DIR", &home()),
            Some(PathBuf::from("/home/tester/second"))
        );
    }

    #[test]
    fn a_value_we_cannot_read_is_no_answer_at_all() {
        // Anything needing real shell expansion is refused rather than guessed at, so the
        // caller falls back to ~/Desktop instead of watching the wrong directory.
        for text in [
            "XDG_DESKTOP_DIR=\"${HOME}/Desktop\"\n",
            "XDG_DESKTOP_DIR=\"$OTHER/Desktop\"\n",
            "XDG_DESKTOP_DIR=\"\"\n",
            "XDG_DESKTOP_DIR=\"relative/path\"\n",
        ] {
            assert_eq!(
                parse_user_dirs(text, "XDG_DESKTOP_DIR", &home()),
                None,
                "{text}"
            );
        }
    }
}
