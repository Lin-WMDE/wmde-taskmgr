// SPDX-License-Identifier: GPL-3.0-only

//! The same walk, folded by the login that owns each process.
//!
//! The reference lists the people signed in, and so does this: a row is one login, so five
//! ssh connections under one name are five rows and the desktop is a sixth. What a login is
//! and where it comes from is in `crate::sessions`.
//!
//! Only real accounts, outside those logins. A desktop runs a dozen system accounts - `root`,
//! `dbus`, `polkitd`, `avahi`, `systemd-timesync` - and a page listing their daemons answers
//! nothing the process list does not already answer better.

use std::collections::HashMap;

use crate::fl;
use crate::probe::Process;
use crate::processes::{Column, Row};
use crate::sessions::Session;

/// Where regular accounts begin and end.
///
/// Taken from `/etc/login.defs`, because a machine is free to move the boundary; the defaults
/// are what `useradd` falls back to. `nobody` sits at 65534, above `UID_MAX`, and is excluded
/// by the upper bound rather than by name.
#[derive(Clone, Copy, Debug)]
pub struct Range {
    min: u32,
    max: u32,
}

impl Default for Range {
    fn default() -> Self {
        Self {
            min: 1000,
            max: 60000,
        }
    }
}

impl Range {
    #[must_use]
    pub fn load() -> Self {
        let mut out = Self::default();
        let Ok(defs) = std::fs::read_to_string("/etc/login.defs") else {
            return out;
        };
        for line in defs.lines() {
            let mut f = line.split_whitespace();
            let (Some(key), Some(value)) = (f.next(), f.next()) else {
                continue;
            };
            let Ok(value) = value.parse::<u32>() else {
                continue;
            };
            match key {
                "UID_MIN" => out.min = value,
                "UID_MAX" => out.max = value,
                _ => {}
            }
        }
        out
    }

    #[must_use]
    fn holds(self, uid: u32) -> bool {
        (self.min..=self.max).contains(&uid)
    }
}

/// Names for the user ids that appear, read once from `/etc/passwd`.
///
/// Read directly rather than through `getpwuid`: that would pull libc's NSS in, which on a
/// machine with a network directory means a lookup that blocks, and a task manager that
/// stalls once a second is worse than one that shows a number.
pub struct Names(HashMap<u32, Box<str>>);

impl Names {
    #[must_use]
    pub fn load() -> Self {
        let mut names = HashMap::new();
        if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
            for line in passwd.lines() {
                // name:x:uid:gid:gecos:home:shell
                let mut fields = line.split(':');
                let (Some(name), Some(_), Some(uid)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    continue;
                };
                if let Ok(uid) = uid.parse::<u32>() {
                    names.entry(uid).or_insert_with(|| name.into());
                }
            }
        }
        Self(names)
    }

    /// The account's name, or its number when `/etc/passwd` does not have it - which is the
    /// ordinary case for a container's user seen from outside.
    #[must_use]
    pub fn get(&self, uid: u32) -> String {
        self.0
            .get(&uid)
            .map_or_else(|| uid.to_string(), ToString::to_string)
    }
}

/// What a row of this page stands for: a login, by its audit session id, or what an account
/// runs outside every login.
///
/// Doubles as the filter of the process list: the scope control offers one entry per row, and
/// `App::rebuild` keeps the processes this names.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Whose {
    Login(u32),
    Outside(u32),
}

impl Whose {
    /// Whether a process belongs to what this names.
    ///
    /// `logins` is the set of audit ids that logind still calls a login; a process whose
    /// session is not among them counts as outside, wherever it came from.
    #[must_use]
    pub fn holds(self, p: &Process, logins: &[Session]) -> bool {
        let login = p
            .session
            .filter(|id| logins.iter().any(|session| session.audit == *id));
        match self {
            Self::Login(audit) => login == Some(audit),
            Self::Outside(uid) => login.is_none() && p.uid == uid,
        }
    }
}

/// One row per login, heaviest first.
///
/// A row is one login, not one account: five ssh connections under the same name are five
/// people at five terminals. What a process belongs to is its audit session id, `p.session`,
/// matched against the logins logind knows.
///
/// A login holds everything running inside it, whoever owns it. The session leader of an ssh
/// login is `sshd` and belongs to root; of a graphical one, whatever started the session.
/// Splitting those out by account number would put a root row named after somebody else
/// beside every login on the page.
///
/// Processes outside every login - the user's service manager and everything under it,
/// pipewire, the portals - are folded into one background row per account. That row is kept
/// to real accounts, `current` aside: root owns the whole system outside any login, and a
/// "background services" row holding every daemon on the machine would say nothing true.
/// A login, on the other hand, shows whatever its account number is - somebody who has
/// logged in is a person, root over ssh included.
#[must_use]
pub fn rows<'a>(
    processes: impl Iterator<Item = &'a Process>,
    sessions: &[Session],
    names: &Names,
    range: Range,
    current: u32,
) -> Vec<Row> {
    struct Fold {
        pids: Vec<u32>,
        cpu: f32,
        rss: u64,
        data: u64,
        threads: u32,
        disk: Option<[f64; 2]>,
    }

    let logins: HashMap<u32, &Session> = sessions
        .iter()
        .map(|session| (session.audit, session))
        .collect();

    let mut by_key: HashMap<Whose, Fold> = HashMap::new();
    for p in processes {
        let key = match p.session.filter(|id| logins.contains_key(id)) {
            Some(audit) => Whose::Login(audit),
            None if range.holds(p.uid) || p.uid == current => Whose::Outside(p.uid),
            None => continue,
        };
        let fold = by_key.entry(key).or_insert_with(|| Fold {
            pids: Vec::new(),
            cpu: 0.0,
            rss: 0,
            data: 0,
            threads: 0,
            disk: None,
        });
        fold.pids.push(p.pid);
        fold.cpu += p.cpu;
        fold.rss += p.rss;
        fold.data += p.data;
        fold.threads += p.threads;
        if let Some([r, w]) = p.disk {
            let acc = fold.disk.get_or_insert([0.0, 0.0]);
            acc[0] += r;
            acc[1] += w;
        }
    }

    // Nothing could be tied to a login: no logind, or a kernel built without audit, which
    // leaves /proc/PID/sessionid empty for everything. The page then reads as it did before
    // logins were told apart - a row per account - and says so with a dash rather than
    // calling a person's whole desktop a background service.
    let anything_signed_in = by_key.keys().any(|key| matches!(key, Whose::Login(_)));
    let outside = if anything_signed_in {
        fl!("session-background")
    } else {
        fl!("no-value")
    };

    // Two logins of one account that read the same - two ssh connections from one address -
    // are told apart by their terminal, and by the session number when they have none.
    let mut twins: HashMap<(u32, String), usize> = HashMap::new();
    for key in by_key.keys() {
        if let Whose::Login(audit) = key
            && let Some(session) = logins.get(audit)
        {
            *twins.entry((session.uid, session.label())).or_default() += 1;
        }
    }

    let mut out: Vec<Row> = by_key
        .into_iter()
        .map(|(key, f)| {
            let (name, label) = match key {
                Whose::Login(audit) => {
                    let session = logins[&audit];
                    let label = session.label();
                    let label = if twins
                        .get(&(session.uid, label.clone()))
                        .is_some_and(|n| *n > 1)
                    {
                        fl!("session-node", what = label, node = session.node())
                    } else {
                        label
                    };
                    // logind knows the name of an account that /etc/passwd has never heard
                    // of, which is the ordinary case for a network directory.
                    let name = if session.user.is_empty() {
                        names.get(session.uid)
                    } else {
                        session.user.clone()
                    };
                    (name, label)
                }
                Whose::Outside(uid) => (names.get(uid), outside.clone()),
            };
            Row::user(
                name, label, key, f.pids, f.cpu, f.rss, f.data, f.threads, f.disk,
            )
        })
        .collect();
    out.sort_by(|a, b| crate::processes::compare(a, b, Column::Data));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Probe;
    use crate::sessions::Kind;

    fn login(audit: u32, uid: u32, kind: Kind, tty: &str) -> Session {
        Session {
            audit,
            uid,
            user: String::new(),
            kind,
            tty: tty.to_owned(),
        }
    }

    fn ssh(audit: u32, uid: u32, host: &str, tty: &str) -> Session {
        login(
            audit,
            uid,
            Kind::Remote {
                service: "sshd".into(),
                host: host.to_owned(),
            },
            tty,
        )
    }

    fn process(pid: u32, uid: u32, session: Option<u32>) -> Process {
        let mut p = Process::stub(pid, 1, "bash");
        p.uid = uid;
        p.session = session;
        p.data = 1024;
        p
    }

    #[test]
    fn the_running_system_has_the_current_user_in_it() {
        let mut probe = Probe::new();
        probe.sample();
        let names = Names::load();
        let me = rustix::process::getuid().as_raw();
        let rows = rows(probe.processes(), &[], &names, Range::load(), me);
        assert!(
            !rows.is_empty(),
            "a running system has at least one account"
        );
        let total: u64 = rows.iter().map(|r| r.sort_data).sum();
        assert!(total > 0, "the accounts hold some memory between them");
    }

    /// root owns more processes than anyone on a desktop, and none of them belong on a page
    /// about the people using the machine.
    #[test]
    fn system_accounts_are_left_out() {
        let mut probe = Probe::new();
        probe.sample();
        let names = Names::load();
        let me = rustix::process::getuid().as_raw();
        let rows = rows(probe.processes(), &[], &names, Range::load(), me);
        assert!(
            !rows.iter().any(|r| r.label().starts_with("root ")),
            "root is on the users page"
        );
    }

    #[test]
    fn the_boundary_comes_from_login_defs() {
        let range = Range {
            min: 1000,
            max: 60000,
        };
        assert!(!range.holds(0), "root");
        assert!(!range.holds(999), "a system account");
        assert!(range.holds(1000), "the first regular account");
        assert!(!range.holds(65534), "nobody sits above UID_MAX");
    }

    #[test]
    fn an_unknown_account_reads_as_its_number() {
        let names = Names(HashMap::new());
        assert_eq!(names.get(4242), "4242");
    }

    /// The whole point: one name, five connections, five rows - and the desktop it was
    /// already showing is a sixth.
    #[test]
    fn every_login_of_one_account_is_its_own_row() {
        let sessions: Vec<Session> = std::iter::once(login(10, 1000, Kind::Desktop, "tty1"))
            .chain((0..5).map(|n| ssh(20 + n, 1000, "172.17.6.2", &format!("pts/{n}"))))
            .collect();
        let procs: Vec<Process> = std::iter::once(process(100, 1000, Some(10)))
            .chain((0..5).map(|n| process(200 + n, 1000, Some(20 + n))))
            .collect();

        let rows = rows(
            procs.iter(),
            &sessions,
            &Names(HashMap::new()),
            Range::default(),
            1000,
        );
        assert_eq!(rows.len(), 6, "one desktop and five ssh");
    }

    /// Everything outside the logins is one row, not one per service, and it never swallows
    /// what a person is running.
    #[test]
    fn what_runs_outside_every_login_is_one_background_row() {
        let sessions = vec![login(10, 1000, Kind::Desktop, "tty1")];
        let procs = [
            process(100, 1000, Some(10)),
            process(101, 1000, Some(10)),
            // The user manager and its children: a session id of their own, or none at all.
            process(200, 1000, Some(4)),
            process(201, 1000, None),
        ];

        let rows = rows(
            procs.iter(),
            &sessions,
            &Names(HashMap::new()),
            Range::default(),
            1000,
        );
        assert_eq!(rows.len(), 2, "the desktop and the background");
        let counts: Vec<usize> = rows.iter().map(|r| r.pids.len()).collect();
        assert!(counts.contains(&2) && counts.iter().sum::<usize>() == 4);
    }

    /// The leader of an ssh login is `sshd` and belongs to root; of a graphical one, whatever
    /// started the session. Both belong to the login they lead, and the row is named after the
    /// account that logged in.
    #[test]
    fn a_login_holds_what_runs_inside_it_whoever_owns_it() {
        let sessions = [ssh(20, 1000, "172.17.6.2", "pts/1")];
        let procs = [
            // sshd, and the shell it started.
            process(100, 0, Some(20)),
            process(101, 1000, Some(20)),
        ];

        let rows = rows(
            procs.iter(),
            &sessions,
            &Names(HashMap::new()),
            Range::default(),
            1000,
        );
        assert_eq!(rows.len(), 1, "one login, one row");
        assert_eq!(rows[0].pids.len(), 2);
        assert_eq!(
            rows[0].label(),
            "1000 (2)",
            "the row is named after the account that logged in, not after root"
        );
    }

    /// The filter of the process list is the row of this page, and it keeps what runs inside
    /// the login - `sshd` from root included - and nothing else.
    #[test]
    fn a_login_filter_keeps_what_runs_inside_it() {
        let sessions = [ssh(20, 1000, "172.17.6.2", "pts/1")];
        let inside_sshd = process(100, 0, Some(20));
        let inside_shell = process(101, 1000, Some(20));
        let other_login = process(102, 1000, Some(21));
        let outside = process(103, 1000, None);

        let login = Whose::Login(20);
        assert!(login.holds(&inside_sshd, &sessions));
        assert!(login.holds(&inside_shell, &sessions));
        assert!(!login.holds(&other_login, &sessions), "another login");
        assert!(!login.holds(&outside, &sessions), "outside every login");

        // Outside is the other half of the same split: what the login row does not hold, the
        // background row of that account does.
        let background = Whose::Outside(1000);
        assert!(background.holds(&outside, &sessions));
        assert!(!background.holds(&inside_shell, &sessions));
        // A session logind no longer lists is not a login, so what carried it counts as
        // outside - the same rule the page folds by.
        assert!(background.holds(&other_login, &sessions));
    }

    /// The scope control offers the rows of this page, named the way the page names them.
    #[test]
    fn a_choice_reads_like_the_row_it_came_from() {
        let sessions = [ssh(20, 1000, "172.17.6.2", "pts/1")];
        let procs = [process(100, 1000, Some(20))];

        let rows = rows(
            procs.iter(),
            &sessions,
            &Names(HashMap::new()),
            Range::default(),
            1000,
        );
        let choice = rows[0].choice();
        assert!(choice.contains("1000"), "the account: {choice}");
        assert!(choice.contains("172.17.6.2"), "the login: {choice}");
        assert_eq!(rows[0].whose(), Some(Whose::Login(20)));
    }

    /// A daemon of a system account is not a person; a system account over ssh is.
    #[test]
    fn a_system_account_shows_only_when_it_has_logged_in() {
        let sessions = vec![ssh(7, 0, "172.17.6.2", "pts/0")];
        let procs = [
            process(10, 0, Some(7)),
            // Every other process root owns: daemons, kernel threads.
            process(11, 0, None),
            process(12, 0, None),
        ];

        let rows = rows(
            procs.iter(),
            &sessions,
            &Names(HashMap::new()),
            Range::default(),
            1000,
        );
        assert_eq!(rows.len(), 1, "the ssh login and nothing else");
        assert_eq!(rows[0].pids, vec![10]);
    }

    #[test]
    fn two_logins_from_one_address_are_told_apart() {
        let sessions = vec![
            ssh(20, 1000, "172.17.6.2", "pts/1"),
            ssh(21, 1000, "172.17.6.2", "pts/2"),
        ];
        let procs = [process(100, 1000, Some(20)), process(101, 1000, Some(21))];

        let paired = rows(
            procs.iter(),
            &sessions,
            &Names(HashMap::new()),
            Range::default(),
            1000,
        );
        let labels: Vec<&str> = paired.iter().map(Row::session_label).collect();
        assert!(labels[0] != labels[1], "{labels:?}");
        assert!(
            labels.iter().all(|l| l.contains("pts/")),
            "the terminal is what tells them apart: {labels:?}"
        );

        // The same pair without terminals - ssh without a pseudo terminal - falls back to the
        // session number.
        let bare = vec![
            ssh(20, 1000, "172.17.6.2", ""),
            ssh(21, 1000, "172.17.6.2", ""),
        ];
        let bare = rows(
            procs.iter(),
            &bare,
            &Names(HashMap::new()),
            Range::default(),
            1000,
        );
        let labels: Vec<&str> = bare.iter().map(Row::session_label).collect();
        assert!(labels.iter().any(|l| l.contains("#20")), "{labels:?}");
        assert!(labels.iter().any(|l| l.contains("#21")), "{labels:?}");
    }

    /// No logind, or a kernel without audit: the page reads as it did before, a row per
    /// account, and the session column says nothing rather than something wrong.
    #[test]
    fn without_any_login_the_page_is_one_row_per_account() {
        let procs = [
            process(100, 1000, None),
            process(101, 1000, None),
            process(102, 1001, None),
        ];

        let rows = rows(
            procs.iter(),
            &[],
            &Names(HashMap::new()),
            Range::default(),
            1000,
        );
        assert_eq!(rows.len(), 2, "two accounts");
        let dash = fl!("no-value");
        assert!(rows.iter().all(|r| r.session_label() == dash));
    }
}
