// SPDX-License-Identifier: GPL-3.0-or-later
//! wlRIX desktop icons, as a library.
//!
//! The binary (`main.rs`) is a thin shell over this, so the layout and file-model code can
//! be exercised by tests without a compositor.

pub mod config;
pub mod desktop_entry;
pub mod entries;
pub mod icon_theme;
pub mod layout;
pub mod locale;
pub mod menu;
pub mod open;
pub mod pidfile;
pub mod running;
pub mod select;
pub mod session;
pub mod signals;
pub mod state;
pub mod trash;
pub mod ui;
pub mod watch;
pub mod xdg;
