// SPDX-License-Identifier: GPL-3.0-only

//! Which process owns which window, asked of the compositor.
//!
//! A Wayland client cannot learn another client's pid, and the toplevel-info protocol carries
//! no pid field, so `fun.wmde.Comp.Windows` in the compositor publishes the mapping instead.
//!
//! Owning the bus name is not incidental. The compositor answers only a caller that owns
//! `fun.wmde.TaskManager`, because a window title names the document, address or conversation
//! that is open. The name is taken for the life of the connection, which the subscription
//! holds open. A second launch finds it taken and hands over to this copy instead - see
//! `crate::single`.

use cosmic::iced::futures::{SinkExt, Stream, channel::mpsc};
use cosmic::iced::stream;

pub const OWN_NAME: &str = "fun.wmde.TaskManager";
/// Where this program answers `Present`, so a second launch can raise this window instead of
/// opening one of its own.
pub const OWN_PATH: &str = "/fun/wmde/TaskManager";
const COMP_NAME: &str = "fun.wmde.Comp";
const COMP_PATH: &str = "/fun/wmde/Comp/Windows";
const COMP_IFACE: &str = "fun.wmde.Comp.Windows";

/// What the bus half reports to the application.
#[derive(Clone, Debug)]
pub enum Event {
    /// The compositor's window list, as it stands.
    Windows(Vec<Window>),
    /// Another launch asked this copy to show itself.
    Present,
}

/// Served on our own name so a second launch has something to call.
struct Instance {
    tx: tokio::sync::mpsc::Sender<()>,
}

#[zbus::interface(name = "fun.wmde.TaskManager")]
impl Instance {
    /// Bring the existing window forward. A full channel means a request is already on its
    /// way to the window; a second one would add nothing.
    async fn present(&self) {
        let _ = self.tx.try_send(());
    }
}

/// One open window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub pid: u32,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
}

/// The window list, refreshed whenever the compositor says it changed.
///
/// A compositor without the interface - an older build, or a session that is not WMDE - is
/// not an error here. The list simply stays empty, and every process then stands as its own
/// row instead of being folded under the application that owns its window.
pub fn subscription() -> impl Stream<Item = Event> {
    stream::channel(4, |mut output: mpsc::Sender<Event>| async move {
        let (present_tx, mut present_rx) = tokio::sync::mpsc::channel(1);
        // Bound to the whole stream: dropping the connection drops the name, and the
        // compositor would start refusing us.
        let connection = match zbus::connection::Builder::session()
            .and_then(|builder| builder.serve_at(OWN_PATH, Instance { tx: present_tx }))
            .and_then(|builder| builder.name(OWN_NAME))
            .map(zbus::connection::Builder::build)
        {
            Ok(building) => match building.await {
                Ok(connection) => connection,
                Err(err) => return stall(&format!("cannot own {OWN_NAME}: {err}")).await,
            },
            Err(err) => return stall(&format!("cannot own {OWN_NAME}: {err}")).await,
        };

        let proxy = match zbus::Proxy::new(&connection, COMP_NAME, COMP_PATH, COMP_IFACE).await {
            Ok(proxy) => proxy,
            Err(err) => {
                // Not fatal: the process list works without it, only the grouping is poorer.
                tracing::info!("no window list from the compositor: {err}");
                return stall("").await;
            }
        };

        let mut changed = match proxy.receive_signal("Changed").await {
            Ok(stream) => stream,
            Err(err) => {
                tracing::info!("cannot watch the window list: {err}");
                return stall("").await;
            }
        };

        // Once before waiting: the compositor signals changes, not the state it starts in.
        if output.send(Event::Windows(list(&proxy).await)).await.is_err() {
            return;
        }

        use cosmic::iced::futures::StreamExt as _;
        loop {
            let event = tokio::select! {
                signal = changed.next() => match signal {
                    Some(_) => Event::Windows(list(&proxy).await),
                    None => return,
                },
                request = present_rx.recv() => match request {
                    Some(()) => Event::Present,
                    None => return,
                },
            };
            if output.send(event).await.is_err() {
                return;
            }
        }
    })
}

async fn list(proxy: &zbus::Proxy<'_>) -> Vec<Window> {
    match proxy.call::<_, _, Vec<(u32, String, String, bool)>>("List", &()).await {
        Ok(rows) => rows
            .into_iter()
            .map(|(pid, app_id, title, activated)| Window {
                pid,
                app_id,
                title,
                activated,
            })
            .collect(),
        Err(err) => {
            tracing::warn!("the compositor refused the window list: {err}");
            Vec::new()
        }
    }
}

/// Wait forever instead of returning. A subscription that ends is restarted, and retrying a
/// name somebody else holds - or an interface that is not there - in a tight loop helps
/// nobody.
async fn stall(complaint: &str) {
    if !complaint.is_empty() {
        tracing::error!("{complaint}");
    }
    cosmic::iced::futures::future::pending::<()>().await;
}
