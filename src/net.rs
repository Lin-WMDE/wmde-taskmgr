// SPDX-License-Identifier: GPL-3.0-only

//! What a network card is doing beyond its rate, and where it sits on the network.
//!
//! Read once a tick and only for the card on screen: the counters and link state from
//! `/sys/class/net/<if>`, its addresses from `getifaddrs`, its default gateways from
//! `/proc/net/route` and `/proc/net/ipv6_route`. Nothing here needs root or starts a process.

use std::ffi::CStr;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

const SYS_NET: &str = "/sys/class/net";
const PROC_NET: &str = "/proc/net";

/// Addresses with their prefix lengths.
pub type Ipv4List = Vec<(Ipv4Addr, u8)>;
pub type Ipv6List = Vec<(Ipv6Addr, u8)>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Duplex {
    Full,
    Half,
}

/// One network card, as of the last tick. Every field the kernel did not answer stays empty.
#[derive(Debug, Default)]
pub struct NetFacts {
    /// `operstate` says the link is down. A card with no operstate - or `unknown`, which
    /// virtual links report while working - counts as connected.
    pub down: bool,
    pub mac: Option<String>,
    pub mtu: Option<u32>,
    /// Megabits per second the link negotiated; read every tick, as a link renegotiates.
    pub speed_mbps: Option<u32>,
    pub duplex: Option<Duplex>,
    pub driver: Option<String>,
    /// Bytes received and sent since the card came up.
    pub bytes: Option<(u64, u64)>,
    /// Received and sent, in that order, as in every pair on the page.
    pub packets: Option<(u64, u64)>,
    pub errors: Option<(u64, u64)>,
    pub dropped: Option<(u64, u64)>,
    /// IPv6 global addresses come before link-local ones.
    pub ipv4: Ipv4List,
    pub ipv6: Ipv6List,
    pub gateway4: Option<Ipv4Addr>,
    pub gateway6: Option<Ipv6Addr>,
}

impl NetFacts {
    pub fn read(iface: &str) -> Self {
        let mut facts = Self::read_at(Path::new(SYS_NET), Path::new(PROC_NET), iface);
        let (ipv4, ipv6) = addresses(iface);
        facts.ipv4 = ipv4;
        facts.ipv6 = ipv6;
        facts
    }

    /// Everything but the addresses, under a `/sys/class/net` and a `/proc/net` that a test
    /// can stand in for. `getifaddrs` has no such root, so the addresses are read apart.
    fn read_at(sys: &Path, proc_net: &Path, iface: &str) -> Self {
        let dir = sys.join(iface);
        let text = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        };
        let number = |name: &str| text(name).and_then(|s| s.parse::<u64>().ok());
        let pair = |rx: &str, tx: &str| {
            number(&format!("statistics/{rx}")).zip(number(&format!("statistics/{tx}")))
        };

        let route = std::fs::read_to_string(proc_net.join("route")).unwrap_or_default();
        let route6 = std::fs::read_to_string(proc_net.join("ipv6_route")).unwrap_or_default();

        Self {
            down: text("operstate").is_some_and(|state| {
                matches!(state.as_str(), "down" | "lowerlayerdown" | "notpresent")
            }),
            mac: text("address").filter(|mac| mac != "00:00:00:00:00:00"),
            mtu: number("mtu").and_then(|mtu| u32::try_from(mtu).ok()),
            // `-1` or an error while the link is down; neither parses as unsigned.
            speed_mbps: number("speed")
                .and_then(|speed| u32::try_from(speed).ok())
                .filter(|speed| *speed > 0),
            duplex: match text("duplex").as_deref() {
                Some("full") => Some(Duplex::Full),
                Some("half") => Some(Duplex::Half),
                _ => None,
            },
            driver: std::fs::read_link(dir.join("device/driver"))
                .ok()
                .and_then(|target| Some(target.file_name()?.to_str()?.to_owned())),
            bytes: pair("rx_bytes", "tx_bytes"),
            packets: pair("rx_packets", "tx_packets"),
            errors: pair("rx_errors", "tx_errors"),
            dropped: pair("rx_dropped", "tx_dropped"),
            ipv4: Vec::new(),
            ipv6: Vec::new(),
            gateway4: gateway4(&route, iface),
            gateway6: gateway6(&route6, iface),
        }
    }
}

/// The IPv4 default route of `iface` with the lowest metric, from `/proc/net/route`.
///
/// The table writes addresses as the hex of a native-endian `u32`: `FA0611AC` is
/// 172.17.6.250 on a little-endian machine.
fn gateway4(table: &str, iface: &str) -> Option<Ipv4Addr> {
    const RTF_GATEWAY: u32 = 0x2;
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [name, destination, gateway, flags, _, _, metric, mask, ..] = fields[..] else {
                return None;
            };
            let flags = u32::from_str_radix(flags, 16).ok()?;
            if name != iface || destination != "00000000" || mask != "00000000" {
                return None;
            }
            if flags & RTF_GATEWAY == 0 {
                return None;
            }
            let gateway = u32::from_str_radix(gateway, 16).ok()?;
            Some((
                metric.parse::<u32>().ok()?,
                Ipv4Addr::from(gateway.to_ne_bytes()),
            ))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, gateway)| gateway)
}

/// The IPv6 default route of `iface` with the lowest metric, from `/proc/net/ipv6_route`:
/// destination `::/0` and a next hop that is not `::`. Addresses there are plain big-endian
/// hex.
fn gateway6(table: &str, iface: &str) -> Option<Ipv6Addr> {
    table
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [destination, prefix, _, _, next_hop, metric, _, _, _, name] = fields[..] else {
                return None;
            };
            if name != iface || u128::from_str_radix(destination, 16).ok()? != 0 || prefix != "00" {
                return None;
            }
            let next_hop = u128::from_str_radix(next_hop, 16).ok()?;
            if next_hop == 0 {
                return None;
            }
            Some((
                u32::from_str_radix(metric, 16).ok()?,
                Ipv6Addr::from(next_hop),
            ))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, gateway)| gateway)
}

/// Prefix length of a netmask: the count of its set bits.
fn prefix_len(mask: &[u8]) -> u8 {
    mask.iter().map(|byte| byte.count_ones() as u8).sum()
}

/// `fe80::/10`.
fn is_link_local(address: &Ipv6Addr) -> bool {
    address.segments()[0] & 0xffc0 == 0xfe80
}

/// Global addresses first: they are what the machine is reached by, and a link-local one
/// every card has.
fn order_ipv6(addresses: &mut [(Ipv6Addr, u8)]) {
    addresses.sort_by_key(|(address, _)| is_link_local(address));
}

/// The card's addresses, through `getifaddrs`.
fn addresses(iface: &str) -> (Ipv4List, Ipv6List) {
    let mut ipv4 = Vec::new();
    let mut ipv6 = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` fills `head` with a list it owns until `freeifaddrs`.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        tracing::warn!(
            err = %std::io::Error::last_os_error(),
            "getifaddrs failed; no addresses shown"
        );
        return (ipv4, ipv6);
    }
    let mut at = head;
    while !at.is_null() {
        // SAFETY: a node of the list `getifaddrs` returned, not yet freed.
        let entry = unsafe { &*at };
        at = entry.ifa_next;
        if entry.ifa_addr.is_null() || entry.ifa_name.is_null() {
            continue;
        }
        // SAFETY: a NUL-terminated name the list owns.
        if unsafe { CStr::from_ptr(entry.ifa_name) }.to_bytes() != iface.as_bytes() {
            continue;
        }
        // SAFETY: every `sockaddr` starts with its family, and the family says which one
        // it is; the netmask, when there is one, is of the same family.
        unsafe {
            match i32::from((*entry.ifa_addr).sa_family) {
                libc::AF_INET => {
                    let address = &*entry.ifa_addr.cast::<libc::sockaddr_in>();
                    let prefix = if entry.ifa_netmask.is_null() {
                        32
                    } else {
                        let mask = &*entry.ifa_netmask.cast::<libc::sockaddr_in>();
                        prefix_len(&mask.sin_addr.s_addr.to_ne_bytes())
                    };
                    ipv4.push((
                        Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()),
                        prefix,
                    ));
                }
                libc::AF_INET6 => {
                    let address = &*entry.ifa_addr.cast::<libc::sockaddr_in6>();
                    let prefix = if entry.ifa_netmask.is_null() {
                        128
                    } else {
                        let mask = &*entry.ifa_netmask.cast::<libc::sockaddr_in6>();
                        prefix_len(&mask.sin6_addr.s6_addr)
                    };
                    ipv6.push((Ipv6Addr::from(address.sin6_addr.s6_addr), prefix));
                }
                _ => {}
            }
        }
    }
    // SAFETY: the list `getifaddrs` returned, freed once and not touched after.
    unsafe { libc::freeifaddrs(head) };
    order_ipv6(&mut ipv6);
    (ipv4, ipv6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    struct Root(PathBuf);

    impl Root {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("wmde-taskmgr-net-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("sys/enp4s0/statistics")).unwrap();
            fs::create_dir_all(dir.join("proc")).unwrap();
            Self(dir)
        }

        fn write(&self, path: &str, text: &str) {
            fs::write(self.0.join(path), text).unwrap();
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Read off `.131`: `enp4s0` on `alx`, at a gigabit, up.
    #[test]
    fn a_card_reads_its_link_and_counters() {
        let root = Root::new("card");
        for (file, text) in [
            ("sys/enp4s0/operstate", "up\n"),
            ("sys/enp4s0/address", "d8:cb:8a:5a:d4:81\n"),
            ("sys/enp4s0/mtu", "1500\n"),
            ("sys/enp4s0/speed", "1000\n"),
            ("sys/enp4s0/duplex", "full\n"),
            ("sys/enp4s0/statistics/rx_bytes", "3686395543\n"),
            ("sys/enp4s0/statistics/tx_bytes", "36297507\n"),
            ("sys/enp4s0/statistics/rx_packets", "2923517\n"),
            ("sys/enp4s0/statistics/tx_packets", "331271\n"),
            ("sys/enp4s0/statistics/rx_errors", "0\n"),
            ("sys/enp4s0/statistics/tx_errors", "0\n"),
            ("sys/enp4s0/statistics/rx_dropped", "405\n"),
            ("sys/enp4s0/statistics/tx_dropped", "0\n"),
        ] {
            root.write(file, text);
        }
        fs::create_dir_all(root.0.join("drivers/alx")).unwrap();
        fs::create_dir_all(root.0.join("sys/enp4s0/device")).unwrap();
        symlink(
            root.0.join("drivers/alx"),
            root.0.join("sys/enp4s0/device/driver"),
        )
        .unwrap();

        let facts = NetFacts::read_at(&root.0.join("sys"), &root.0.join("proc"), "enp4s0");
        assert!(!facts.down);
        assert_eq!(facts.mac.as_deref(), Some("d8:cb:8a:5a:d4:81"));
        assert_eq!(facts.mtu, Some(1500));
        assert_eq!(facts.speed_mbps, Some(1000));
        assert_eq!(facts.duplex, Some(Duplex::Full));
        assert_eq!(facts.driver.as_deref(), Some("alx"));
        assert_eq!(facts.bytes, Some((3_686_395_543, 36_297_507)));
        assert_eq!(facts.packets, Some((2_923_517, 331_271)));
        assert_eq!(facts.errors, Some((0, 0)));
        assert_eq!(facts.dropped, Some((405, 0)));
    }

    /// A card with its cable out: down, and no speed where the kernel writes `-1`.
    #[test]
    fn a_card_without_a_link_is_down_and_has_no_speed() {
        let root = Root::new("down");
        root.write("sys/enp4s0/operstate", "down\n");
        root.write("sys/enp4s0/speed", "-1\n");
        let facts = NetFacts::read_at(&root.0.join("sys"), &root.0.join("proc"), "enp4s0");
        assert!(facts.down);
        assert_eq!(facts.speed_mbps, None);
        assert_eq!(facts.duplex, None);
    }

    #[test]
    fn the_ipv4_gateway_is_the_default_route_of_the_card() {
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                     enp4s0\t00000000\tFA0611AC\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                     enp4s0\t000611AC\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
                     wlan0\t00000000\t0100A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n";
        assert_eq!(
            gateway4(table, "enp4s0"),
            Some(Ipv4Addr::new(172, 17, 6, 250))
        );
        assert_eq!(
            gateway4(table, "wlan0"),
            Some(Ipv4Addr::new(192, 168, 0, 1))
        );
        assert_eq!(gateway4(table, "eth9"), None);
    }

    #[test]
    fn the_ipv6_gateway_is_the_default_route_of_the_card() {
        let table = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 \
                     fe80000000000000020c29fffe123456 00000400 00000001 00000000 00000003 enp4s0\n\
                     fe800000000000000000000000000000 40 00000000000000000000000000000000 00 \
                     00000000000000000000000000000000 00000100 00000001 00000000 00000001 enp4s0\n";
        assert_eq!(
            gateway6(table, "enp4s0"),
            Some("fe80::20c:29ff:fe12:3456".parse().unwrap())
        );
        assert_eq!(gateway6(table, "wlan0"), None);
    }

    #[test]
    fn a_netmask_is_a_prefix_length() {
        assert_eq!(prefix_len(&[255, 255, 255, 0]), 24);
        assert_eq!(prefix_len(&[255, 255, 240, 0]), 20);
        assert_eq!(prefix_len(&[0; 4]), 0);
        let mut v6 = [0_u8; 16];
        v6[..8].fill(255);
        assert_eq!(prefix_len(&v6), 64);
    }

    #[test]
    fn global_ipv6_addresses_come_before_link_local() {
        let mut addresses = vec![
            ("fe80::4306:38db:2125:df30".parse().unwrap(), 64),
            ("2a02:2378::1".parse().unwrap(), 64),
        ];
        order_ipv6(&mut addresses);
        assert_eq!(addresses[0].0, "2a02:2378::1".parse::<Ipv6Addr>().unwrap());
        assert!(is_link_local(&addresses[1].0));
    }
}
