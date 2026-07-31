// SPDX-License-Identifier: GPL-3.0-or-later
//! Which translation of a `.desktop` file's text to use.
//!
//! The Desktop Entry Specification puts translations in the key itself -- `Name`, `Name[fr]`,
//! `Name[zh_CN]` -- and [says exactly][spec] which of them a given locale picks. This is that
//! rule, plus the reading of the environment that feeds it.
//!
//! [spec]: https://specifications.freedesktop.org/desktop-entry-spec/latest/localized-keys.html
//!
//! The locale is `lang_COUNTRY.ENCODING@MODIFIER`, and everything but `lang` may be missing.
//! Matching drops the encoding entirely and then tries, most specific first:
//!
//! | locale        | keys tried                                  |
//! |---------------|---------------------------------------------|
//! | `sr_RS@latin` | `sr_RS@latin`, `sr_RS`, `sr@latin`, `sr`    |
//! | `zh_TW.UTF-8` | `zh_TW`, `zh`                               |
//! | `ja`          | `ja`                                        |
//!
//! and falls back to the unlocalized key when none of them is in the file. Note that this is
//! *not* a general "try a less specific language" rule: `fr_CA` falls back to `fr`, but `pt_BR`
//! does **not** fall back to `pt` past what the table gives -- the spec's list is the whole of
//! it, and inventing more matches would show Portuguese to someone who asked for Brazilian
//! Portuguese only by accident of the two sharing a prefix.

use std::sync::OnceLock;

/// A locale, reduced to the key suffixes it will accept.
///
/// Parsed once rather than per lookup: a desktop of launchers asks the same question of every
/// file on every rescan, and the answer cannot change while the process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locale {
    /// The `[…]` suffixes to try, most specific first. Empty means no translation is wanted,
    /// which is what `C` and an unset environment both mean.
    tags: Vec<String>,
}

impl Locale {
    /// The locale this process is running under.
    ///
    /// Cached, because it is read for every localized key of every launcher on the desktop and
    /// the environment cannot change underneath a running session.
    pub fn current() -> &'static Self {
        static CURRENT: OnceLock<Locale> = OnceLock::new();
        CURRENT.get_or_init(Self::from_env)
    }

    /// Read the locale from the environment.
    ///
    /// POSIX's order for the category that covers user-visible text: `LC_ALL` overrides
    /// everything, then `LC_MESSAGES`, then `LANG` as the fallback for both. An empty value is
    /// treated as unset, as POSIX says.
    pub fn from_env() -> Self {
        for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            match std::env::var(name) {
                Ok(value) if !value.trim().is_empty() => return Self::parse(&value),
                _ => continue,
            }
        }
        Self::untranslated()
    }

    /// The locale that wants no translation at all: only unlocalized keys match.
    pub fn untranslated() -> Self {
        Self { tags: Vec::new() }
    }

    /// Work out which key suffixes a locale string accepts.
    pub fn parse(spec: &str) -> Self {
        let spec = spec.trim();
        // POSIX's two names for "no locale". Neither is a language, and treating either as one
        // would have every file matched against a `[C]` key that no file has.
        if spec.is_empty() || spec == "C" || spec == "POSIX" {
            return Self::untranslated();
        }

        // The encoding takes no part in matching, so it goes first and never comes back:
        // `ja_JP.UTF-8` and `ja_JP` have to behave identically.
        let (head, modifier) = match spec.split_once('@') {
            Some((head, modifier)) => (head, Some(modifier).filter(|m| !m.is_empty())),
            None => (spec, None),
        };
        let head = head.split_once('.').map_or(head, |(before, _)| before);
        let (lang, country) = match head.split_once('_') {
            Some((lang, country)) => (lang, Some(country).filter(|c| !c.is_empty())),
            None => (head, None),
        };
        if lang.is_empty() {
            return Self::untranslated();
        }

        // The spec's table, written out: each line is skipped when it needs a part the locale
        // did not give.
        let mut tags = Vec::with_capacity(4);
        if let Some(country) = country {
            if let Some(modifier) = modifier {
                tags.push(format!("{lang}_{country}@{modifier}"));
            }
            tags.push(format!("{lang}_{country}"));
        }
        if let Some(modifier) = modifier {
            tags.push(format!("{lang}@{modifier}"));
        }
        tags.push(lang.to_owned());

        Self { tags }
    }

    /// The keys to try for `key`, most specific first, ending with the unlocalized one.
    ///
    /// Returns names rather than doing the lookup so the caller keeps its own field map: a
    /// desktop file is read into a plain `key -> value` map, and localization is only ever a
    /// question of which key to ask for.
    pub fn candidates<'a>(&'a self, key: &'a str) -> impl Iterator<Item = String> + 'a {
        self.tags
            .iter()
            .map(move |tag| format!("{key}[{tag}]"))
            .chain(std::iter::once(key.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(spec: &str) -> Vec<String> {
        Locale::parse(spec).tags
    }

    #[test]
    fn a_bare_language_tries_only_itself() {
        assert_eq!(tags("ja"), ["ja"]);
    }

    #[test]
    fn a_country_falls_back_to_the_language() {
        assert_eq!(tags("zh_TW"), ["zh_TW", "zh"]);
    }

    #[test]
    fn the_encoding_is_dropped_and_changes_nothing() {
        // `LANG` almost always carries one, so this is the ordinary case rather than an edge.
        assert_eq!(tags("zh_TW.UTF-8"), tags("zh_TW"));
        assert_eq!(tags("ja_JP.eucJP"), ["ja_JP", "ja"]);
    }

    #[test]
    fn a_modifier_gives_the_full_four_way_fallback() {
        assert_eq!(
            tags("sr_RS@latin"),
            ["sr_RS@latin", "sr_RS", "sr@latin", "sr"]
        );
    }

    #[test]
    fn a_modifier_without_a_country_skips_the_country_lines() {
        assert_eq!(tags("sr@latin"), ["sr@latin", "sr"]);
    }

    #[test]
    fn encoding_and_modifier_together() {
        assert_eq!(tags("sr_RS.UTF-8@latin"), tags("sr_RS@latin"));
    }

    #[test]
    fn the_c_locale_wants_no_translation() {
        assert!(tags("C").is_empty());
        assert!(tags("POSIX").is_empty());
        assert!(tags("").is_empty());
    }

    #[test]
    fn a_locale_that_is_only_punctuation_is_not_a_language() {
        // Nonsense in the environment must end at the unlocalized key, not at a key named
        // `Name[]` or `Name[@x]` that could never be in a file anyway.
        assert!(tags("_RS").is_empty());
        assert!(tags("@latin").is_empty());
    }

    #[test]
    fn empty_parts_are_treated_as_absent() {
        assert_eq!(tags("fr_"), ["fr"]);
        assert_eq!(tags("fr@"), ["fr"]);
    }

    #[test]
    fn candidates_end_at_the_unlocalized_key() {
        let candidates: Vec<String> = Locale::parse("zh_TW").candidates("Name").collect();
        assert_eq!(candidates, ["Name[zh_TW]", "Name[zh]", "Name"]);
    }

    #[test]
    fn an_untranslated_locale_asks_only_for_the_plain_key() {
        let candidates: Vec<String> = Locale::untranslated().candidates("Name").collect();
        assert_eq!(candidates, ["Name"]);
    }

    #[test]
    fn a_language_is_not_matched_by_a_shared_prefix() {
        // `pt_BR` must not reach a `pt` key by way of some cleverer rule than the spec's, and
        // `pt` must never reach `pt_BR` -- the fallback only ever gets less specific.
        assert!(!tags("pt").contains(&"pt_BR".to_owned()));
        assert_eq!(tags("pt_BR"), ["pt_BR", "pt"]);
    }
}
