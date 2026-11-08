//! Per-app and per-connection traffic from one `nettop` snapshot. Counters in a snapshot are
//! totals since each connection began, so traffic is the change between snapshots. An app's
//! traffic is the sum over its connections, because a process's own total drops when one of
//! its connections closes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct Connection {
    /// The local and remote ends as `nettop` prints them; unique while the connection lives.
    pub key: String,
    /// `None` for a socket with no single remote end, like one listening for broadcasts.
    pub remote: Option<IpAddr>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub rtt_ms: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Process {
    pub name: String,
    pub pid: u32,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connections: Vec<Connection>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub bytes_in: u64,
    pub bytes_out: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.bytes_in + self.bytes_out
    }
}

/// Traffic over one interval, grouped by app.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Interval {
    pub seconds: f64,
    pub apps: BTreeMap<String, Usage>,
    /// Keyed by app, then remote address.
    pub destinations: BTreeMap<String, BTreeMap<IpAddr, Usage>>,
    /// Latest round trip per remote address, where `nettop` reports one.
    pub rtt_ms: BTreeMap<IpAddr, f32>,
}

/// `fe80::1%en0.443`, `10.0.0.2:443`, or `*.*` for an unconnected socket.
fn parse_remote(endpoint: &str, ipv6: bool) -> Option<IpAddr> {
    let address = if ipv6 { endpoint.rsplit_once('.')?.0 } else { endpoint.rsplit_once(':')?.0 };
    let address = address.split('%').next()?;
    address.parse().ok()
}

/// Sockets with no remote end on the mDNS or SSDP ports talk to their multicast group, which
/// stays on the local network.
fn multicast_group(local: &str, ipv6: bool) -> Option<IpAddr> {
    let port = if ipv6 { local.rsplit_once('.')?.1 } else { local.rsplit_once(':')?.1 };
    let group = match (port, ipv6) {
        ("5353", false) => "224.0.0.251",
        ("5353", true) => "ff02::fb",
        ("1900", false) => "239.255.255.250",
        ("1900", true) => "ff02::c",
        _ => return None,
    };
    group.parse().ok()
}

/// `nettop` prints round trips as `1.00 ms`, sometimes cut to `6284.22 m`.
fn parse_rtt(text: &str) -> Option<f32> {
    let text = text.trim();
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let value: f32 = text[..split].parse().ok()?;
    match text[split..].trim() {
        "us" | "µs" => Some(value / 1000.0),
        "ms" | "m" => Some(value),
        "s" => Some(value * 1000.0),
        _ => None,
    }
}

/// Parses `nettop -L 1 -n -x -J bytes_in,bytes_out,rtt_avg`.
pub fn parse_snapshot(output: &str) -> Vec<Process> {
    let mut processes: Vec<Process> = Vec::new();
    for line in output.lines() {
        if line.starts_with(',') {
            continue;
        }
        let fields: Vec<&str> = line.trim_end_matches(',').rsplitn(4, ',').collect();
        // Connection rows look like `tcp4 a<->b`, `udp6 a<->b` or `quic6 a<->b`.
        if let Some((protocol, rest)) = line.split_once(' ')
            && rest.contains("<->")
        {
            // Endpoint, in, out, rtt. Listening sockets have no counts and are skipped.
            let parts: Vec<&str> = rest.splitn(5, ',').collect();
            let (Some(ends), Some(input), Some(out)) = (parts.first(), parts.get(1), parts.get(2)) else { continue };
            let (Ok(bytes_in), Ok(bytes_out)) = (input.parse(), out.parse()) else { continue };
            let Some((local, remote)) = ends.split_once("<->") else { continue };
            let ipv6 = protocol.ends_with('6');
            if let Some(process) = processes.last_mut() {
                // Unconnected sockets can print the same ends several times in one process, in
                // any order, so those are added together into one.
                let key = format!("{protocol} {ends}");
                if let Some(same) = process.connections.iter_mut().find(|c| c.key == key) {
                    same.bytes_in += bytes_in;
                    same.bytes_out += bytes_out;
                    continue;
                }
                process.connections.push(Connection {
                    key,
                    remote: parse_remote(remote, ipv6).or_else(|| multicast_group(local, ipv6)),
                    bytes_in,
                    bytes_out,
                    rtt_ms: parts.get(3).and_then(|t| parse_rtt(t)),
                });
            }
            continue;
        }
        // Process rows: name.pid, in, out, (empty rtt).
        let (name_pid, input, out) = match fields.as_slice() {
            [_, out, input, name] => (*name, *input, *out),
            [out, input, name] => (*name, *input, *out),
            _ => continue,
        };
        let (Ok(bytes_in), Ok(bytes_out)) = (input.parse(), out.parse()) else { continue };
        let Some((name, pid)) = name_pid.rsplit_once('.') else { continue };
        let Ok(pid) = pid.parse() else { continue };
        processes.push(Process { name: name.to_string(), pid, bytes_in, bytes_out, connections: Vec::new() });
    }
    processes
}

pub fn read_snapshot() -> std::io::Result<Vec<Process>> {
    let output = Command::new("/usr/bin/nettop")
        .args(["-L", "1", "-n", "-x", "-J", "bytes_in,bytes_out,rtt_avg"])
        .output()?;
    Ok(parse_snapshot(&String::from_utf8_lossy(&output.stdout)))
}

/// Helpers live inside their app's bundle, so traffic groups under the app people know. A
/// service inside a framework, like WebKit's networking process, takes the framework's name.
pub fn app_name_from_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    if let Some(bundle) = parts.iter().find(|part| part.ends_with(".app")) {
        return Some(bundle.trim_end_matches(".app").to_string());
    }
    let framework = parts.iter().find(|part| part.ends_with(".framework"))?;
    parts.iter().any(|part| part.ends_with(".xpc")).then(|| framework.trim_end_matches(".framework").to_string())
}

fn process_path(pid: u32) -> Option<String> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is as large as the call allows.
    let length = unsafe { libc::proc_pidpath(pid as libc::c_int, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if length <= 0 {
        return None;
    }
    buffer.truncate(length as usize);
    String::from_utf8(buffer).ok()
}

/// Turns snapshots into traffic per interval. The first snapshot only sets the starting point.
#[derive(Default)]
pub struct Tracker {
    connections: HashMap<(u32, String), Usage>,
    names: HashMap<(String, u32), String>,
    started: bool,
}

impl Tracker {
    fn app_name(&mut self, process: &Process, lookup: &dyn Fn(u32) -> Option<String>) -> String {
        self.names
            .entry((process.name.clone(), process.pid))
            .or_insert_with(|| lookup(process.pid).and_then(|p| app_name_from_path(&p)).unwrap_or_else(|| process.name.clone()))
            .clone()
    }

    pub fn step(&mut self, snapshot: &[Process], seconds: f64) -> Interval {
        self.step_with(snapshot, seconds, &process_path)
    }

    pub fn step_with(&mut self, snapshot: &[Process], seconds: f64, lookup: &dyn Fn(u32) -> Option<String>) -> Interval {
        let mut interval = Interval { seconds, ..Interval::default() };
        let mut seen_processes = HashSet::new();
        let mut seen_connections = HashSet::new();
        for process in snapshot {
            let app = self.app_name(process, lookup);
            seen_processes.insert((process.name.clone(), process.pid));
            for connection in &process.connections {
                let key = (process.pid, connection.key.clone());
                let now = Usage { bytes_in: connection.bytes_in, bytes_out: connection.bytes_out };
                // A connection new since the starting point began after it, so all of it is new.
                let before = self.connections.get(&key).cloned().unwrap_or_default();
                if self.started {
                    let delta = Usage {
                        bytes_in: now.bytes_in.saturating_sub(before.bytes_in),
                        bytes_out: now.bytes_out.saturating_sub(before.bytes_out),
                    };
                    let usage = interval.apps.entry(app.clone()).or_default();
                    usage.bytes_in += delta.bytes_in;
                    usage.bytes_out += delta.bytes_out;
                    if let Some(remote) = connection.remote {
                        let usage = interval.destinations.entry(app.clone()).or_default().entry(remote).or_default();
                        usage.bytes_in += delta.bytes_in;
                        usage.bytes_out += delta.bytes_out;
                    }
                }
                if let (Some(remote), Some(rtt)) = (connection.remote, connection.rtt_ms) {
                    interval.rtt_ms.insert(remote, rtt);
                }
                self.connections.insert(key.clone(), now);
                seen_connections.insert(key);
            }
        }
        self.connections.retain(|k, _| seen_connections.contains(k));
        self.names.retain(|k, _| seen_processes.contains(k));
        interval.apps.retain(|_, u| u.total() > 0);
        for destinations in interval.destinations.values_mut() {
            destinations.retain(|_, u| u.total() > 0);
        }
        interval.destinations.retain(|_, d| !d.is_empty());
        self.started = true;
        interval
    }
}

/// Addresses that never leave this network: private, link-local, loopback, unique local, and
/// shared address space (used by carrier NAT and VPNs like Tailscale).
pub fn is_local(address: &IpAddr) -> bool {
    if let IpAddr::V6(v6) = address
        && let Some(v4) = v6.to_ipv4_mapped()
    {
        return is_local(&IpAddr::V4(v4));
    }
    match address {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_multicast() || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || v6.is_multicast() || (first & 0xffc0) == 0xfe80 || (first & 0xfe00) == 0xfc00
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = ",bytes_in,bytes_out,rtt_avg,\n\
        launchd.1,0,0,,\n\
        tcp4 *:445<->*:*,,,,\n\
        apsd.126,311818,647846,,\n\
        tcp4 192.0.2.10:59440<->203.0.113.185:5223,311818,647846,111.31 ms,\n\
        Safari Networkin.42,5000,700,,\n\
        tcp6 fe80::1%en0.55895<->fe80::2%en0.49201,5000,700,6284.22 m,\n\
        quic6 2001:db8::5.50491<->2001:db8:1::7.443,900,100,30.00 ms,\n\
        quic4 192.0.2.10:6000<->198.51.100.9:443,90,10,,\n\
        udp4 *:5353<->*:*,40,4,,\n";

    #[test]
    fn parses_processes_and_their_connected_sockets() {
        let got = parse_snapshot(SNAPSHOT);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].name, "launchd");
        assert!(got[0].connections.is_empty());
        assert_eq!(got[1].pid, 126);
        assert_eq!(got[1].connections[0].remote, Some("203.0.113.185".parse::<IpAddr>().unwrap()));
        assert_eq!(got[1].connections[0].rtt_ms, Some(111.31));
        assert_eq!(got[2].name, "Safari Networkin");
        assert_eq!(got[2].connections[0].remote, Some("fe80::2".parse::<IpAddr>().unwrap()));
        assert_eq!(got[2].connections[0].rtt_ms, Some(6284.22));
        let quic = &got[2].connections;
        assert_eq!(quic.len(), 4, "QUIC and unconnected UDP rows belong to the process above them");
        assert_eq!(quic[1].remote, Some("2001:db8:1::7".parse().unwrap()));
        assert_eq!(quic[2].remote, Some("198.51.100.9".parse().unwrap()));
        assert_eq!(quic[3].remote, Some("224.0.0.251".parse().unwrap()), "mDNS goes to its group");
    }

    #[test]
    fn repeated_sockets_in_one_process_keep_separate_counts() {
        let first = "p.5,0,0,,\nudp6 *.5353<->*.*,400,0,,\nudp6 *.5353<->*.*,600,0,,\nudp6 *.7000<->*.*,9,0,,\n";
        let reordered = "p.5,0,0,,\nudp6 *.5353<->*.*,600,0,,\nudp6 *.7000<->*.*,9,0,,\nudp6 *.5353<->*.*,450,0,,\n";
        let mut tracker = Tracker::default();
        let none = |_| None;
        tracker.step_with(&parse_snapshot(first), 1.0, &none);
        let got = tracker.step_with(&parse_snapshot(reordered), 1.0, &none);
        assert_eq!(got.apps["p"].bytes_in, 50, "only the real change counts, whatever the order: {got:?}");
    }

    #[test]
    fn parses_round_trip_units() {
        assert_eq!(parse_rtt("250 us"), Some(0.25));
        assert_eq!(parse_rtt("1.5 s"), Some(1500.0));
        assert_eq!(parse_rtt(""), None);
        assert_eq!(parse_rtt("fast"), None);
    }

    #[test]
    fn helpers_group_under_their_app() {
        let path = "/Applications/Safari.app/Contents/XPCServices/Networking.xpc/Contents/MacOS/Networking";
        assert_eq!(app_name_from_path(path), Some("Safari".into()));
        let nested = "/Applications/Google Chrome.app/Contents/Frameworks/x/Helpers/Google Chrome Helper.app/Contents/MacOS/h";
        assert_eq!(app_name_from_path(nested), Some("Google Chrome".into()));
        assert_eq!(app_name_from_path("/usr/libexec/apsd"), None);
        let webkit = "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking";
        assert_eq!(app_name_from_path(webkit), Some("WebKit".into()));
        assert_eq!(app_name_from_path("/System/Library/PrivateFrameworks/CloudKitDaemon.framework/Support/cloudd"), None);
    }

    fn process(name: &str, pid: u32, connections: Vec<Connection>) -> Process {
        Process { name: name.into(), pid, bytes_in: 0, bytes_out: 0, connections }
    }

    fn connection(key: &str, remote: &str, bytes_in: u64) -> Connection {
        Connection { key: key.into(), remote: remote.parse().ok(), bytes_in, bytes_out: 0, rtt_ms: None }
    }

    #[test]
    fn traffic_is_the_change_since_the_last_snapshot() {
        let lookup = |pid: u32| (pid == 7).then(|| "/Applications/Mail.app/Contents/MacOS/Mail".to_string());
        let mut tracker = Tracker::default();
        let first = tracker.step_with(&[process("Mail", 7, vec![connection("a", "203.0.113.5", 1000)])], 2.0, &lookup);
        assert!(first.apps.is_empty(), "the first snapshot only sets the starting point");
        let second = tracker.step_with(
            &[
                process("Mail", 7, vec![connection("a", "203.0.113.5", 1400), connection("b", "198.51.100.1", 100)]),
                process("curl", 9, vec![connection("c", "*", 300)]),
            ],
            2.0,
            &lookup,
        );
        assert_eq!(second.apps["Mail"].bytes_in, 500);
        assert_eq!(second.apps["curl"].bytes_in, 300, "a connection that started since counts in full");
        assert!(!second.destinations.contains_key("curl"), "no remote end, so no destination");
        let mail = &second.destinations["Mail"];
        assert_eq!(mail[&"203.0.113.5".parse::<IpAddr>().unwrap()].bytes_in, 400);
        assert_eq!(mail[&"198.51.100.1".parse::<IpAddr>().unwrap()].bytes_in, 100);
    }

    #[test]
    fn closing_one_connection_doesnt_hide_traffic_on_another() {
        let none = |_| None;
        let mut tracker = Tracker::default();
        tracker.step_with(&[process("x", 5, vec![connection("big", "203.0.113.5", 50_000), connection("small", "203.0.113.6", 10)])], 1.0, &none);
        let got = tracker.step_with(&[process("x", 5, vec![connection("small", "203.0.113.6", 2010)])], 1.0, &none);
        assert_eq!(got.apps["x"].bytes_in, 2000);
    }

    #[test]
    fn a_reused_connection_with_lower_counters_is_never_negative() {
        let mut tracker = Tracker::default();
        let none = |_| None;
        tracker.step_with(&[process("x", 5, vec![connection("a", "203.0.113.5", 9000)])], 1.0, &none);
        let got = tracker.step_with(&[process("x", 5, vec![connection("a", "203.0.113.5", 10)])], 1.0, &none);
        assert!(got.apps.is_empty());
    }

    #[test]
    fn local_addresses_include_vpn_and_link_local_ranges() {
        for local in ["192.168.1.1", "10.2.3.4", "100.101.1.2", "fe80::1", "fd7a:9fa5::1", "127.0.0.1", "::ffff:192.168.1.5"] {
            assert!(is_local(&local.parse().unwrap()), "{local}");
        }
        for remote in ["203.0.113.185", "2606:4700::1111", "100.128.0.1"] {
            assert!(!is_local(&remote.parse().unwrap()), "{remote}");
        }
    }
}
