// SPDX-License-Identifier: GPL-3.0-or-later
//! wlRIX desktop icons.
//!
//! Shows the files in the user's XDG desktop directory as icons on the desktop, IRIX-style:
//! filling from the top right downward, then leftward. Icons can be dragged anywhere, or
//! aligned to a grid, and where they were left is remembered across restarts.
//!
//! Started by `wlrix-session` alongside the toolchest and the Desks overview. It is a
//! wlr-layer-shell background surface, so it sits below every window; see [`wlrix_desktop::ui`].

fn main() {
    // No option here combines with another, so only the first is looked at -- a loop would
    // read as though `wlrix-desktop --version --help` meant something.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("--help" | "-h") => {
            println!(
                "wlrix-desktop {}\n\n\
                     Desktop icons for the wlRIX desktop. Started by wlrix-session; needs a\n\
                     running compositor with wlr-layer-shell.\n\n\
                     Usage: wlrix-desktop [options]\n\n\
                     Options:\n  \
                       --check-config <path>  say whether that file would be accepted, exit\n  \
                       -h, --help     this message\n  \
                       -V, --version  print the version\n\n\
                     Settings live in ~/.config/wlrix/desktop.toml; icon positions are\n\
                     remembered in $XDG_STATE_HOME/wlrix/desktop-icons.toml.",
                env!("CARGO_PKG_VERSION")
            );
            return;
        }
        Some("--version" | "-V") => {
            println!("wlrix-desktop {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        // Answers a question about a file rather than doing anything with it, so it needs no
        // compositor and starts nothing. `wlrix-settings-daemon` runs this against a candidate
        // file before renaming it into place, which is what stops a settings app from writing
        // a `desktop.toml` this program would refuse.
        Some("--check-config") => {
            let Some(path) = args.get(1) else {
                eprintln!("wlrix-desktop: --check-config needs a path");
                std::process::exit(2);
            };
            if let Err(why) = wlrix_desktop::config::check(std::path::Path::new(path)) {
                eprintln!("{why}");
                std::process::exit(1);
            }
            return;
        }
        Some(other) => {
            eprintln!("wlrix-desktop: unknown argument: {other}");
            std::process::exit(2);
        }
    }

    if let Err(err) = wlrix_desktop::ui::run() {
        eprintln!("wlrix-desktop: {err}");
        std::process::exit(1);
    }
}
