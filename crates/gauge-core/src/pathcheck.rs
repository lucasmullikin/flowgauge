//! Checks each step between this Mac and the internet: the router, the DNS server macOS uses,
//! and an address you choose. The internet step only runs once you choose an address.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV6, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

const WINDOW: usize = 30;
const MIN_FOR_USUAL: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Good,
    Slow,
    Down,
    /// Not checked, because no address is set or the step doesn't apply.
    Off,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub latest_ms: Option<f32>,
    pub usual_ms: Option<f32>,
    pub loss_percent: f32,
    pub health: Health,
}

impl Step {
    pub fn off() -> Step {
        Step { latest_ms: None, usual_ms: None, loss_percent: 0.0, health: Health::Off }
    }
}

/// Recent results for one step, to tell slow from usual and count lost replies.
#[derive(Debug, Default, Clone)]
pub struct Tracker {
    results: VecDeque<Option<f32>>,
}

impl Tracker {
    pub fn push(&mut self, result: Option<f32>) {
        self.results.push_back(result);
        while self.results.len() > WINDOW {
            self.results.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.results.clear();
    }

    fn usual(&self) -> Option<f32> {
        let mut times: Vec<f32> = self.results.iter().flatten().copied().collect();
        if times.len() < MIN_FOR_USUAL {
            return None;
        }
        times.sort_by(f32::total_cmp);
        Some(times[times.len() / 2])
    }

    /// `slow_ms` is the time that counts as slow whatever is usual.
    pub fn step(&self, slow_ms: f32) -> Step {
        let Some(latest) = self.results.back() else { return Step::off() };
        let lost = self.results.iter().filter(|r| r.is_none()).count();
        let loss_percent = lost as f32 * 100.0 / self.results.len() as f32;
        let usual = self.usual();
        let health = match latest {
            None => Health::Down,
            Some(ms) => {
                let above_usual = usual.is_some_and(|u| *ms > u * 2.0 + 20.0);
                if *ms > slow_ms || above_usual || loss_percent >= 10.0 { Health::Slow } else { Health::Good }
            }
        };
        Step { latest_ms: *latest, usual_ms: usual, loss_percent, health }
    }
}

/// The first `nameserver` in resolv.conf, which macOS keeps in step with its DNS settings.
/// A link-local IPv6 server keeps its `%en0` scope, which is needed to reach it.
pub fn parse_resolver(resolv_conf: &str) -> Option<String> {
    resolv_conf
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .map(str::trim)
        .find(|server| server_address(server).is_some())
        .map(str::to_string)
}

pub fn system_resolver() -> Option<String> {
    parse_resolver(&std::fs::read_to_string("/etc/resolv.conf").ok()?)
}

/// Where to send a query: the address, plus the interface for a scoped IPv6 address.
pub fn server_address(server: &str) -> Option<SocketAddr> {
    let (address, scope) = match server.split_once('%') {
        Some((address, scope)) => (address, Some(scope)),
        None => (server, None),
    };
    match address.parse::<IpAddr>().ok()? {
        IpAddr::V4(v4) => Some(SocketAddr::new(IpAddr::V4(v4), 53)),
        IpAddr::V6(v6) => {
            let index = scope.map_or(0, |name| {
                let name = std::ffi::CString::new(name).unwrap_or_default();
                // SAFETY: the name is NUL-terminated and only read.
                unsafe { libc::if_nametoindex(name.as_ptr()) }
            });
            Some(SocketAddr::V6(SocketAddrV6::new(v6, 53, 0, index)))
        }
    }
}

/// A standard query for the A record of `name`. An empty name or `.` asks about the root,
/// which every DNS server can answer without telling it anything about you.
pub fn build_query(id: u16, name: &str) -> Option<Vec<u8>> {
    let mut packet = Vec::with_capacity(32 + name.len());
    packet.extend(id.to_be_bytes());
    packet.extend([0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    let name = name.trim_end_matches('.');
    for label in name.split('.').filter(|_| !name.is_empty()) {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        packet.push(label.len() as u8);
        packet.extend(label.as_bytes());
    }
    packet.extend([0, 0, 1, 0, 1]);
    Some(packet)
}

/// Milliseconds for a DNS server to answer, skipping any cache on this Mac.
pub fn time_dns(server: &str, name: &str, id: u16, timeout: Duration) -> Option<f32> {
    let query = build_query(id, name)?;
    let target = server_address(server)?;
    let local: SocketAddr = if target.is_ipv4() { "0.0.0.0:0".parse().ok()? } else { "[::]:0".parse().ok()? };
    let socket = UdpSocket::bind(local).ok()?;
    socket.set_read_timeout(Some(timeout)).ok()?;
    let started = Instant::now();
    socket.send_to(&query, target).ok()?;
    let mut buffer = [0u8; 512];
    while started.elapsed() < timeout {
        let (n, _) = socket.recv_from(&mut buffer).ok()?;
        if n >= 2 && buffer[..2] == id.to_be_bytes() {
            return Some(started.elapsed().as_secs_f32() * 1000.0);
        }
    }
    None
}

/// The address to ping, and the name to look up if the target is a name.
pub fn resolve_target(target: &str) -> (Option<Ipv4Addr>, Option<String>) {
    let target = target.trim();
    if target.is_empty() {
        return (None, None);
    }
    if let Ok(address) = target.parse::<Ipv4Addr>() {
        return (Some(address), None);
    }
    let address = (target, 0).to_socket_addrs().ok().and_then(|mut all| {
        all.find_map(|a| match a.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
    });
    (address, Some(target.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_is_slow_against_its_usual_time_or_a_fixed_limit() {
        let mut tracker = Tracker::default();
        for _ in 0..10 {
            tracker.push(Some(10.0));
        }
        assert_eq!(tracker.step(100.0).health, Health::Good);
        tracker.push(Some(60.0));
        let step = tracker.step(100.0);
        assert_eq!(step.health, Health::Slow);
        assert_eq!(step.usual_ms, Some(10.0));
        tracker.push(None);
        assert_eq!(tracker.step(100.0).health, Health::Down);
    }

    #[test]
    fn loss_counts_missing_replies_in_the_window() {
        let mut tracker = Tracker::default();
        for i in 0..40 {
            tracker.push(if i % 10 == 0 { None } else { Some(5.0) });
        }
        let step = tracker.step(100.0);
        assert!((step.loss_percent - 10.0).abs() < 0.01, "{}", step.loss_percent);
        assert_eq!(step.health, Health::Slow);
        assert_eq!(Tracker::default().step(1.0).health, Health::Off);
    }

    #[test]
    fn reads_the_first_nameserver() {
        let conf = "# comment\nsearch lan\nnameserver 100.100.100.100\nnameserver 1.1.1.1\n";
        assert_eq!(parse_resolver(conf).as_deref(), Some("100.100.100.100"));
        assert_eq!(parse_resolver("nameserver fe80::1%lo0\n").as_deref(), Some("fe80::1%lo0"));
        assert_eq!(parse_resolver(""), None);
    }

    #[test]
    fn a_scoped_ipv6_server_keeps_its_interface() {
        let Some(SocketAddr::V6(v6)) = server_address("fe80::1%lo0") else { panic!() };
        assert_ne!(v6.scope_id(), 0);
        assert_eq!(server_address("192.0.2.53"), Some("192.0.2.53:53".parse().unwrap()));
        assert_eq!(server_address("not an address"), None);
    }

    #[test]
    fn builds_a_standard_a_query() {
        let packet = build_query(0x1234, "example.com").unwrap();
        assert_eq!(&packet[..4], &[0x12, 0x34, 0x01, 0x00]);
        assert_eq!(&packet[12..], b"\x07example\x03com\x00\x00\x01\x00\x01");
        assert!(build_query(1, "bad..name").is_none());
        assert_eq!(&build_query(2, ".").unwrap()[12..], b"\x00\x00\x01\x00\x01", "the root");
    }

    #[test]
    fn an_ip_target_needs_no_lookup() {
        assert_eq!(resolve_target(" 192.0.2.1 "), (Some(Ipv4Addr::new(192, 0, 2, 1)), None));
        assert_eq!(resolve_target(""), (None, None));
    }
}
