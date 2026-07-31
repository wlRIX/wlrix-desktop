// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading freedesktop `.desktop` files.
//!
//! Enough of the [Desktop Entry Specification][spec] to launch one: the `[Desktop Entry]`
//! group, the handful of keys that decide *what* to run, and the spec's own rules for turning
//! an `Exec=` line into an argument vector.
//!
//! [spec]: https://specifications.freedesktop.org/desktop-entry-spec/latest/
//!
//! Deliberately not a general parser. Categories, MIME associations and startup notification
//! are all skipped -- they matter to an application menu, and the desktop is not one. What is
//! here is what it takes to double-click a launcher and have the right process start, to draw
//! it with the right words underneath, and to offer its `[Desktop Action …]` groups on the
//! right-click menu.
//!
//! Keys the spec types as `localestring` -- `Name` and `Icon`, of the ones read here -- are
//! resolved through [`crate::locale`] as they are parsed, so what comes out is already in the
//! user's language. Everything else, `Exec` and `Type` among them, is the same in every locale
//! by definition; a translated command line would be a different program.
//!
//! **`Exec=` is a shell-*like* string but is not shell.** The spec defines its own quoting, and
//! handing the line to `sh -c` instead would let a `.desktop` file run anything a shell can --
//! substitutions, redirection, chained commands -- none of which the format is supposed to
//! allow. So it is split here, and the pieces are passed to `execvp` as-is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::locale::Locale;

/// One group's key/value pairs.
type Fields = HashMap<String, String>;

/// Look a `localestring` key up in a group, already resolved for the locale.
type Localized<'a> = dyn Fn(&Fields, &str) -> Option<String> + 'a;

/// wlRIX's own key for the icon to show while an application is running.
///
/// The Desktop Entry spec reserves the `X-` prefix for exactly this, so a file carrying it is
/// still a valid desktop entry to everything that does not know what it means.
pub const RUNNING_ICON_KEY: &str = "X-WLRIX-Running-Icon";

/// What kind of thing an entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    /// Something to run: `Exec` says what.
    Application,
    /// Something to open: `URL` says what.
    Link,
    /// A directory. Carries no action of its own.
    Directory,
}

/// A parsed `[Desktop Entry]` group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    pub entry_type: EntryType,
    /// `Name`, in the user's language: what the label under the icon reads, and what the `%c`
    /// field code expands to. The spec says `%c` is the *translated* name, so these are the
    /// same string and not two.
    pub name: Option<String>,
    pub exec: Option<String>,
    /// A binary that must exist for the entry to be usable.
    pub try_exec: Option<String>,
    /// The working directory to run in.
    pub path: Option<PathBuf>,
    /// Whether the program wants a terminal around it.
    pub terminal: bool,
    /// `Type=Link`'s target.
    pub url: Option<String>,
    /// The entry is "deleted"; the spec says to act as though it were not there.
    pub hidden: bool,
    /// The icon to draw: a theme name or an absolute path.
    pub icon: Option<String>,
    /// **A wlRIX extension**, not in the spec: the icon to draw *while the application is
    /// running*, in place of [`icon`](Self::icon).
    ///
    /// IRIX let an application show a different symbol once it was up -- the magic carpet says
    /// running or not, and this says what stands on it. The `X-` prefix is the spec's own
    /// reservation for extensions, so a file carrying this stays valid everywhere else.
    pub running_icon: Option<String>,
    /// The window class the application will use, when it declares one. This is how a window
    /// on screen is matched back to the launcher that would have started it; see
    /// [`crate::running`].
    pub startup_wm_class: Option<String>,
    /// The entry's `[Desktop Action …]` groups, in the order they should be offered.
    pub actions: Vec<DesktopAction>,
}

/// One `[Desktop Action …]` group: a second way into the same application.
///
/// Steam's launcher offers Store, Library, Friends and the rest this way; a browser offers a
/// private window. They are the launcher's own menu, and the desktop shows them on the one it
/// posts for a selected `.desktop` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAction {
    /// The identifier from the group header, `Store` in `[Desktop Action Store]`.
    ///
    /// Kept because it, not a position in a list, is how an action is named later: the file is
    /// re-read when the action is chosen, and by then it may have been edited.
    pub id: String,
    /// `Name`, in the user's language. Required by the spec, and there would be nothing to
    /// label the row with without it.
    pub name: String,
    /// `Exec`. Required by the spec; an action with nothing to run is not one.
    pub exec: String,
    /// `Icon`, if the action gives its own. Not drawn yet -- the menu has no icon column.
    pub icon: Option<String>,
}

impl DesktopEntry {
    /// Parse the `[Desktop Entry]` group out of a `.desktop` file's text.
    ///
    /// `None` if there is no such group, or it has no usable `Type`. Anything else malformed is
    /// skipped rather than fatal: a stray line in a file someone hand-edited should not stop
    /// the entry working.
    ///
    /// Localized keys are resolved for the process's own locale. [`parse_in`](Self::parse_in)
    /// takes one instead, for tests and for anything that needs to ask about a locale it is not
    /// running under.
    pub fn parse(text: &str) -> Option<Self> {
        Self::parse_in(text, Locale::current())
    }

    /// Parse against a given locale. See [`parse`](Self::parse).
    pub fn parse_in(text: &str, locale: &Locale) -> Option<Self> {
        let all = groups(text);
        let fields = all
            .iter()
            .find(|(name, _)| name == "Desktop Entry")
            .map(|(_, fields)| fields)?;

        // The spec's fallback walk, for the keys it types as `localestring`. A file with no
        // translations at all lands on the plain key, which is the last candidate.
        //
        // An empty value does not count as a match: `Name[ja]=` is a broken translation, not a
        // request for a nameless icon, so the walk carries on to the next candidate.
        let localized = |fields: &Fields, key: &str| {
            locale
                .candidates(key)
                .find_map(|candidate| fields.get(&candidate).filter(|value| !value.is_empty()))
                .cloned()
        };

        let entry_type = match fields.get("Type").map(String::as_str) {
            Some("Application") => EntryType::Application,
            Some("Link") => EntryType::Link,
            Some("Directory") => EntryType::Directory,
            // The spec makes Type required, and without it there is nothing to do.
            _ => return None,
        };

        Some(Self {
            entry_type,
            name: localized(fields, "Name"),
            exec: fields.get("Exec").cloned(),
            try_exec: fields.get("TryExec").cloned(),
            path: fields
                .get("Path")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
            terminal: fields.get("Terminal").map(String::as_str) == Some("true"),
            url: fields.get("URL").cloned(),
            hidden: fields.get("Hidden").map(String::as_str) == Some("true"),
            icon: localized(fields, "Icon"),
            running_icon: fields
                .get(RUNNING_ICON_KEY)
                .filter(|icon| !icon.is_empty())
                .cloned(),
            startup_wm_class: fields
                .get("StartupWMClass")
                .filter(|class| !class.is_empty())
                .cloned(),
            actions: actions(&all, fields.get("Actions").map(String::as_str), &localized),
        })
    }

    /// The action with this id, if the entry has one.
    ///
    /// By id rather than by position, because the two ends of choosing an action are separated
    /// by a re-read of the file: the menu is built from one parse and the action runs from
    /// another, and the file may have been edited in between.
    pub fn action(&self, id: &str) -> Option<&DesktopAction> {
        self.actions.iter().find(|action| action.id == id)
    }

    /// Read and parse a file.
    pub fn from_path(path: &Path) -> Option<Self> {
        Self::parse(&std::fs::read_to_string(path).ok()?)
    }

    /// The argument vector to run, or an explanation of why there is none.
    ///
    /// `path` is the `.desktop` file itself, for the `%k` field code.
    pub fn argv(&self, path: &Path) -> Result<Vec<String>, String> {
        if self.hidden {
            return Err("the entry is marked Hidden".to_string());
        }
        if self.entry_type != EntryType::Application {
            return Err(format!("{:?} entries have nothing to run", self.entry_type));
        }
        let exec = self
            .exec
            .as_deref()
            .filter(|exec| !exec.trim().is_empty())
            .ok_or_else(|| "the entry has no Exec line".to_string())?;

        // `TryExec` is the entry's own statement that it needs this binary. Checking it turns
        // "nothing happened" into a line saying what is missing.
        if let Some(try_exec) = self.try_exec.as_deref().filter(|t| !t.is_empty())
            && !on_path(try_exec)
        {
            return Err(format!("TryExec {try_exec:?} is not installed"));
        }

        let argv = expand(&split(exec)?, self.name.as_deref(), path);
        if argv.is_empty() {
            return Err("the Exec line expands to nothing".to_string());
        }
        Ok(argv)
    }
}

impl DesktopAction {
    /// The argument vector this action runs.
    ///
    /// `name` is the *application's* translated name, for `%c`: the spec defines that code as
    /// the name of the application, and an action is still that application under another
    /// door. `path` is the `.desktop` file, for `%k`.
    pub fn argv(&self, name: Option<&str>, path: &Path) -> Result<Vec<String>, String> {
        let argv = expand(&split(&self.exec)?, name, path);
        if argv.is_empty() {
            return Err(format!("action {:?} expands to nothing", self.id));
        }
        Ok(argv)
    }
}

/// Read the `[Desktop Action …]` groups an entry offers, in the order to offer them.
///
/// `listed` is the `Actions=` key: a semicolon-separated list of identifiers, which the spec
/// makes the authority on both *which* actions exist and what order they come in. When it is
/// missing -- which a hand-written file easily is -- the groups are taken in file order
/// instead. The spec would have them ignored entirely; showing them is more use than showing
/// nothing, and the file is malformed either way.
///
/// An action missing `Name` or `Exec` is dropped: the spec requires both, and without them
/// there is either no label to draw or nothing to run.
fn actions(
    all: &[(String, Fields)],
    listed: Option<&str>,
    localized: &Localized,
) -> Vec<DesktopAction> {
    let group_of = |id: &str| {
        let wanted = format!("Desktop Action {id}");
        all.iter()
            .find(|(name, _)| *name == wanted)
            .map(|(_, fields)| fields)
    };

    let ids: Vec<String> = match listed {
        Some(listed) => listed
            .split(';')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect(),
        None => all
            .iter()
            .filter_map(|(name, _)| name.strip_prefix("Desktop Action "))
            .map(str::to_owned)
            .collect(),
    };

    ids.into_iter()
        .filter_map(|id| {
            let fields = group_of(&id)?;
            Some(DesktopAction {
                name: localized(fields, "Name")?,
                exec: fields.get("Exec").filter(|exec| !exec.is_empty())?.clone(),
                icon: localized(fields, "Icon"),
                id,
            })
        })
        .collect()
}

/// Whether a `TryExec` value names something runnable.
///
/// An absolute path is checked directly; a bare name is looked for on `PATH`, as the spec says.
fn on_path(program: &str) -> bool {
    let candidate = Path::new(program);
    if candidate.is_absolute() {
        return candidate.is_file();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
}

/// Every group in a desktop file, in the order the file lists them.
///
/// In order because the action groups are read from it, and a file that gives no `Actions=`
/// key has nothing else to say what order its actions should be offered in.
///
/// Later duplicate keys lose, as `desktop-file-validate` would have rejected them anyway and
/// the first is the more likely intent. A duplicate *group* is the same: the first wins, and
/// the second is parsed into its own entry that nothing will look up.
fn groups(text: &str) -> Vec<(String, Fields)> {
    let mut groups: Vec<(String, Fields)> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        // Blank lines and comments carry nothing.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            groups.push((header.to_owned(), HashMap::new()));
            continue;
        }
        // Anything before the first group header belongs to no group at all.
        let Some((_, fields)) = groups.last_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Localized keys (`Name[ja]`) are kept under their whole name, brackets and all, so
        // they cannot collide with the plain key. Which of them wins is a separate question,
        // answered per lookup by `Locale::candidates`.
        let key = key.trim();
        fields
            .entry(key.to_owned())
            .or_insert_with(|| unescape(value.trim()));
    }

    groups
}

/// Undo the escape sequences the spec defines for string values.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            // Not a sequence the spec defines: keep both characters rather than eating one.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Split an `Exec=` value into arguments, by the spec's quoting rules.
///
/// Double quotes group; inside them a backslash escapes `"`, `` ` ``, `$` and `\`, and nothing
/// else. Single quotes are *not* quoting characters here -- the spec reserves them but gives
/// them no meaning -- so they are ordinary text, which is a real difference from a shell.
fn split(exec: &str) -> Result<Vec<String>, String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut chars = exec.chars().peekable();

    while let Some(character) = chars.next() {
        match character {
            '"' => {
                quoted = !quoted;
                // An empty pair of quotes is still an argument.
                started = true;
            }
            '\\' if quoted => match chars.next() {
                Some(escaped @ ('"' | '`' | '$' | '\\')) => {
                    current.push(escaped);
                    started = true;
                }
                // Any other backslash inside quotes is literal.
                Some(other) => {
                    current.push('\\');
                    current.push(other);
                    started = true;
                }
                None => return Err("Exec ends in a backslash".to_string()),
            },
            ' ' | '\t' if !quoted => {
                if started {
                    argv.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            other => {
                current.push(other);
                started = true;
            }
        }
    }

    if quoted {
        return Err("Exec has an unclosed quote".to_string());
    }
    if started {
        argv.push(current);
    }
    Ok(argv)
}

/// Replace the spec's field codes.
///
/// The desktop launches an entry with no files, so `%f`/`%F`/`%u`/`%U` expand to nothing and
/// the argument holding them disappears. `%i` is dropped as well -- it expands to
/// `--icon <Icon>`, and nothing here has an icon to pass. The deprecated codes
/// (`%d %D %n %N %v %m`) are removed, as the spec requires.
fn expand(argv: &[String], name: Option<&str>, path: &Path) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());

    for argument in argv {
        // A field code is only a field code on its own; `%%` is a literal percent anywhere.
        let mut expanded = String::with_capacity(argument.len());
        let mut dropped = false;
        let mut chars = argument.chars().peekable();

        while let Some(character) = chars.next() {
            if character != '%' {
                expanded.push(character);
                continue;
            }
            match chars.next() {
                Some('%') => expanded.push('%'),
                // No files or URLs are ever passed, so these leave nothing behind.
                Some('f' | 'F' | 'u' | 'U' | 'i' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm') => {
                    dropped = true;
                }
                Some('c') => expanded.push_str(name.unwrap_or_default()),
                Some('k') => expanded.push_str(&path.to_string_lossy()),
                // An unknown code is not ours to interpret; leave it be.
                Some(other) => {
                    expanded.push('%');
                    expanded.push(other);
                }
                None => expanded.push('%'),
            }
        }

        // An argument that was *only* a field code vanishes; one that merely contained one
        // keeps whatever was around it.
        if dropped && expanded.is_empty() {
            continue;
        }
        out.push(expanded);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("/desktop/thing.desktop")
    }

    #[test]
    fn a_plain_launcher_parses() {
        let entry = DesktopEntry::parse(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Alacritty\n\
             TryExec=alacritty\n\
             Exec=alacritty\n\
             Terminal=false\n",
        )
        .expect("should parse");
        assert_eq!(entry.entry_type, EntryType::Application);
        assert_eq!(entry.name.as_deref(), Some("Alacritty"));
        assert_eq!(entry.exec.as_deref(), Some("alacritty"));
        assert!(!entry.terminal);
    }

    #[test]
    fn only_the_desktop_entry_group_is_read() {
        // A real launcher carries `[Desktop Action …]` groups with their own Exec lines;
        // taking one of those would run the wrong thing.
        let entry = DesktopEntry::parse(
            "[Desktop Entry]\n\
             Type=Application\n\
             Exec=alacritty\n\
             \n\
             [Desktop Action New]\n\
             Name=New Terminal\n\
             Exec=should-not-win\n",
        )
        .expect("should parse");
        assert_eq!(entry.exec.as_deref(), Some("alacritty"));
    }

    #[test]
    fn a_file_with_no_desktop_entry_group_is_refused() {
        assert!(DesktopEntry::parse("[Desktop Action New]\nExec=nope\n").is_none());
    }

    #[test]
    fn an_entry_without_a_type_is_refused() {
        assert!(DesktopEntry::parse("[Desktop Entry]\nExec=nope\n").is_none());
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let entry = DesktopEntry::parse(
            "# a comment\n\
             \n\
             [Desktop Entry]\n\
             # another\n\
             Type=Application\n\
             Exec=thing\n",
        )
        .expect("should parse");
        assert_eq!(entry.exec.as_deref(), Some("thing"));
    }

    /// An abridged `mpv.desktop`: the shape every translated launcher has.
    const MPV: &str = "[Desktop Entry]\n\
                       Type=Application\n\
                       Name=mpv Media Player\n\
                       Name[fr]=Lecteur multimédia mpv\n\
                       Name[ja]=mpv メディアプレイヤー\n\
                       Name[zh_CN]=mpv 媒体播放器\n\
                       Name[zh_TW]=mpv 媒體播放器\n\
                       Icon=mpv\n\
                       Exec=mpv --player-operation-mode=pseudo-gui -- %U\n";

    /// `MPV`'s `Name`, as read under `locale`.
    fn mpv_name(locale: &str) -> String {
        DesktopEntry::parse_in(MPV, &Locale::parse(locale))
            .expect("should parse")
            .name
            .expect("mpv has a Name")
    }

    #[test]
    fn a_translated_name_is_taken_for_the_matching_locale() {
        assert_eq!(mpv_name("ja_JP.UTF-8"), "mpv メディアプレイヤー");
        assert_eq!(mpv_name("fr_FR.UTF-8"), "Lecteur multimédia mpv");
    }

    #[test]
    fn a_country_picks_its_own_translation_over_the_other_ones() {
        // The case that makes this worth doing properly: `zh_CN` and `zh_TW` are different
        // text, and there is no plain `Name[zh]` to fall back to.
        assert_eq!(mpv_name("zh_CN.UTF-8"), "mpv 媒体播放器");
        assert_eq!(mpv_name("zh_TW.UTF-8"), "mpv 媒體播放器");
    }

    #[test]
    fn a_country_falls_back_to_the_language_it_belongs_to() {
        // Canadian French is not in the file, and French is.
        assert_eq!(mpv_name("fr_CA.UTF-8"), "Lecteur multimédia mpv");
    }

    #[test]
    fn an_untranslated_locale_gets_the_plain_key() {
        assert_eq!(mpv_name("C"), "mpv Media Player");
        assert_eq!(mpv_name(""), "mpv Media Player");
    }

    #[test]
    fn a_locale_the_file_has_no_translation_for_gets_the_plain_key() {
        assert_eq!(mpv_name("is_IS.UTF-8"), "mpv Media Player");
    }

    #[test]
    fn localised_keys_do_not_shadow_the_plain_one() {
        let entry = DesktopEntry::parse_in(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Files\n\
             Name[ja]=ファイル\n\
             Exec=files\n",
            &Locale::parse("de_DE.UTF-8"),
        )
        .expect("should parse");
        assert_eq!(entry.name.as_deref(), Some("Files"));
    }

    #[test]
    fn only_localestring_keys_are_translated() {
        // A translated `Exec` would be a different program. The spec types `Exec` as a plain
        // string for exactly that reason, so a file carrying `Exec[ja]` is simply wrong and
        // the key must be ignored rather than run.
        let entry = DesktopEntry::parse_in(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Thing\n\
             Exec=thing\n\
             Exec[ja]=rm -rf /\n\
             Icon=thing\n\
             Icon[ja]=thing-ja\n",
            &Locale::parse("ja_JP.UTF-8"),
        )
        .expect("should parse");
        assert_eq!(entry.exec.as_deref(), Some("thing"));
        // `Icon` *is* a localestring, so it does follow the locale.
        assert_eq!(entry.icon.as_deref(), Some("thing-ja"));
    }

    #[test]
    fn an_empty_translation_falls_through_to_the_plain_name() {
        // Rather than labeling the icon with nothing at all, which is what an unfiltered
        // lookup would do -- the empty value matched first and stopped the walk.
        let entry = DesktopEntry::parse_in(
            "[Desktop Entry]\nType=Application\nName=Files\nName[ja]=\nExec=files\n",
            &Locale::parse("ja_JP.UTF-8"),
        )
        .expect("should parse");
        assert_eq!(entry.name.as_deref(), Some("Files"));
    }

    #[test]
    fn the_translated_name_is_what_the_percent_c_code_expands_to() {
        // The spec says `%c` is the *translated* name, so this follows from the same lookup
        // rather than being a second decision.
        let entry = DesktopEntry::parse_in(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Files\n\
             Name[ja]=ファイル\n\
             Exec=files --title %c\n",
            &Locale::parse("ja_JP.UTF-8"),
        )
        .expect("should parse");
        assert_eq!(
            entry.argv(&path()).unwrap(),
            ["files", "--title", "ファイル"]
        );
    }

    /// An abridged `steam.desktop`: `Actions=` plus the groups it names.
    const STEAM: &str = "[Desktop Entry]\n\
                         Type=Application\n\
                         Name=Steam\n\
                         Exec=/usr/bin/steam %U\n\
                         Actions=Store;Library;Friends;\n\
                         \n\
                         [Desktop Action Store]\n\
                         Name=Store\n\
                         Name[ja]=ストア\n\
                         Name[uk]=Крамниця\n\
                         Exec=/usr/bin/steam steam://store\n\
                         \n\
                         [Desktop Action Library]\n\
                         Name=Library\n\
                         Name[ja]=ライブラリ\n\
                         Icon=steam-library\n\
                         Exec=/usr/bin/steam steam://open/games\n\
                         \n\
                         [Desktop Action Friends]\n\
                         Name=Friends\n\
                         Exec=/usr/bin/steam steam://open/friends\n";

    fn steam(locale: &str) -> DesktopEntry {
        DesktopEntry::parse_in(STEAM, &Locale::parse(locale)).expect("should parse")
    }

    fn action_names(entry: &DesktopEntry) -> Vec<&str> {
        entry
            .actions
            .iter()
            .map(|action| action.name.as_str())
            .collect()
    }

    #[test]
    fn actions_come_back_in_the_order_the_actions_key_gives() {
        // The spec makes `Actions=` the authority on order, not the order of the groups.
        let entry = steam("C");
        assert_eq!(
            entry
                .actions
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            ["Store", "Library", "Friends"]
        );
        assert_eq!(action_names(&entry), ["Store", "Library", "Friends"]);
    }

    #[test]
    fn an_action_name_is_translated_like_any_other() {
        assert_eq!(
            action_names(&steam("ja_JP.UTF-8")),
            ["ストア", "ライブラリ", "Friends"]
        );
        assert_eq!(action_names(&steam("uk_UA.UTF-8"))[0], "Крамниця");
    }

    #[test]
    fn an_action_keeps_its_own_exec_and_icon() {
        let entry = steam("C");
        let store = entry.action("Store").expect("Store");
        assert_eq!(store.exec, "/usr/bin/steam steam://store");
        assert_eq!(store.icon, None);
        assert_eq!(
            entry.action("Library").and_then(|a| a.icon.as_deref()),
            Some("steam-library")
        );
        assert_eq!(entry.action("Nope"), None);
    }

    #[test]
    fn an_action_runs_its_own_command_not_the_entrys() {
        // The whole point: choosing "Store" must not simply start Steam.
        let entry = steam("C");
        let store = entry.action("Store").expect("Store");
        assert_eq!(
            store.argv(entry.name.as_deref(), &path()).unwrap(),
            ["/usr/bin/steam", "steam://store"]
        );
        assert_eq!(entry.argv(&path()).unwrap(), ["/usr/bin/steam"]);
    }

    #[test]
    fn an_action_group_the_actions_key_does_not_name_is_ignored() {
        // The spec's rule, and a real protection: a group nobody listed is not an offer.
        let entry = DesktopEntry::parse_in(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Thing\n\
             Exec=thing\n\
             Actions=Wanted;\n\
             \n\
             [Desktop Action Wanted]\n\
             Name=Wanted\n\
             Exec=thing --wanted\n\
             \n\
             [Desktop Action Sneaky]\n\
             Name=Sneaky\n\
             Exec=rm -rf /\n",
            &Locale::untranslated(),
        )
        .expect("should parse");
        assert_eq!(action_names(&entry), ["Wanted"]);
    }

    #[test]
    fn a_file_with_no_actions_key_falls_back_to_the_groups_in_order() {
        // Not what the spec says -- it would ignore them -- but a hand-written file that
        // forgot the key is better served by showing them than by showing nothing.
        let entry = DesktopEntry::parse_in(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Thing\n\
             Exec=thing\n\
             \n\
             [Desktop Action Second]\n\
             Name=Second\n\
             Exec=thing --second\n\
             \n\
             [Desktop Action First]\n\
             Name=First\n\
             Exec=thing --first\n",
            &Locale::untranslated(),
        )
        .expect("should parse");
        assert_eq!(action_names(&entry), ["Second", "First"]);
    }

    #[test]
    fn an_action_missing_a_name_or_an_exec_is_dropped() {
        // The spec requires both. Without a `Name` there is no row to draw, and without an
        // `Exec` the row would do nothing when chosen.
        let entry = DesktopEntry::parse_in(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Thing\n\
             Exec=thing\n\
             Actions=Nameless;Inert;Fine;\n\
             \n\
             [Desktop Action Nameless]\n\
             Exec=thing --nameless\n\
             \n\
             [Desktop Action Inert]\n\
             Name=Inert\n\
             \n\
             [Desktop Action Fine]\n\
             Name=Fine\n\
             Exec=thing --fine\n",
            &Locale::untranslated(),
        )
        .expect("should parse");
        assert_eq!(action_names(&entry), ["Fine"]);
    }

    #[test]
    fn an_actions_key_naming_a_group_that_is_not_there_skips_it() {
        let entry = DesktopEntry::parse_in(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Thing\n\
             Exec=thing\n\
             Actions=Missing;Present;\n\
             \n\
             [Desktop Action Present]\n\
             Name=Present\n\
             Exec=thing --present\n",
            &Locale::untranslated(),
        )
        .expect("should parse");
        assert_eq!(action_names(&entry), ["Present"]);
    }

    #[test]
    fn an_entry_with_no_action_groups_has_none() {
        let entry = DesktopEntry::parse_in(MPV, &Locale::untranslated()).expect("should parse");
        assert!(entry.actions.is_empty());
    }

    #[test]
    fn the_desktop_entry_group_still_wins_over_a_later_action_group() {
        // The regression the group rewrite could have caused: `group` used to stop at the next
        // header, and now every group is parsed. The main group's keys must still be its own.
        let entry = steam("C");
        assert_eq!(entry.exec.as_deref(), Some("/usr/bin/steam %U"));
        assert_eq!(entry.name.as_deref(), Some("Steam"));
    }

    #[test]
    fn a_desktop_entry_group_that_is_not_first_is_still_found() {
        let entry = DesktopEntry::parse_in(
            "[Desktop Action Early]\n\
             Name=Early\n\
             Exec=thing --early\n\
             \n\
             [Desktop Entry]\n\
             Type=Application\n\
             Name=Thing\n\
             Exec=thing\n\
             Actions=Early;\n",
            &Locale::untranslated(),
        )
        .expect("should parse");
        assert_eq!(entry.exec.as_deref(), Some("thing"));
        assert_eq!(action_names(&entry), ["Early"]);
    }

    #[test]
    fn value_escapes_are_undone() {
        let entry =
            DesktopEntry::parse("[Desktop Entry]\nType=Application\nName=a\\sb\\nc\\\\d\nExec=x\n")
                .expect("should parse");
        assert_eq!(entry.name.as_deref(), Some("a b\nc\\d"));
    }

    #[test]
    fn spaces_around_the_equals_are_ignored() {
        let entry =
            DesktopEntry::parse("[Desktop Entry]\nType = Application\nExec = thing\n").expect("ok");
        assert_eq!(entry.exec.as_deref(), Some("thing"));
    }

    // --- Exec splitting -------------------------------------------------------------

    fn args(exec: &str) -> Vec<String> {
        split(exec).expect("should split")
    }

    #[test]
    fn arguments_split_on_whitespace() {
        assert_eq!(args("prog -a -b"), ["prog", "-a", "-b"]);
        assert_eq!(args("  prog   -a  "), ["prog", "-a"]);
    }

    #[test]
    fn double_quotes_group() {
        assert_eq!(
            args(r#"prog "one argument" two"#),
            ["prog", "one argument", "two"]
        );
    }

    #[test]
    fn an_empty_quoted_string_is_still_an_argument() {
        assert_eq!(args(r#"prog "" x"#), ["prog", "", "x"]);
    }

    #[test]
    fn the_spec_escapes_work_inside_quotes() {
        assert_eq!(args(r#""a\"b""#), [r#"a"b"#]);
        assert_eq!(args(r#""a\$b""#), ["a$b"]);
        assert_eq!(args(r#""a\\b""#), [r"a\b"]);
        assert_eq!(args("\"a\\`b\""), ["a`b"]);
    }

    #[test]
    fn single_quotes_are_ordinary_text() {
        // A real difference from a shell, and the spec's own rule.
        assert_eq!(args("prog it's"), ["prog", "it's"]);
    }

    #[test]
    fn an_unclosed_quote_is_an_error() {
        assert!(split(r#"prog "unterminated"#).is_err());
        assert!(split(r#""ends in a backslash \"#).is_err());
    }

    // --- field codes ----------------------------------------------------------------

    fn argv_of(exec: &str) -> Vec<String> {
        let entry = DesktopEntry::parse(&format!(
            "[Desktop Entry]\nType=Application\nName=Thing\nExec={exec}\n"
        ))
        .expect("should parse");
        entry.argv(&path()).expect("should build an argv")
    }

    #[test]
    fn file_and_url_codes_expand_to_nothing() {
        // The desktop launches with no arguments, so these must disappear entirely rather
        // than being passed through as a literal "%F".
        for code in ["%f", "%F", "%u", "%U"] {
            assert_eq!(argv_of(&format!("prog {code}")), ["prog"], "{code}");
        }
    }

    #[test]
    fn deprecated_codes_are_removed() {
        for code in ["%d", "%D", "%n", "%N", "%v", "%m", "%i"] {
            assert_eq!(argv_of(&format!("prog {code}")), ["prog"], "{code}");
        }
    }

    #[test]
    fn a_doubled_percent_is_a_literal_one() {
        assert_eq!(argv_of("prog 100%%"), ["prog", "100%"]);
    }

    #[test]
    fn name_and_path_codes_expand() {
        assert_eq!(argv_of("prog %c"), ["prog", "Thing"]);
        assert_eq!(argv_of("prog %k"), ["prog", "/desktop/thing.desktop"]);
    }

    #[test]
    fn a_code_inside_a_bigger_argument_keeps_its_surroundings() {
        assert_eq!(argv_of("prog --file=%f"), ["prog", "--file="]);
    }

    #[test]
    fn an_unknown_code_is_left_alone() {
        assert_eq!(argv_of("prog %z"), ["prog", "%z"]);
    }

    // --- what refuses to launch -----------------------------------------------------

    fn refusal(text: &str) -> String {
        DesktopEntry::parse(text)
            .expect("should parse")
            .argv(&path())
            .expect_err("should refuse")
    }

    #[test]
    fn a_hidden_entry_does_not_run() {
        let why = refusal("[Desktop Entry]\nType=Application\nExec=x\nHidden=true\n");
        assert!(why.contains("Hidden"), "{why}");
    }

    #[test]
    fn a_link_or_directory_has_nothing_to_run() {
        // Both are legitimate entries; they are just not something `argv` can answer for.
        let why = refusal("[Desktop Entry]\nType=Link\nURL=https://example.invalid\n");
        assert!(why.contains("Link"), "{why}");
        let why = refusal("[Desktop Entry]\nType=Directory\n");
        assert!(why.contains("Directory"), "{why}");
    }

    #[test]
    fn a_missing_or_empty_exec_is_refused() {
        let why = refusal("[Desktop Entry]\nType=Application\n");
        assert!(why.contains("Exec"), "{why}");
        let why = refusal("[Desktop Entry]\nType=Application\nExec=   \n");
        assert!(why.contains("Exec"), "{why}");
    }

    #[test]
    fn an_uninstalled_try_exec_is_refused_by_name() {
        let why = refusal(
            "[Desktop Entry]\nType=Application\nTryExec=/nonexistent/binary\nExec=something\n",
        );
        assert!(why.contains("/nonexistent/binary"), "{why}");
    }

    #[test]
    fn a_link_entry_still_carries_its_url() {
        let entry = DesktopEntry::parse(
            "[Desktop Entry]\nType=Link\nName=Site\nURL=https://example.invalid/x\n",
        )
        .expect("should parse");
        assert_eq!(entry.url.as_deref(), Some("https://example.invalid/x"));
    }

    #[test]
    fn exec_is_not_run_through_a_shell() {
        // The whole reason for splitting here: a `.desktop` file must not be able to reach
        // shell features. These stay inert text in the argv.
        let argv = argv_of("prog $HOME");
        assert_eq!(argv, ["prog", "$HOME"], "no variable expansion");
        let argv = argv_of("prog a;b");
        assert_eq!(argv, ["prog", "a;b"], "no command separation");
        let argv = argv_of("prog *");
        assert_eq!(argv, ["prog", "*"], "no globbing");
    }
}
