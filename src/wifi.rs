// SPDX-License-Identifier: GPL-3.0-only

//! A wireless card's connection, asked of NetworkManager over the system bus.
//!
//! The kernel's `/sys/class/net` knows nothing of the network a card has joined. The network
//! name, the signal and the frequency come from NetworkManager's access point object; the
//! standard (802.11ac, ax) it does not report, and the only other sources - nl80211 over
//! netlink, `iw` as a process - are not used.

use std::sync::OnceLock;

use zbus::zvariant::OwnedObjectPath;

const NM_NAME: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Band {
    /// 2.4 GHz.
    TwoFour,
    Five,
    Six,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WifiInfo {
    pub ssid: String,
    /// Signal strength, 0 to 100, as NetworkManager scales it.
    pub strength: u8,
    pub band: Option<Band>,
    /// The rate the card is connected at, in megabits per second.
    pub bitrate_mbps: Option<u32>,
}

/// The band a channel frequency falls in. The 5 GHz channels end at 5885 MHz and the 6 GHz
/// ones start at 5935.
pub fn band(frequency_mhz: u32) -> Option<Band> {
    match frequency_mhz {
        2400..=2500 => Some(Band::TwoFour),
        4900..=5900 => Some(Band::Five),
        5925..=7125 => Some(Band::Six),
        _ => None,
    }
}

/// A network name as NetworkManager hands it over: bytes, with no promise of UTF-8.
fn ssid(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// What NetworkManager says about the connection of `iface`. `None` when it is not running,
/// does not manage the card, or the card has joined nothing.
pub async fn query(iface: String) -> Option<WifiInfo> {
    let connection = connection().await?;
    let nm = zbus::Proxy::new(connection, NM_NAME, NM_PATH, NM_NAME)
        .await
        .ok()?;
    let device: OwnedObjectPath = nm
        .call("GetDeviceByIpIface", &(iface.as_str(),))
        .await
        .ok()?;

    let wireless = uncached(connection, device, WIRELESS).await?;
    let point: OwnedObjectPath = wireless.get_property("ActiveAccessPoint").await.ok()?;
    if point.as_str() == "/" {
        return None;
    }
    let bitrate_kbps: u32 = wireless.get_property("Bitrate").await.unwrap_or(0);

    let point = uncached(connection, point, ACCESS_POINT).await?;
    let name: Vec<u8> = point.get_property("Ssid").await.ok()?;
    let strength: u8 = point.get_property("Strength").await.unwrap_or(0);
    let frequency: u32 = point.get_property("Frequency").await.unwrap_or(0);

    Some(WifiInfo {
        ssid: ssid(&name),
        strength: strength.min(100),
        band: band(frequency),
        bitrate_mbps: (bitrate_kbps > 0).then_some(bitrate_kbps / 1000),
    })
}

/// One system bus connection for the life of the window: a query every other tick would
/// otherwise open and close a socket each time.
async fn connection() -> Option<&'static zbus::Connection> {
    static CONNECTION: OnceLock<zbus::Connection> = OnceLock::new();
    if let Some(connection) = CONNECTION.get() {
        return Some(connection);
    }
    match zbus::Connection::system().await {
        Ok(connection) => Some(CONNECTION.get_or_init(|| connection)),
        Err(err) => {
            tracing::info!("no system bus, no Wi-Fi details: {err}");
            None
        }
    }
}

/// A proxy that asks every time: a cached property would only be as fresh as the last
/// change signal, and signal strength changes all the time.
async fn uncached(
    connection: &zbus::Connection,
    path: OwnedObjectPath,
    interface: &'static str,
) -> Option<zbus::Proxy<'static>> {
    zbus::proxy::Builder::new(connection)
        .destination(NM_NAME)
        .ok()?
        .path(path)
        .ok()?
        .interface(interface)
        .ok()?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frequency_falls_in_its_band() {
        assert_eq!(band(2412), Some(Band::TwoFour));
        assert_eq!(band(2484), Some(Band::TwoFour));
        assert_eq!(band(5180), Some(Band::Five));
        assert_eq!(band(5885), Some(Band::Five));
        assert_eq!(band(5955), Some(Band::Six));
        assert_eq!(band(0), None);
    }

    /// A network name is bytes: one that is not UTF-8 still reads, with the bad bytes marked.
    #[test]
    fn a_network_name_need_not_be_utf8() {
        assert_eq!(ssid(b"Home"), "Home");
        assert_eq!(ssid("Дім".as_bytes()), "Дім");
        assert_eq!(ssid(&[0x48, 0xff, 0x69]), "H\u{fffd}i");
    }
}
