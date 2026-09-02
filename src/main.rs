// SPDX-License-Identifier: GPL-3.0-only

mod app;
mod localize;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    localize::localize();
    app::run()
}
