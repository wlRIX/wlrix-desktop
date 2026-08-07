// SPDX-License-Identifier: GPL-3.0-or-later
//! Re-reading `desktop.toml` when asked to.
//!
//! `SIGHUP` means "re-read the config", turned into a calloop [`Ping`] fired from the handler --
//! an eventfd write, which is async-signal-safe -- whose source on the event loop does the
//! actual work, where it can touch the Wayland connection and the layout without racing
//! anything. The same shape `wlrix-idle` and `wlrix-compositor` use.
//!
//! Only `SIGHUP`. Stopping needs no handler here: this is a Wayland client, and the ordinary end
//! of its life is the compositor going away, which the event loop already reports. `wlrix-idle`
//! installs a quit handler because it is the thing that switched the monitors off and the only
//! thing that will switch them back on; a desktop that dies leaves nothing behind but a missing
//! backdrop.

use std::sync::OnceLock;

use smithay_client_toolkit::reexports::calloop::ping::Ping;

/// The ping the `SIGHUP` handler fires, to reload the config on the event loop.
static RELOAD: OnceLock<Ping> = OnceLock::new();

/// Install a `SIGHUP` handler that fires `reload`, so the loop can re-read the config.
pub fn forward_reload_to_loop(reload: Ping) {
    if RELOAD.set(reload).is_err() {
        return;
    }
    // SAFETY: the handler does only async-signal-safe work -- firing the ping, which is an
    // eventfd write.
    unsafe {
        libc::signal(
            libc::SIGHUP,
            handle_reload as *const () as libc::sighandler_t,
        )
    };
}

/// Runs in signal context; may only do async-signal-safe work.
extern "C" fn handle_reload(_signal: libc::c_int) {
    if let Some(reload) = RELOAD.get() {
        reload.ping();
    }
}
