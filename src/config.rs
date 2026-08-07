// SPDX-License-Identifier: GPL-3.0-or-later
//! The hand-edited settings file.
//!
//! ```toml
//! # ~/.config/wlrix/desktop.toml
//! snap_to_grid = false    # icons drop where released; true aligns them to cells
//! output = "DP-1"         # which monitor gets the icons; default is the leftmost
//!
//! [metrics]
//! icon = 64               # the icon artwork, square
//! cell_width = 96         # one cell: the icon plus its label
//! cell_height = 92
//! gap = 8                 # between cells
//! margin = 12             # between the outermost cells and the screen edge
//! ```
//!
//! Read from the user's config directory first, then `/etc/wlrix`; the first file found wins
//! outright rather than merging, so what a user sees in their own file is the whole of what
//! they get. This mirrors the compositor and the session deliberately: one shape of file
//! across the stack.
//!
//! Unknown keys are an error, for the same reason the compositor rejects them -- a silently
//! ignored typo in a config file is a bad afternoon.
//!
//! These are *defaults*. The live snap setting is in the state file, which wins; see
//! [`crate::state`].

use serde::Deserialize;

use crate::layout::Metrics;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Whether a dropped icon aligns to a grid cell. The user can flip this at runtime, and
    /// the state file remembers what they chose; this is only the starting value.
    #[serde(default)]
    pub snap_to_grid: bool,
    /// Which monitor the icons appear on, by connector name (`DP-1`, `Virtual-1`, ...).
    /// Absent means "work it out" -- see [`crate::ui`].
    #[serde(default)]
    pub output: Option<String>,
    /// What opens a file or a URL, as a command and its leading arguments; the target is
    /// appended. Empty means the default, `xdg-open`, which consults the user's MIME
    /// associations.
    #[serde(default)]
    pub open: Vec<String>,
    /// What wraps a `Terminal=true` launcher, as a command and its leading arguments; the
    /// program is appended. Empty means work it out -- see [`crate::open::terminal_argv`].
    #[serde(default)]
    pub terminal: Vec<String>,
    #[serde(default)]
    pub metrics: MetricsConfig,
}

/// Cell geometry. Every field is optional and falls back to [`Metrics::default`], so a file
/// that only wants bigger icons says only that.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    pub icon: Option<i32>,
    pub cell_width: Option<i32>,
    pub cell_height: Option<i32>,
    pub gap: Option<i32>,
    pub margin: Option<i32>,
}

impl MetricsConfig {
    /// The configured metrics, with anything unset left at the default.
    ///
    /// Values are floored at sane minimums rather than rejected: a cell narrower than its
    /// icon, or a negative gap, would put the layout arithmetic somewhere it cannot recover
    /// from, and refusing to start over a silly number in a config file helps nobody.
    pub fn resolve(&self) -> Metrics {
        let base = Metrics::default();
        let icon = self.icon.unwrap_or(base.icon).max(8);
        Metrics {
            icon,
            cell_w: self.cell_width.unwrap_or(base.cell_w).max(icon),
            cell_h: self.cell_height.unwrap_or(base.cell_h).max(icon),
            gap: self.gap.unwrap_or(base.gap).max(0),
            margin: self.margin.unwrap_or(base.margin).max(0),
        }
    }
}

/// Parse a candidate config file, for `--check-config`.
///
/// This program's own serde types are the authority on what `desktop.toml` may contain.
/// `wlrix-settings-daemon` writes a temporary file and runs this against it before renaming it
/// into place, so a settings app cannot produce a file this program would refuse -- which
/// matters because `deny_unknown_fields` means one wrong key costs the *whole* file and the
/// user silently gets built-in defaults for all of it.
///
/// Deliberately not [`Config::load`]: that reports to stderr and carries on with defaults,
/// which is right at startup -- a typo should cost the setting, not the desktop -- and exactly
/// wrong here, where the question *is* whether the file is acceptable.
pub fn check(path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    toml::from_str::<Config>(&text)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

impl Config {
    /// The opener command, falling back to `xdg-open`.
    pub fn open_command(&self) -> Vec<String> {
        if self.open.is_empty() {
            return vec!["xdg-open".to_owned()];
        }
        self.open.clone()
    }

    /// The configured terminal wrapper, if there is one. `None` means "work it out".
    pub fn terminal_command(&self) -> Option<Vec<String>> {
        (!self.terminal.is_empty()).then(|| self.terminal.clone())
    }

    /// Load the first config file that exists. No file at all is not an error -- the
    /// defaults are a working desktop.
    pub fn load() -> Self {
        for path in crate::xdg::config_paths() {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                // Not-found is the ordinary case; only real errors are worth a line.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    eprintln!("wlrix-desktop: could not read {}: {err}", path.display());
                    continue;
                }
            };
            match toml::from_str::<Self>(&text) {
                Ok(config) => return config,
                Err(err) => {
                    // Loud, then carry on with defaults: a broken config should not cost the
                    // user their desktop icons.
                    eprintln!("wlrix-desktop: {} is not valid: {err}", path.display());
                    return Self::default();
                }
            }
        }
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_all_defaults() {
        let config: Config = toml::from_str("").expect("empty config should parse");
        assert!(!config.snap_to_grid);
        assert_eq!(config.output, None);
        assert_eq!(config.metrics.resolve(), Metrics::default());
    }

    #[test]
    fn metrics_override_one_field_at_a_time() {
        let config: Config = toml::from_str("[metrics]\nicon = 48\n").unwrap();
        let metrics = config.metrics.resolve();
        assert_eq!(metrics.icon, 48);
        // Everything else untouched.
        assert_eq!(metrics.gap, Metrics::default().gap);
        assert_eq!(metrics.margin, Metrics::default().margin);
    }

    #[test]
    fn a_typo_is_refused_rather_than_ignored() {
        // `snap_to_gird` would otherwise silently do nothing, and the user would never
        // find out why their setting had no effect.
        assert!(toml::from_str::<Config>("snap_to_gird = true\n").is_err());
    }

    #[test]
    fn nonsense_geometry_is_clamped_not_fatal() {
        let config: Config = toml::from_str(
            "[metrics]\nicon = -10\ncell_width = 1\ncell_height = 1\ngap = -5\nmargin = -5\n",
        )
        .unwrap();
        let metrics = config.metrics.resolve();
        assert!(metrics.icon >= 8);
        assert!(metrics.cell_w >= metrics.icon, "a cell must hold its icon");
        assert!(metrics.cell_h >= metrics.icon);
        assert!(metrics.gap >= 0);
        assert!(metrics.margin >= 0);
    }

    #[test]
    fn a_full_file_round_trips() {
        let config: Config = toml::from_str(
            "snap_to_grid = true\n\
             output = \"DP-1\"\n\
             [metrics]\n\
             icon = 48\n\
             cell_width = 80\n\
             cell_height = 76\n\
             gap = 4\n\
             margin = 20\n",
        )
        .unwrap();
        assert!(config.snap_to_grid);
        assert_eq!(config.output.as_deref(), Some("DP-1"));
        assert_eq!(
            config.metrics.resolve(),
            Metrics {
                icon: 48,
                cell_w: 80,
                cell_h: 76,
                gap: 4,
                margin: 20,
            }
        );
    }
}
