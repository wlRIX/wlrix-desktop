// SPDX-License-Identifier: GPL-3.0-or-later
//! What type a file is, by its name, and which icon names to try for that type.
//!
//! The freedesktop shared-mime-info database, read from the compiled files
//! `update-mime-database` writes -- `globs2`, `aliases`, `icons` and `generic-icons` -- under
//! `$XDG_DATA_HOME/mime` and each `$XDG_DATA_DIRS/mime`, earlier directories winning.
//!
//! A port of the name tier of `wlrix-apps`' `SharedMimeDatabase`, so the desktop and the file
//! manager put the same picture on the same file. The same deliberate limit, too: **the name
//! decides, and the contents are never read.** Sniffing would mean opening every file on the
//! desktop on every rescan, for the minority of files with no useful name. Those get the
//! generic icon, exactly as they do in a wlRIX Files listing.
//!
//! The precedence is the spec's, and it is not "whatever matches": a literal file name first
//! (`Makefile`, `core`), then the longest matching extension (so `.tar.gz` beats `.gz`), then
//! any other pattern by weight.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// What a directory is called. The stat decides it, never the name.
pub const DIRECTORY: &str = "inode/directory";
/// What an empty file is called -- `xdg-mime`'s answer, and the `inode/` family the stat-derived
/// types already use. Known from the metadata, so it costs no read.
pub const EMPTY: &str = "inode/x-empty";
/// What a file with the execute bit and no recognizable name is.
pub const EXECUTABLE: &str = "application/x-executable";
/// What anything else unrecognized is called.
pub const DEFAULT: &str = "application/octet-stream";

/// One line of `globs2`.
#[derive(Debug, Clone)]
struct Glob {
    weight: i32,
    mime: String,
    pattern: String,
    /// The `cs` flag. Rare and load-bearing where it appears: `*.C` is C++ source, and a
    /// case-insensitive match would also claim every `.c` file.
    case_sensitive: bool,
}

/// Which table a pattern goes in, which decides the order it is consulted in.
#[derive(Debug, PartialEq, Eq)]
enum GlobKind {
    Literal,
    Extension,
    Other,
}

impl Glob {
    fn kind(&self) -> GlobKind {
        let wild = |text: &str| text.contains(['*', '?', '[']);
        match self.pattern.strip_prefix("*.") {
            Some(rest) if !wild(rest) => GlobKind::Extension,
            _ if !wild(&self.pattern) => GlobKind::Literal,
            _ => GlobKind::Other,
        }
    }

    fn matches(&self, name: &str) -> bool {
        match self.kind() {
            GlobKind::Literal if self.case_sensitive => self.pattern == name,
            GlobKind::Literal => self.pattern.to_lowercase() == name.to_lowercase(),
            // `*.ext`: everything after the star, dot included.
            GlobKind::Extension if self.case_sensitive => name.ends_with(&self.pattern[1..]),
            GlobKind::Extension => name
                .to_lowercase()
                .ends_with(&self.pattern[1..].to_lowercase()),
            GlobKind::Other if self.case_sensitive => fnmatch(&self.pattern, name),
            GlobKind::Other => fnmatch(&self.pattern.to_lowercase(), &name.to_lowercase()),
        }
    }
}

/// The name tables of the shared-mime-info database.
#[derive(Debug, Default)]
pub struct Database {
    /// Keyed by the lowercased name, and checked against the glob's own case rule on a hit.
    literals: HashMap<String, Vec<Glob>>,
    /// Keyed by the lowercased suffix, dot included, likewise.
    extensions: HashMap<String, Vec<Glob>>,
    others: Vec<Glob>,
    aliases: HashMap<String, String>,
    icons: HashMap<String, String>,
    generic_icons: HashMap<String, String>,
}

impl Database {
    /// The database the session has, loaded once.
    ///
    /// Shared rather than reloaded per rescan: the desktop rescans on every change to the
    /// directory, and the database only changes when software is installed. A type a newly
    /// installed package registers is picked up the next time the desktop starts.
    pub fn shared() -> &'static Database {
        static SHARED: OnceLock<Database> = OnceLock::new();
        SHARED.get_or_init(|| Database::load(&search_paths()))
    }

    /// Load from these `mime` directories, earliest winning.
    ///
    /// The seam the tests use. They must never read `/usr/share/mime`: its contents vary with
    /// the shared-mime-info version, and an assertion about what `.md` maps to would fail for
    /// reasons having nothing to do with this code.
    pub fn load(directories: &[PathBuf]) -> Self {
        let mut db = Self::default();
        for dir in directories {
            db.load_globs(&dir.join("globs2"));
            for (alias, canonical) in pairs(&dir.join("aliases"), ' ') {
                db.aliases.entry(alias).or_insert(canonical);
            }
            for (mime, icon) in pairs(&dir.join("icons"), ':') {
                db.icons.entry(mime).or_insert(icon);
            }
            for (mime, icon) in pairs(&dir.join("generic-icons"), ':') {
                db.generic_icons.entry(mime).or_insert(icon);
            }
        }

        // Heaviest first. Stable, so equal weights keep file order -- and across directories,
        // the earlier directory's entry stays ahead.
        for list in db.literals.values_mut().chain(db.extensions.values_mut()) {
            list.sort_by_key(|glob| std::cmp::Reverse(glob.weight));
        }
        db.others.sort_by(|a, b| {
            b.weight
                .cmp(&a.weight)
                .then_with(|| b.pattern.len().cmp(&a.pattern.len()))
        });
        db
    }

    /// The type a file name implies, or `None` when nothing matches.
    pub fn resolve_by_name(&self, name: &str) -> Option<&str> {
        if name.is_empty() {
            return None;
        }
        // 1. An exact file name. `core` and `Makefile` are types in their own right and must
        //    not lose to some wildcard that also matches.
        let literal = self.literals.get(&name.to_lowercase());
        if let Some(found) = literal.and_then(|list| best(list, name)) {
            return Some(found);
        }

        // 2. The longest matching extension. Walking the dots left to right yields the longest
        //    suffix first, which is what makes `.tar.gz` beat `.gz`. A trailing dot is no
        //    extension at all.
        for (i, _) in name.match_indices('.') {
            if i + 1 >= name.len() {
                break;
            }
            // Lowercased per suffix rather than sliced out of a lowercased copy of the name:
            // lowercasing can change a character's length in bytes, and the indices are the
            // original's.
            let suffix = name[i..].to_lowercase();
            if let Some(found) = self
                .extensions
                .get(&suffix)
                .and_then(|list| best(list, name))
            {
                return Some(found);
            }
        }

        // 3. Anything else, already ordered by weight and then pattern length.
        self.others
            .iter()
            .find(|glob| glob.matches(name))
            .map(|glob| glob.mime.as_str())
    }

    /// An alias resolved to the type it stands for.
    pub fn canonical<'a>(&'a self, mime: &'a str) -> &'a str {
        self.aliases.get(mime).map_or(mime, String::as_str)
    }

    /// Icon names to try for a type, most specific first.
    ///
    /// Four tiers, the same as the file manager's: the `icons` override for a type that names
    /// its own; the spec's `type/subtype` to `type-subtype`; the `generic-icons` hint; and
    /// `<media>-x-generic`, which is what actually draws for most files, because a theme carries
    /// a dozen generic icons and not a thousand specific ones.
    pub fn icon_names(&self, mime: &str) -> Vec<String> {
        let canonical = self.canonical(mime);
        let mut names = Vec::with_capacity(4);
        if let Some(icon) = self.icons.get(canonical) {
            names.push(icon.clone());
        }
        names.push(canonical.replace('/', "-"));
        if let Some(generic) = self.generic_icons.get(canonical) {
            names.push(generic.clone());
        }
        if let Some((media, _)) = canonical.split_once('/') {
            names.push(format!("{media}-x-generic"));
        }
        names.dedup();
        names
    }

    fn load_globs(&mut self, path: &Path) {
        for line in lines(path) {
            // weight:mimetype:pattern[:flags]. The pattern may itself contain a colon, so this
            // splits from the left a fixed number of times rather than on every one.
            let mut parts = line.splitn(3, ':');
            let (Some(weight), Some(mime), Some(rest)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Ok(weight) = weight.parse() else {
                continue;
            };
            // The only flag there is, trailing. Anything else after a colon is pattern.
            let (pattern, case_sensitive) = match rest.strip_suffix(":cs") {
                Some(pattern) => (pattern, true),
                None => (rest, false),
            };
            if mime.is_empty() || pattern.is_empty() {
                continue;
            }

            let glob = Glob {
                weight,
                mime: mime.to_owned(),
                pattern: pattern.to_owned(),
                case_sensitive,
            };
            match glob.kind() {
                GlobKind::Literal => self
                    .literals
                    .entry(glob.pattern.to_lowercase())
                    .or_default()
                    .push(glob),
                GlobKind::Extension => self
                    .extensions
                    .entry(glob.pattern[1..].to_lowercase())
                    .or_default()
                    .push(glob),
                GlobKind::Other => self.others.push(glob),
            }
        }
    }
}

/// The first of several patterns that matched the same key and genuinely matches `name`.
///
/// The check matters because a case-sensitive glob shares its bucket with case-insensitive ones.
fn best<'a>(candidates: &'a [Glob], name: &str) -> Option<&'a str> {
    candidates
        .iter()
        .find(|glob| glob.matches(name))
        .map(|glob| glob.mime.as_str())
}

/// The `mime` directories, in precedence order.
fn search_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")));
    if let Some(home) = home {
        dirs.push(home.join("mime"));
    }
    let system = std::env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(std::env::split_paths(&system).map(|dir| dir.join("mime")));
    dirs
}

/// A database file's lines, without comments and blanks.
///
/// A missing file is normal, not an error: a data directory may exist with no compiled mime
/// database under it, and `icons` is often absent entirely.
fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `left<separator>right` pairs from a database file.
fn pairs(path: &Path, separator: char) -> Vec<(String, String)> {
    lines(path)
        .into_iter()
        .filter_map(|line| {
            let (left, right) = line.split_once(separator)?;
            (!left.is_empty() && !right.is_empty()).then(|| (left.to_owned(), right.to_owned()))
        })
        .collect()
}

/// Shell-style matching: `*`, `?` and `[...]` classes, with `!` or `^` negating a class.
///
/// All a `globs2` pattern uses. Matched over characters rather than bytes, so `?` is one
/// character of a non-ASCII name and not half of one.
fn fnmatch(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    // Where the last `*` was, and how much of the name it had swallowed, for backtracking.
    let mut star: Option<(usize, usize)> = None;

    while n < name.len() {
        let step = match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
                continue;
            }
            Some('?') => Some(p + 1),
            Some('[') => class(&pattern, p, name[n]),
            Some(&literal) => (literal == name[n]).then_some(p + 1),
            None => None,
        };
        match step {
            Some(next) => {
                p = next;
                n += 1;
            }
            // A mismatch: let the last star swallow one more character, or fail.
            None => match star {
                Some((star_p, star_n)) => {
                    star = Some((star_p, star_n + 1));
                    p = star_p + 1;
                    n = star_n + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// Match `c` against the class opening at `pattern[open]`, returning where the pattern
/// continues if it matches. An unterminated `[` is taken as a literal bracket.
fn class(pattern: &[char], open: usize, c: char) -> Option<usize> {
    let mut i = open + 1;
    let negated = matches!(pattern.get(i), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    loop {
        match pattern.get(i) {
            // A `]` straight after the opening is a member, not the end.
            Some(']') if !first => break,
            Some(&low) => {
                if pattern.get(i + 1) == Some(&'-') && pattern.get(i + 2).is_some_and(|&h| h != ']')
                {
                    matched |= (low..=pattern[i + 2]).contains(&c);
                    i += 3;
                } else {
                    matched |= low == c;
                    i += 1;
                }
            }
            None => return (c == '[').then_some(open + 1),
        }
        first = false;
    }
    (matched != negated).then_some(i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database built from text, written to a scratch `mime` directory.
    fn database(name: &str, files: &[(&str, &str)]) -> Database {
        let dir =
            std::env::temp_dir().join(format!("wlrix-desktop-mime-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        for (file, text) in files {
            std::fs::write(dir.join(file), text).expect("write");
        }
        let db = Database::load(std::slice::from_ref(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        db
    }

    fn globs(name: &str, text: &str) -> Database {
        database(name, &[("globs2", text)])
    }

    #[test]
    fn an_extension_names_the_type_whatever_its_case() {
        let db = globs("ext", "50:image/png:*.png\n");
        assert_eq!(db.resolve_by_name("photo.png"), Some("image/png"));
        assert_eq!(db.resolve_by_name("PHOTO.PNG"), Some("image/png"));
        assert_eq!(db.resolve_by_name("photo.jpeg"), None);
        assert_eq!(db.resolve_by_name("png"), None);
    }

    #[test]
    fn the_longest_extension_wins() {
        let db = globs(
            "longest",
            "50:application/gzip:*.gz\n50:application/x-compressed-tar:*.tar.gz\n",
        );
        assert_eq!(
            db.resolve_by_name("backup.tar.gz"),
            Some("application/x-compressed-tar")
        );
        assert_eq!(db.resolve_by_name("notes.gz"), Some("application/gzip"));
    }

    #[test]
    fn a_literal_name_beats_a_wildcard() {
        // `core` is a core dump, and some `*e` pattern must not claim it.
        let db = globs(
            "literal",
            "50:application/x-core:core\n90:text/x-something:*e\n",
        );
        assert_eq!(db.resolve_by_name("core"), Some("application/x-core"));
        assert_eq!(db.resolve_by_name("table"), Some("text/x-something"));
    }

    #[test]
    fn a_case_sensitive_glob_only_claims_its_own_case() {
        let db = globs("cs", "50:text/x-c++src:*.C:cs\n50:text/x-csrc:*.c\n");
        assert_eq!(db.resolve_by_name("main.C"), Some("text/x-c++src"));
        assert_eq!(db.resolve_by_name("main.c"), Some("text/x-csrc"));
    }

    #[test]
    fn the_heavier_glob_wins_a_shared_extension() {
        let db = globs("weight", "40:text/x-weak:*.x\n80:text/x-strong:*.x\n");
        assert_eq!(db.resolve_by_name("file.x"), Some("text/x-strong"));
    }

    #[test]
    fn other_patterns_are_matched_as_globs() {
        let db = globs("other", "50:text/x-readme:README*\n50:text/x-log:*.[0-9]\n");
        assert_eq!(db.resolve_by_name("README.md2"), Some("text/x-readme"));
        assert_eq!(db.resolve_by_name("syslog.1"), Some("text/x-log"));
        assert_eq!(db.resolve_by_name("syslog.x"), None);
    }

    #[test]
    fn icon_names_run_from_specific_to_generic() {
        let db = database(
            "icons",
            &[
                ("globs2", "50:text/markdown:*.md\n"),
                ("generic-icons", "text/markdown:text-x-generic\n"),
            ],
        );
        assert_eq!(
            db.icon_names("text/markdown"),
            ["text-markdown", "text-x-generic"],
            "the generic hint and <media>-x-generic are the same name here, and named once"
        );
    }

    #[test]
    fn an_explicit_icon_comes_first_and_an_alias_is_followed() {
        let db = database(
            "alias",
            &[
                ("aliases", "application/x-pdf application/pdf\n"),
                ("icons", "application/pdf:x-office-document\n"),
            ],
        );
        assert_eq!(
            db.icon_names("application/x-pdf"),
            [
                "x-office-document",
                "application-pdf",
                "application-x-generic"
            ]
        );
    }

    #[test]
    fn a_missing_database_resolves_nothing_and_still_names_icons() {
        let db = Database::load(&[PathBuf::from("/nonexistent/mime")]);
        assert_eq!(db.resolve_by_name("photo.png"), None);
        assert_eq!(
            db.icon_names(DIRECTORY),
            ["inode-directory", "inode-x-generic"]
        );
    }

    #[test]
    fn fnmatch_handles_what_globs2_uses() {
        assert!(fnmatch("*", ""));
        assert!(fnmatch("*.txt", "a.txt"));
        assert!(!fnmatch("*.txt", "a.txt.bak"));
        assert!(fnmatch("a?c", "abc"));
        assert!(fnmatch("a?c", "aéc"), "? is one character, not one byte");
        assert!(fnmatch("[abc]x", "bx"));
        assert!(!fnmatch("[!abc]x", "bx"));
        assert!(fnmatch("[a-c]*", "core"));
        assert!(fnmatch("[]]", "]"), "a leading ] is a member");
        assert!(
            fnmatch("a[", "a["),
            "an unterminated class is a literal bracket"
        );
        assert!(fnmatch("*a*b", "xaxxb"));
        assert!(!fnmatch("*a*b", "xaxxc"));
    }

    #[test]
    fn this_machines_database_knows_a_png() {
        // A smoke test against the installed database: the fixtures cannot notice the real
        // file format drifting, and this will.
        let db = Database::load(&search_paths());
        if db.literals.is_empty() && db.extensions.is_empty() {
            return; // No shared-mime-info here at all; nothing to check against.
        }
        assert_eq!(db.resolve_by_name("photo.png"), Some("image/png"));
    }
}
