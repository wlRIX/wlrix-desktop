// SPDX-License-Identifier: GPL-3.0-or-later
//! Says what double-clicking a file would do, without doing it.
//!
//! Opening a file has several ways to come to nothing -- a `.desktop` file without the execute
//! bit, a `TryExec` naming a binary that is not installed, a `Terminal=true` entry with no
//! terminal to be found -- and from the desktop all of them look the same: a double-click, and
//! nothing happens. This prints the decision instead.
//!
//! It asks [`wlrix_desktop::open::plan`] rather than working the answer out again, so it
//! cannot disagree with what the desktop actually does.
//!
//! ```sh
//! cargo run --example explain_open -- ~/Desktop/*
//! ```
//!
//! Not part of the desktop; a dev tool only.

use std::path::PathBuf;

use wlrix_desktop::config::Config;
use wlrix_desktop::entries::Entry;
use wlrix_desktop::open;

fn main() {
    let paths: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!(
            "usage: explain_open <file>...\n\n\
             Prints what wlrix-desktop would run for each file, and why, without running it."
        );
        std::process::exit(2);
    }

    let config = Config::load();
    println!("opener:   {:?}", config.open_command());
    println!(
        "terminal: {}",
        match open::terminal_argv(&config) {
            Some(argv) => format!("{argv:?}"),
            None => "none found".to_string(),
        }
    );
    println!();

    for path in &paths {
        println!("{}", path.display());
        let Some(entry) = Entry::at(path) else {
            println!("  not something the desktop would show (hidden, missing, or unreadable)\n");
            continue;
        };
        println!("  kind: {:?}", entry.kind);
        match open::plan(&entry, &config) {
            Ok(plan) => {
                println!("  would run: {:?}", plan.argv);
                if let Some(directory) = plan.directory {
                    println!("  in: {}", directory.display());
                }
            }
            Err(why) => println!("  would refuse: {why}"),
        }
        println!();
    }
}
