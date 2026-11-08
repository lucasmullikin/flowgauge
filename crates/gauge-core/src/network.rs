//! Interface totals are exact and 64-bit, so long-running totals don't wrap.
//! Per-app traffic has no public API; it comes from Apple's `nettop`, on demand only.

use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Counters {
    pub name: String,
    pub up: bool,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InterfaceRate {
    pub name: String,
    pub up: bool,
    pub is_default: bool,
    pub addresses: Vec<String>,
    pub bytes_in_total: u64,
    pub bytes_out_total: u64,
    pub in_per_sec: f64,
    pub out_per_sec: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppTraffic {
    pub process: String,
    pub pid: u32,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

/// A counter that went backwards (an interface was reset) counts as zero, never a huge spike.
pub fn rate(previous: u64, current: u64, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        return 0.0;
    }
    current.saturating_sub(previous) as f64 / seconds
}

const NETLINK_GENERIC: libc::c_int = 0;
const IFMIB_IFDATA: libc::c_int = 2;
const IFDATA_GENERAL: libc::c_int = 1;

// Mirrors struct ifmibdata in net/if_mib.h.
#[repr(C)]
#[derive(Clone, Copy)]
struct IfMibData {
    name: [libc::c_char; libc::IF_NAMESIZE],
    pcount: u32,
    flags: u32,
    snd_len: u32,
    snd_maxlen: u32,
    snd_drops: u32,
    filler: [u32; 4],
    data: libc::if_data64,
}

fn read_interface(index: u32) -> std::io::Result<IfMibData> {
    let mut mib = [
        libc::CTL_NET,
        libc::PF_LINK,
        NETLINK_GENERIC,
        IFMIB_IFDATA,
        index as libc::c_int,
        IFDATA_GENERAL,
    ];
    // SAFETY: all-zero is a valid value for this plain-data struct.
    let mut data: IfMibData = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<IfMibData>();
    // SAFETY: data has `size` writable bytes.
    let code = unsafe {
        libc::sysctl(mib.as_mut_ptr(), 6, (&raw mut data).cast(), &mut size, std::ptr::null_mut(), 0)
    };
    if code != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(data)
}

/// Uses the interface MIB. On macOS 27 the routing sysctl and getifaddrs return values cut to
/// 32 bits and rounded to 1 KiB without a private entitlement; this one matches `netstat`.
pub fn read_counters() -> std::io::Result<Vec<Counters>> {
    // SAFETY: the list is NULL-terminated, only read before being freed once.
    unsafe {
        let list = libc::if_nameindex();
        if list.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut counters = Vec::new();
        let mut cursor = list;
        while (*cursor).if_index != 0 && !(*cursor).if_name.is_null() {
            let name = CStr::from_ptr((*cursor).if_name).to_string_lossy().into_owned();
            // An interface can vanish between listing and reading; skip it.
            if let Ok(data) = read_interface((*cursor).if_index) {
                counters.push(Counters {
                    name,
                    up: data.flags as libc::c_int & libc::IFF_UP != 0,
                    bytes_in: data.data.ifi_ibytes,
                    bytes_out: data.data.ifi_obytes,
                });
            }
            cursor = cursor.add(1);
        }
        libc::if_freenameindex(list);
        Ok(counters)
    }
}

pub fn read_addresses() -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut head = std::ptr::null_mut();
    // SAFETY: head is a valid out pointer, and the list is freed below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return map;
    }
    let mut cursor = head;
    while !cursor.is_null() {
        // SAFETY: cursor is a live node of the getifaddrs list.
        let node = unsafe { &*cursor };
        cursor = node.ifa_next;
        if node.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: ifa_addr is non-null and sa_family says which sockaddr it really is.
        let address = unsafe {
            match (*node.ifa_addr).sa_family as i32 {
                libc::AF_INET => {
                    let raw = &*node.ifa_addr.cast::<libc::sockaddr_in>();
                    Some(std::net::Ipv4Addr::from(u32::from_be(raw.sin_addr.s_addr)).to_string())
                }
                libc::AF_INET6 => {
                    let raw = &*node.ifa_addr.cast::<libc::sockaddr_in6>();
                    Some(std::net::Ipv6Addr::from(raw.sin6_addr.s6_addr).to_string())
                }
                _ => None,
            }
        };
        if let Some(address) = address {
            // SAFETY: ifa_name is a NUL-terminated string owned by the list.
            let name = unsafe { CStr::from_ptr(node.ifa_name) }.to_string_lossy().into_owned();
            map.entry(name).or_default().push(address);
        }
    }
    // SAFETY: head came from getifaddrs and is freed once.
    unsafe { libc::freeifaddrs(head) };
    map
}

pub fn parse_default_route(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("interface: "))
        .map(str::to_string)
}

pub fn default_interface() -> Option<String> {
    let output = Command::new("/sbin/route").args(["-n", "get", "default"]).output().ok()?;
    parse_default_route(&String::from_utf8_lossy(&output.stdout))
}

/// Addresses and the default route change rarely and cost more to read, so callers pass them in.
pub fn rates(
    previous: &[Counters],
    current: &[Counters],
    seconds: f64,
    addresses: &BTreeMap<String, Vec<String>>,
    default: Option<&str>,
) -> Vec<InterfaceRate> {
    current
        .iter()
        .map(|now| {
            let before = previous.iter().find(|p| p.name == now.name);
            InterfaceRate {
                name: now.name.clone(),
                up: now.up,
                is_default: default == Some(now.name.as_str()),
                addresses: addresses.get(&now.name).cloned().unwrap_or_default(),
                bytes_in_total: now.bytes_in,
                bytes_out_total: now.bytes_out,
                in_per_sec: before.map_or(0.0, |b| rate(b.bytes_in, now.bytes_in, seconds)),
                out_per_sec: before.map_or(0.0, |b| rate(b.bytes_out, now.bytes_out, seconds)),
            }
        })
        .collect()
}

/// Splits `nettop -x` CSV into samples: `name.pid,in,out,` rows under a `,bytes_in,...` header.
fn parse_samples(output: &str) -> Vec<BTreeMap<(String, u32), (u64, u64)>> {
    let mut samples: Vec<BTreeMap<(String, u32), (u64, u64)>> = Vec::new();
    for line in output.lines() {
        if line.starts_with(',') {
            samples.push(BTreeMap::new());
            continue;
        }
        let mut fields = line.trim_end_matches(',').rsplitn(3, ',');
        let (Some(out), Some(input), Some(process)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(out), Ok(input)) = (out.parse(), input.parse()) else {
            continue;
        };
        let Some((name, pid)) = process.rsplit_once('.') else {
            continue;
        };
        let (Ok(pid), Some(sample)) = (pid.parse(), samples.last_mut()) else {
            continue;
        };
        sample.insert((name.to_string(), pid), (input, out));
    }
    samples
}

/// Traffic between the first and last samples, busiest first; idle processes are dropped.
pub fn parse_nettop(output: &str) -> Vec<AppTraffic> {
    let samples = parse_samples(output);
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return Vec::new();
    };
    let mut apps: Vec<AppTraffic> = last
        .iter()
        .map(|((process, pid), (input, out))| {
            let (first_in, first_out) = first.get(&(process.clone(), *pid)).copied().unwrap_or((0, 0));
            AppTraffic {
                process: process.clone(),
                pid: *pid,
                bytes_in: input.saturating_sub(first_in),
                bytes_out: out.saturating_sub(first_out),
            }
        })
        .filter(|a| a.bytes_in + a.bytes_out > 0)
        .collect();
    apps.sort_by_key(|a| std::cmp::Reverse(a.bytes_in + a.bytes_out));
    apps
}

pub fn app_traffic(seconds: u32) -> std::io::Result<Vec<AppTraffic>> {
    let output = Command::new("/usr/bin/nettop")
        .args(["-P", "-L", "2", "-n", "-x", "-J", "bytes_in,bytes_out", "-s", &seconds.to_string()])
        .output()?;
    Ok(parse_nettop(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_divides_by_elapsed_time() {
        assert_eq!(rate(1000, 3000, 2.0), 1000.0);
    }

    #[test]
    fn rate_treats_a_reset_counter_as_zero() {
        assert_eq!(rate(5000, 10, 1.0), 0.0);
        assert_eq!(rate(0, 10, 0.0), 0.0);
    }

    #[test]
    fn reads_loopback_counters_from_the_kernel() {
        let counters = read_counters().unwrap();
        assert!(counters.iter().any(|c| c.name == "lo0" && c.up));
    }

    #[test]
    fn en0_counters_match_netstat_rather_than_a_truncated_value() {
        let output = Command::new("/usr/sbin/netstat").arg("-ibn").output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let row: Vec<&str> = text
            .lines()
            .find(|l| l.starts_with("en0") && l.contains("<Link#"))
            .map_or_else(Vec::new, |line| line.split_whitespace().collect());
        if row.is_empty() {
            return;
        }
        let from_netstat: u64 = row[6].parse().unwrap();
        let counters = read_counters().unwrap();
        let en0 = counters.iter().find(|c| c.name == "en0").unwrap();
        assert!(en0.bytes_in >= from_netstat);
        assert!(en0.bytes_in - from_netstat < 1_000_000_000);
    }

    #[test]
    fn reads_loopback_address() {
        assert!(read_addresses()["lo0"].contains(&"127.0.0.1".to_string()));
    }

    #[test]
    fn default_route_comes_from_the_interface_line() {
        let output = "   route to: default\n  interface: utun4\n      flags: <UP>\n";
        assert_eq!(parse_default_route(output), Some("utun4".to_string()));
        assert_eq!(parse_default_route("route: not in table\n"), None);
    }

    #[test]
    fn rates_mark_the_default_and_attach_addresses() {
        let counters = |bytes| {
            vec![Counters { name: "en0".into(), up: true, bytes_in: bytes, bytes_out: bytes }]
        };
        let addresses = BTreeMap::from([("en0".to_string(), vec!["10.0.0.2".to_string()])]);
        let got = rates(&counters(100), &counters(300), 2.0, &addresses, Some("en0"));
        assert!(got[0].is_default);
        assert_eq!(got[0].in_per_sec, 100.0);
        assert_eq!(got[0].addresses, ["10.0.0.2"]);
    }

    const NETTOP: &str = ",bytes_in,bytes_out,\n\
        kernel_task.0,100,200,\n\
        Google Chrome He.42,1000,500,\n\
        idle.7,5,5,\n\
        ,bytes_in,bytes_out,\n\
        kernel_task.0,150,200,\n\
        Google Chrome He.42,4000,700,\n\
        idle.7,5,5,\n\
        new.thing.9,10,0,\n";

    #[test]
    fn nettop_reports_the_change_between_samples_busiest_first() {
        let apps = parse_nettop(NETTOP);
        assert_eq!(apps.len(), 3);
        assert_eq!(
            apps[0],
            AppTraffic {
                process: "Google Chrome He".into(),
                pid: 42,
                bytes_in: 3000,
                bytes_out: 200
            }
        );
        assert_eq!(apps[1].process, "kernel_task");
        assert_eq!(apps[2].process, "new.thing");
        assert_eq!(apps[2].bytes_in, 10);
    }

    #[test]
    fn nettop_ignores_garbage_and_empty_output() {
        assert!(parse_nettop("").is_empty());
        assert!(parse_nettop("nettop: failed\nfoo,bar\n").is_empty());
    }
}
