// SPDX-License-Identifier: GPL-3.0-or-later
//! wlRIX desktop icons, as a library.
//!
//! The binary (`main.rs`) is a thin shell over this, so the layout and file-model code can
//! be exercised by tests without a compositor.

pub mod config;
pub mod entries;
pub mod layout;
pub mod select;
pub mod state;
pub mod theme;
pub mod ui;
pub mod watch;
pub mod xdg;
