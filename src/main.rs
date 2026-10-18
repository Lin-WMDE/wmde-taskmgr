// SPDX-License-Identifier: GPL-3.0-only

mod app;
mod devices;
mod disk;
mod facts;
mod gpu;
mod group;
mod localize;
mod memory;
mod net;
mod performance;
mod probe;
mod processes;
mod sessions;
mod single;
mod tabs;
mod users;
mod wifi;
mod windows;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    localize::localize();
    // A second press of the shortcut brings the running copy forward instead of opening a
    // window that the compositor would refuse the window list to.
    match single::claim_or_hand_over() {
        single::Start::HandedOver => Ok(()),
        single::Start::Fresh => app::run(),
    }
}
