// SPDX-License-Identifier: GPL-3.0-or-later
//! Says which file an `Icon=` name resolves to, and which lookup found it.
//!
//! A launcher whose icon cannot be found is drawn as a bare magic carpet with no symbol on it,
//! and from the desktop that looks the same as an application that simply has no icon. This
//! prints the decision instead: whether the configured theme had it, whether the fallback did,
//! or whether neither did.
//!
//! It goes through [`wlrix_desktop::icon_theme::Icons`], the same lookup the desktop uses, so it
//! cannot disagree with what is actually drawn.
//!
//! ```sh
//! cargo run --example explain_icon -- firefox org.gnome.Nautilus input-keyboard-symbolic
//! cargo run --example explain_icon -- --desktop        # every launcher on the desktop
//! ```
//!
//! The table goes to stdout. The `no icon found for …` lines on **stderr** come from the second,
//! themeless lookup this tool runs to work out *which* half found the file -- they are expected,
//! and are not what the desktop would log. `2>/dev/null` if they are in the way.
//!
//! Not part of the desktop; a dev tool only.

use wlrix_desktop::config::Config;
use wlrix_desktop::entries;
use wlrix_desktop::icon_theme::Icons;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "usage: explain_icon <Icon= name>...\n       \
             explain_icon --desktop\n\n\
             Prints the file each name resolves to, through wlrix-desktop's own lookup."
        );
        std::process::exit(2);
    }

    let config = Config::load();
    let metrics = config.metrics.resolve();
    let theme = config.appearance.icon_theme();
    println!(
        "theme: {}",
        match theme {
            "" => "none configured -- hicolor and /usr/share/pixmaps only",
            theme => theme,
        }
    );
    println!("size:  {}", metrics.icon);
    println!();

    // Two lookups rather than one, so the output can say *which* half found the file: the whole
    // question this tool exists for is whether the theme is pulling its weight. The desktop
    // itself only needs the answer. The fallback probe complains on stderr for every miss, which
    // is why the module comment warns about it.
    let mut configured = Icons::default();
    configured.set_theme(theme);
    let mut fallback = Icons::default();

    let names = if args.iter().any(|arg| arg == "--desktop") {
        desktop_icon_names()
    } else {
        args
    };
    if names.is_empty() {
        println!("nothing to look up");
        return;
    }

    let mut missing = 0;
    for name in &names {
        let found = configured.resolve(name, metrics.icon);
        let without = fallback.resolve(name, metrics.icon);
        let how = match (&found, &without) {
            (None, _) => {
                missing += 1;
                "NOT FOUND -- this launcher gets a bare carpet".to_string()
            }
            // The interesting case, and the whole reason the theme setting exists: the fallback
            // alone would have come up empty.
            (Some(path), None) => format!("{} (theme only)", path.display()),
            (Some(path), Some(_)) => format!("{} (fallback would do)", path.display()),
        };
        println!("{name:<40} {how}");
    }

    println!();
    println!("{} of {} resolved", names.len() - missing, names.len());
}

/// Every `Icon=` name on the user's desktop, launchers only.
fn desktop_icon_names() -> Vec<String> {
    let Some(directory) = wlrix_desktop::xdg::desktop_dir() else {
        eprintln!("explain_icon: no desktop directory");
        return Vec::new();
    };
    // Both the resting icon and the running one, since a launcher can name a different symbol
    // for each and only one of the two would otherwise be checked.
    let mut names: Vec<String> = entries::read(&directory)
        .iter()
        .filter_map(|entry| entry.launcher.as_ref())
        .flat_map(|launcher| [launcher.icon.clone(), launcher.running_icon.clone()])
        .flatten()
        .filter(|icon| !icon.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}
