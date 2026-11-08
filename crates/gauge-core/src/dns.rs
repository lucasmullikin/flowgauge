//! Which DNS servers macOS is using, and what set them. A VPN or a profile can put its own
//! servers ahead of the network's, so this says which one wins.

use std::net::IpAddr;
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct Resolver {
    pub nameservers: Vec<String>,
    /// The interface that supplied it, like `en0` or `utun2`.
    pub interface: Option<String>,
    /// Set for resolvers that only answer for one domain, like `lan.` or `local`.
    pub domain: Option<String>,
    pub order: u64,
}

/// Parses `scutil --dns`, keeping only the first section, which is what macOS uses for
/// ordinary lookups.
pub fn parse_scutil(output: &str) -> Vec<Resolver> {
    let mut resolvers: Vec<Resolver> = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("DNS configuration (for scoped queries)") {
            break;
        }
        if line.starts_with("resolver #") {
            resolvers.push(Resolver { nameservers: Vec::new(), interface: None, domain: None, order: u64::MAX });
            continue;
        }
        let Some(resolver) = resolvers.last_mut() else { continue };
        let Some((key, value)) = line.split_once(" : ") else { continue };
        let (key, value) = (key.trim(), value.trim());
        if key.starts_with("nameserver[") {
            resolver.nameservers.push(value.to_string());
        } else if key == "domain" {
            resolver.domain = Some(value.to_string());
        } else if key == "if_index" {
            resolver.interface = value.split_once('(').map(|(_, name)| name.trim_end_matches(')').to_string());
        } else if key == "order" {
            resolver.order = value.parse().unwrap_or(u64::MAX);
        }
    }
    resolvers
}

/// Resolvers for ordinary names, in the order macOS tries them.
pub fn default_resolvers(all: &[Resolver]) -> Vec<Resolver> {
    let mut found: Vec<Resolver> = all.iter().filter(|r| r.domain.is_none() && !r.nameservers.is_empty()).cloned().collect();
    found.sort_by_key(|r| r.order);
    found
}

pub fn read() -> Vec<Resolver> {
    Command::new("/usr/sbin/scutil")
        .arg("--dns")
        .output()
        .map(|o| default_resolvers(&parse_scutil(&String::from_utf8_lossy(&o.stdout))))
        .unwrap_or_default()
}

/// Who runs a DNS server, for well-known public ones and the usual local cases.
pub fn provider(server: &str, interface: Option<&str>, gateway: Option<IpAddr>) -> String {
    let address = server.split('%').next().unwrap_or(server);
    let known = match address {
        "100.100.100.100" | "fd7a:115c:a1e0::53" => Some("Tailscale"),
        "1.1.1.1" | "1.0.0.1" | "2606:4700:4700::1111" | "2606:4700:4700::1001" => Some("Cloudflare"),
        "8.8.8.8" | "8.8.4.4" | "2001:4860:4860::8888" | "2001:4860:4860::8844" => Some("Google"),
        "9.9.9.9" | "149.112.112.112" | "2620:fe::fe" => Some("Quad9"),
        "208.67.222.222" | "208.67.220.220" => Some("OpenDNS"),
        "94.140.14.14" | "94.140.15.15" => Some("AdGuard"),
        _ if address.starts_with("45.90.28.") || address.starts_with("45.90.30.") => Some("NextDNS"),
        _ => None,
    };
    if let Some(name) = known {
        return name.to_string();
    }
    if gateway.is_some_and(|g| g.to_string() == address) {
        return "Your router".to_string();
    }
    match interface {
        Some(i) if i.starts_with("utun") || i.starts_with("ipsec") => "A VPN".to_string(),
        _ => "Your network".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCUTIL: &str = "DNS configuration\n\n\
        resolver #1\n  search domain[0] : example.ts.net\n  nameserver[0] : 100.100.100.100\n  nameserver[1] : fd7a:115c:a1e0::53\n  if_index : 29 (utun2)\n  flags    : Supplemental\n  order    : 100600\n\n\
        resolver #2\n  nameserver[0] : 192.168.1.1\n  if_index : 14 (en0)\n  order    : 200000\n\n\
        resolver #3\n  domain   : lan.\n  nameserver[0] : 100.100.100.100\n  order    : 100602\n\n\
        resolver #4\n  domain   : local\n  options  : mdns\n  order    : 300000\n\n\
        DNS configuration (for scoped queries)\n\n\
        resolver #1\n  nameserver[0] : 10.0.0.1\n  if_index : 14 (en0)\n";

    #[test]
    fn finds_the_resolvers_used_for_ordinary_names_in_order() {
        let got = default_resolvers(&parse_scutil(SCUTIL));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].nameservers[0], "100.100.100.100");
        assert_eq!(got[0].interface.as_deref(), Some("utun2"));
        assert_eq!(got[1].nameservers, ["192.168.1.1"]);
        assert_eq!(got[1].interface.as_deref(), Some("en0"));
    }

    #[test]
    fn names_who_runs_a_server() {
        let router: IpAddr = "192.168.1.1".parse().unwrap();
        assert_eq!(provider("100.100.100.100", Some("utun2"), Some(router)), "Tailscale");
        assert_eq!(provider("1.1.1.1", Some("en0"), Some(router)), "Cloudflare");
        assert_eq!(provider("192.168.1.1", Some("en0"), Some(router)), "Your router");
        assert_eq!(provider("10.8.0.1", Some("utun4"), Some(router)), "A VPN");
        assert_eq!(provider("10.0.0.53", Some("en0"), Some(router)), "Your network");
    }
}
