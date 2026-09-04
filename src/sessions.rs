// SPDX-License-Identifier: GPL-3.0-only

//! Who is logged in, asked of logind.
//!
//! A row on the users page is one login, not one account: five ssh connections under the same
//! name are five people at five terminals, and one folded row answers nothing about any of
//! them.
//!
//! `/run/systemd/sessions/<id>` carries all of this and opens with "This is private data. Do
//! not parse.", so it is asked for over the bus instead. What ties a login to a process is the
//! **audit session id**: PAM sets it at login, every child inherits it, and it is what
//! `/proc/PID/sessionid` holds. The cgroup path names a session too, and lies: a program
//! started from the desktop is placed under `user@1000.service`, not under its login's scope.

use std::collections::HashMap;

use cosmic::iced::futures::{SinkExt, Stream, channel::mpsc};
use cosmic::iced::stream;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::fl;

const LOGIND: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER_IFACE: &str = "org.freedesktop.login1.Manager";
const SESSION_IFACE: &str = "org.freedesktop.login1.Session";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";

/// The class logind gives a login of a person. The other classes - `manager`,
/// `manager-early`, `greeter`, `background` - are services of an account, not somebody
/// arriving, and what runs under them belongs in the account's background row.
const CLASS_USER: &str = "user";

/// One login.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    /// Audit session id, the number `/proc/PID/sessionid` carries.
    pub audit: u32,
    pub uid: u32,
    /// The account name logind knows, which beats `/etc/passwd` when the account comes from a
    /// network directory that the passwd file has never heard of.
    pub user: String,
    pub kind: Kind,
    /// Terminal of the login, empty when it has none - an ssh command without a pseudo
    /// terminal is the ordinary case.
    pub tty: String,
}

/// What kind of login it is, in the terms the page shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    /// A graphical session on a seat.
    Desktop,
    /// A text login at a local terminal.
    Console,
    /// A login that came over the network.
    Remote { service: String, host: String },
}

impl Session {
    /// What the session column shows.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.kind {
            Kind::Desktop => fl!("session-desktop"),
            Kind::Console if self.tty.is_empty() => fl!("session-console-plain"),
            Kind::Console => fl!("session-console", tty = self.tty.clone()),
            // Checked before the type, so that a remote desktop reads as remote rather than
            // as a desktop: which machine it is on is the thing worth saying about it.
            Kind::Remote { service, host } => match (service.as_str(), host.is_empty()) {
                ("sshd", false) => fl!("session-remote", host = host.clone()),
                ("sshd", true) => fl!("session-remote-plain"),
                (_, false) => {
                    fl!(
                        "session-remote-service",
                        service = service.clone(),
                        host = host.clone()
                    )
                }
                (_, true) => service.clone(),
            },
        }
    }

    /// What tells two logins with the same label apart: the terminal when there is one, and
    /// the session number when there is not.
    #[must_use]
    pub fn node(&self) -> String {
        if self.tty.is_empty() {
            format!("#{}", self.audit)
        } else {
            self.tty.clone()
        }
    }
}

/// The logins, refreshed whenever logind says one arrived or left.
///
/// A machine without logind is not an error here: the list stays empty, and the users page
/// falls back to a row per account, which is what it did before this existed.
pub fn subscription() -> impl Stream<Item = Vec<Session>> {
    stream::channel(4, |mut output: mpsc::Sender<Vec<Session>>| async move {
        // The system bus, not the session bus: logind is a system service.
        let connection = match zbus::Connection::system().await {
            Ok(connection) => connection,
            Err(err) => {
                tracing::info!("no system bus, so no logins: {err}");
                return stall().await;
            }
        };

        let manager = match zbus::Proxy::new(&connection, LOGIND, MANAGER_PATH, MANAGER_IFACE).await
        {
            Ok(proxy) => proxy,
            Err(err) => {
                tracing::info!("no logind, so no logins: {err}");
                return stall().await;
            }
        };

        let (Ok(mut arrived), Ok(mut left)) = (
            manager.receive_signal("SessionNew").await,
            manager.receive_signal("SessionRemoved").await,
        ) else {
            tracing::info!("cannot watch logins; the list would go stale");
            return stall().await;
        };

        // Once before waiting: logind signals what changes, not what is already there.
        if output
            .send(list(&connection, &manager).await)
            .await
            .is_err()
        {
            return;
        }

        use cosmic::iced::futures::StreamExt as _;
        loop {
            let signalled = tokio::select! {
                signal = arrived.next() => signal.is_some(),
                signal = left.next() => signal.is_some(),
            };
            if !signalled {
                return;
            }
            if output
                .send(list(&connection, &manager).await)
                .await
                .is_err()
            {
                return;
            }
        }
    })
}

/// Every login logind knows, with the properties the page needs.
///
/// One round trip for the list and one `GetAll` per session. Logins are counted in single
/// digits and this runs only when one comes or goes, never on the tick.
async fn list(connection: &zbus::Connection, manager: &zbus::Proxy<'_>) -> Vec<Session> {
    type Listed = Vec<(String, u32, String, String, OwnedObjectPath)>;

    let listed: Listed = match manager.call("ListSessions", &()).await {
        Ok(listed) => listed,
        Err(err) => {
            tracing::warn!("logind refused the session list: {err}");
            return Vec::new();
        }
    };

    let mut out = Vec::with_capacity(listed.len());
    for (id, uid, user, _seat, path) in listed {
        let Ok(properties) = zbus::Proxy::new(connection, LOGIND, path, PROPERTIES_IFACE).await
        else {
            continue;
        };
        let props: HashMap<String, OwnedValue> =
            match properties.call("GetAll", &(SESSION_IFACE)).await {
                Ok(props) => props,
                // A session can end between the list and the read. That is ordinary here.
                Err(err) => {
                    tracing::debug!("session {id} went away while it was being read: {err}");
                    continue;
                }
            };
        if let Some(session) = read(uid, user, &props) {
            out.push(session);
        }
    }
    out
}

/// One session out of its properties, or nothing when it is not a login of a person.
fn read(uid: u32, user: String, props: &HashMap<String, OwnedValue>) -> Option<Session> {
    if text(props, "Class") != CLASS_USER {
        return None;
    }
    // Without audit the kernel writes no session id into /proc either, so a login that cannot
    // be named by that number cannot be matched to a single process and is not worth a row.
    let audit = number(props, "Audit")?;
    if audit == 0 || audit == u32::MAX {
        return None;
    }
    let kind = if flag(props, "Remote") {
        Kind::Remote {
            service: text(props, "Service"),
            host: text(props, "RemoteHost"),
        }
    } else if matches!(text(props, "Type").as_str(), "wayland" | "x11" | "mir") {
        Kind::Desktop
    } else {
        Kind::Console
    };
    Some(Session {
        audit,
        uid,
        user,
        kind,
        tty: text(props, "TTY"),
    })
}

fn text(props: &HashMap<String, OwnedValue>, key: &str) -> String {
    props
        .get(key)
        .and_then(|value| value.downcast_ref::<&str>().ok())
        .unwrap_or_default()
        .to_owned()
}

fn number(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    props
        .get(key)
        .and_then(|value| value.downcast_ref::<u32>().ok())
}

fn flag(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props
        .get(key)
        .and_then(|value| value.downcast_ref::<bool>().ok())
        .unwrap_or_default()
}

/// Wait forever instead of returning. A subscription that ends is restarted, and asking a bus
/// that is not there again every frame helps nobody.
async fn stall() {
    cosmic::iced::futures::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, OwnedValue)]) -> HashMap<String, OwnedValue> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    fn owned(value: &str) -> OwnedValue {
        zbus::zvariant::Value::from(value).try_into().expect("owns")
    }

    #[test]
    fn a_service_manager_is_not_somebody_logging_in() {
        let manager = props(&[
            ("Class", owned("manager")),
            ("Audit", 4u32.into()),
            ("Type", owned("unspecified")),
        ]);
        assert!(read(1000, "softer".into(), &manager).is_none());
    }

    #[test]
    fn a_login_is_named_by_where_it_came_from() {
        let desktop = read(
            1000,
            "softer".into(),
            &props(&[
                ("Class", owned("user")),
                ("Audit", 10u32.into()),
                ("Type", owned("wayland")),
                ("TTY", owned("tty1")),
            ]),
        )
        .expect("a login");
        assert_eq!(desktop.kind, Kind::Desktop);

        let ssh = read(
            1000,
            "softer".into(),
            &props(&[
                ("Class", owned("user")),
                ("Audit", 97u32.into()),
                ("Type", owned("tty")),
                ("Remote", true.into()),
                ("Service", owned("sshd")),
                ("RemoteHost", owned("172.17.6.2")),
            ]),
        )
        .expect("a login");
        assert_eq!(
            ssh.kind,
            Kind::Remote {
                service: "sshd".into(),
                host: "172.17.6.2".into(),
            }
        );
        // No terminal of its own: an ssh command without a pty. The number is then the only
        // thing that tells it from the next one.
        assert_eq!(ssh.node(), "#97");

        let console = read(
            1000,
            "softer".into(),
            &props(&[
                ("Class", owned("user")),
                ("Audit", 3u32.into()),
                ("Type", owned("tty")),
                ("TTY", owned("tty3")),
            ]),
        )
        .expect("a login");
        assert_eq!(console.kind, Kind::Console);
        assert_eq!(console.node(), "tty3");
    }

    /// A remote desktop is remote first: which machine it came from is what tells it apart
    /// from the person sitting at this one.
    #[test]
    fn a_remote_desktop_reads_as_remote() {
        let session = read(
            1000,
            "softer".into(),
            &props(&[
                ("Class", owned("user")),
                ("Audit", 12u32.into()),
                ("Type", owned("x11")),
                ("Remote", true.into()),
                ("Service", owned("xrdp")),
                ("RemoteHost", owned("10.0.0.5")),
            ]),
        )
        .expect("a login");
        assert!(matches!(session.kind, Kind::Remote { .. }));
        let label = session.label();
        assert!(label.contains("xrdp"), "{label}");
        assert!(label.contains("10.0.0.5"), "{label}");
    }
}
