//! Wi-Fi signal details from CoreWLAN. macOS treats the network name as location data, so `ssid`
//! stays `None` unless the signed app bundle has Location permission.

use objc2_core_wlan::CWWiFiClient;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Wifi {
    pub interface: Option<String>,
    pub ssid: Option<String>,
    pub rssi_dbm: i64,
    pub noise_dbm: i64,
    pub transmit_mbps: f64,
    pub channel: Option<i64>,
}

/// Signal minus noise, in dB.
pub fn snr(rssi_dbm: i64, noise_dbm: i64) -> i64 {
    rssi_dbm - noise_dbm
}

pub fn quality_label(snr_db: i64) -> &'static str {
    match snr_db {
        i64::MIN..=14 => "Poor",
        15..=24 => "Fair",
        25..=39 => "Good",
        _ => "Excellent",
    }
}

/// `with_name` asks for the network name too. That's slower, since macOS checks Location
/// permission each time, so it's skipped unless the name is shown.
pub fn read(with_name: bool) -> Option<Wifi> {
    // SAFETY: documented CoreWLAN getters on objects retained for the duration of the calls.
    unsafe {
        let client = CWWiFiClient::sharedWiFiClient();
        let interface = client.interface()?;
        let rssi_dbm = interface.rssiValue() as i64;
        // An interface that's off or unassociated reports zero signal.
        if rssi_dbm == 0 {
            return None;
        }
        Some(Wifi {
            interface: interface.interfaceName().map(|n| n.to_string()),
            ssid: if with_name { interface.ssid().map(|s| s.to_string()) } else { None },
            rssi_dbm,
            noise_dbm: interface.noiseMeasurement() as i64,
            transmit_mbps: interface.transmitRate(),
            channel: interface.wlanChannel().map(|c| c.channelNumber() as i64),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_quality_comes_from_snr() {
        assert_eq!(snr(-43, -90), 47);
        assert_eq!(quality_label(47), "Excellent");
        assert_eq!(quality_label(30), "Good");
        assert_eq!(quality_label(20), "Fair");
        assert_eq!(quality_label(5), "Poor");
    }
}
