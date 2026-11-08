//! ICMP echo without root, using the datagram ICMP socket macOS allows for normal users.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

const ECHO_REQUEST: u8 = 8;
const ECHO_REPLY: u8 = 0;
const PACKET_BYTES: usize = 16;

pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = data
        .chunks(2)
        .map(|c| u32::from(u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])))
        .sum();
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

pub fn build_request(identifier: u16, sequence: u16) -> [u8; PACKET_BYTES] {
    let mut packet = [0u8; PACKET_BYTES];
    packet[0] = ECHO_REQUEST;
    packet[4..6].copy_from_slice(&identifier.to_be_bytes());
    packet[6..8].copy_from_slice(&sequence.to_be_bytes());
    let sum = checksum(&packet);
    packet[2..4].copy_from_slice(&sum.to_be_bytes());
    packet
}

/// macOS hands datagram ICMP replies over with the IP header still attached.
pub fn is_reply_for(packet: &[u8], sequence: u16) -> bool {
    let header = match packet.first() {
        Some(first) if first >> 4 == 4 => usize::from(first & 0x0f) * 4,
        Some(_) => 0,
        None => return false,
    };
    let Some(icmp) = packet.get(header..) else {
        return false;
    };
    icmp.len() >= 8 && icmp[0] == ECHO_REPLY && icmp[6..8] == sequence.to_be_bytes()
}

/// Round trip in milliseconds, or `None` on timeout or failure.
pub fn ping(address: Ipv4Addr, sequence: u16, timeout: Duration) -> Option<f32> {
    // SAFETY: plain socket call; the descriptor is closed below on every path.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
    if fd < 0 {
        return None;
    }
    let result = exchange(fd, address, sequence, timeout);
    // SAFETY: fd is open and owned here.
    unsafe { libc::close(fd) };
    result
}

fn exchange(fd: libc::c_int, address: Ipv4Addr, sequence: u16, timeout: Duration) -> Option<f32> {
    let time = libc::timeval {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_usec: timeout.subsec_micros() as libc::suseconds_t,
    };
    // SAFETY: all-zero is valid for sockaddr_in; pointers and sizes below describe live values.
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&raw const time).cast(),
            size_of::<libc::timeval>() as libc::socklen_t,
        );
        let mut target: libc::sockaddr_in = std::mem::zeroed();
        target.sin_len = size_of::<libc::sockaddr_in>() as u8;
        target.sin_family = libc::AF_INET as libc::sa_family_t;
        target.sin_addr.s_addr = u32::from(address).to_be();
        let request = build_request(std::process::id() as u16, sequence);
        let started = Instant::now();
        let sent = libc::sendto(
            fd,
            request.as_ptr().cast(),
            request.len(),
            0,
            (&raw const target).cast(),
            size_of::<libc::sockaddr_in>() as libc::socklen_t,
        );
        if sent < 0 {
            return None;
        }
        let mut buffer = [0u8; 256];
        while started.elapsed() < timeout {
            let n = libc::recv(fd, buffer.as_mut_ptr().cast(), buffer.len(), 0);
            if n <= 0 {
                return None;
            }
            if is_reply_for(&buffer[..n as usize], sequence) {
                return Some(started.elapsed().as_secs_f32() * 1000.0);
            }
        }
        None
    }
}

pub fn parse_gateway(route_output: &str) -> Option<Ipv4Addr> {
    route_output
        .lines()
        .find_map(|line| line.trim().strip_prefix("gateway: "))
        .and_then(|g| g.parse().ok())
}

pub fn default_gateway() -> Option<Ipv4Addr> {
    let output = std::process::Command::new("/sbin/route")
        .args(["-n", "get", "default"])
        .output()
        .ok()?;
    parse_gateway(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_of_a_built_packet_verifies_to_zero() {
        assert_eq!(checksum(&build_request(0x1234, 7)), 0);
    }

    #[test]
    fn recognizes_replies_with_and_without_an_ip_header() {
        let mut bare = [0u8; 16];
        bare[7] = 9;
        assert!(is_reply_for(&bare, 9));
        assert!(!is_reply_for(&bare, 8));
        let mut with_ip = vec![0x45u8];
        with_ip.extend([0u8; 19]);
        with_ip.extend(bare);
        assert!(is_reply_for(&with_ip, 9));
        assert!(!is_reply_for(&[], 9));
        assert!(!is_reply_for(&[0x45], 9));
    }

    #[test]
    fn parses_a_gateway_and_skips_ipv6_or_missing_ones() {
        assert_eq!(parse_gateway("  gateway: 192.168.1.1\n"), Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(parse_gateway("  gateway: fe80::1%en0\n"), None);
        assert_eq!(parse_gateway("nothing"), None);
    }

    #[test]
    fn pings_loopback_without_root() {
        let got = ping(Ipv4Addr::LOCALHOST, 1, Duration::from_secs(1));
        assert!(got.is_some_and(|ms| ms < 1000.0), "{got:?}");
    }
}
