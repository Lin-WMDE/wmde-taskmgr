// SPDX-License-Identifier: GPL-3.0-only

//! One task manager at a time.
//!
//! `Ctrl+Shift+Escape` spawns the program afresh every time it is pressed, so without this a
//! second press would open a second window - and the second copy is not merely redundant, it
//! is worse: the compositor hands the window list only to whoever owns
//! `fun.wmde.TaskManager`, and the name is already taken, so the second window would run
//! blind, with no "Applications" section at all.
//!
//! The name is asked about here rather than claimed here: claiming it needs a connection that
//! outlives this function, and the subscription that talks to the compositor already holds
//! one. Two launches in the very same millisecond can therefore both see the name free and
//! both open a window; that is the price of not threading a connection through startup, and
//! it takes two key presses inside one scheduling quantum to reach.

use crate::windows::{OWN_NAME, OWN_PATH};

/// Whether this process should go on to open a window.
pub enum Start {
    /// Nobody else is running. Open the window.
    Fresh,
    /// Another copy is running and has been asked to show itself. Exit quietly.
    HandedOver,
}

#[must_use]
pub fn claim_or_hand_over() -> Start {
    // A runtime of our own, for the moment before the toolkit starts one: zbus with the
    // tokio feature needs one, and there is none yet.
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return Start::Fresh;
    };

    runtime.block_on(async {
        let Ok(connection) = zbus::Connection::session().await else {
            // No session bus at all. Nothing to hand over to, and the window list will be
            // empty, but a process list without one is still worth showing.
            return Start::Fresh;
        };
        let Ok(dbus) = zbus::fdo::DBusProxy::new(&connection).await else {
            return Start::Fresh;
        };
        let Ok(name) = OWN_NAME.try_into() else {
            return Start::Fresh;
        };
        if dbus.name_has_owner(name).await != Ok(true) {
            return Start::Fresh;
        }

        match zbus::Proxy::new(&connection, OWN_NAME, OWN_PATH, OWN_NAME).await {
            Ok(proxy) => match proxy.call::<_, _, ()>("Present", &()).await {
                Ok(()) => Start::HandedOver,
                // The other copy is on its way out, or is wedged. Opening a window is better
                // than exiting and leaving the person with nothing.
                Err(err) => {
                    tracing::info!("the running copy did not answer: {err}");
                    Start::Fresh
                }
            },
            Err(err) => {
                tracing::info!("cannot reach the running copy: {err}");
                Start::Fresh
            }
        }
    })
}
