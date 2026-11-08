//! Optional reverse DNS for remote addresses. Each lookup goes to your DNS server, so it only
//! runs when turned on, on one background thread, and every answer is cached.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const MAX_CACHED: usize = 4000;

/// A host name, or `None` when the address has no name.
pub fn reverse_lookup(address: IpAddr) -> Option<String> {
    let socket = std::net::SocketAddr::new(address, 0);
    let (storage, length) = socket_storage(&socket);
    let mut host = [0 as libc::c_char; libc::NI_MAXHOST as usize];
    // SAFETY: the address and buffer are valid for the lengths passed.
    let code = unsafe {
        libc::getnameinfo(
            (&raw const storage).cast(),
            length,
            host.as_mut_ptr(),
            host.len() as libc::socklen_t,
            std::ptr::null_mut(),
            0,
            libc::NI_NAMEREQD,
        )
    };
    if code != 0 {
        return None;
    }
    // SAFETY: getnameinfo wrote a NUL-terminated string on success.
    let name = unsafe { std::ffi::CStr::from_ptr(host.as_ptr()) }.to_string_lossy().trim_end_matches('.').to_string();
    (!name.is_empty() && name.parse::<IpAddr>().is_err()).then_some(name)
}

fn socket_storage(socket: &std::net::SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: all-zero is valid for these plain structs; each branch fills the one it uses.
    unsafe {
        let mut storage: libc::sockaddr_storage = std::mem::zeroed();
        match socket {
            std::net::SocketAddr::V4(v4) => {
                let raw = (&raw mut storage).cast::<libc::sockaddr_in>();
                (*raw).sin_len = size_of::<libc::sockaddr_in>() as u8;
                (*raw).sin_family = libc::AF_INET as libc::sa_family_t;
                (*raw).sin_addr.s_addr = u32::from(*v4.ip()).to_be();
                (storage, size_of::<libc::sockaddr_in>() as libc::socklen_t)
            }
            std::net::SocketAddr::V6(v6) => {
                let raw = (&raw mut storage).cast::<libc::sockaddr_in6>();
                (*raw).sin6_len = size_of::<libc::sockaddr_in6>() as u8;
                (*raw).sin6_family = libc::AF_INET6 as libc::sa_family_t;
                (*raw).sin6_addr.s6_addr = v6.ip().octets();
                (storage, size_of::<libc::sockaddr_in6>() as libc::socklen_t)
            }
        }
    }
}

/// The last two labels, or three for names like `example.co.uk`, so many servers of one
/// service group together.
pub fn short_domain(name: &str) -> String {
    let labels: Vec<&str> = name.split('.').filter(|l| !l.is_empty()).collect();
    let keep = match labels.as_slice() {
        [.., second, last] if last.len() == 2 && second.len() <= 3 && labels.len() >= 3 => 3,
        _ => 2,
    };
    labels[labels.len().saturating_sub(keep)..].join(".")
}

#[derive(Clone)]
pub struct Names {
    cache: Arc<Mutex<HashMap<IpAddr, Option<String>>>>,
    asked: Arc<Mutex<HashSet<IpAddr>>>,
    queue: Sender<IpAddr>,
    /// Lookups already queued are dropped once this is turned off.
    enabled: Arc<AtomicBool>,
}

impl Names {
    pub fn start() -> Names {
        let (queue, requests): (Sender<IpAddr>, Receiver<IpAddr>) = channel();
        let names = Names { cache: Arc::default(), asked: Arc::default(), queue, enabled: Arc::default() };
        let worker = names.clone();
        std::thread::spawn(move || {
            for address in requests {
                if !worker.enabled.load(Ordering::Relaxed) {
                    if let Ok(mut asked) = worker.asked.lock() {
                        asked.remove(&address);
                    }
                    continue;
                }
                let name = reverse_lookup(address);
                if let Ok(mut cache) = worker.cache.lock() {
                    if cache.len() >= MAX_CACHED
                        && let Some(oldest) = cache.keys().next().copied()
                    {
                        cache.remove(&oldest);
                    }
                    cache.insert(address, name);
                }
                if let Ok(mut asked) = worker.asked.lock() {
                    asked.remove(&address);
                }
            }
        });
        names
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// A cached answer only, never a new lookup.
    pub fn cached(&self, address: IpAddr) -> Option<String> {
        self.cache.lock().ok()?.get(&address).cloned().flatten()
    }

    /// A cached answer. Unknown addresses are queued and answered on a later call.
    pub fn get(&self, address: IpAddr) -> Option<String> {
        if !self.enabled.load(Ordering::Relaxed) {
            return None;
        }
        if let Some(found) = self.cache.lock().ok()?.get(&address) {
            return found.clone();
        }
        if self.asked.lock().ok()?.insert(address) {
            let _ = self.queue.send(address);
        }
        None
    }

    pub fn clear(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_domains_keep_the_registered_name() {
        assert_eq!(short_domain("edge-24.cdn.example.net"), "example.net");
        assert_eq!(short_domain("mail.example.co.uk"), "example.co.uk");
        assert_eq!(short_domain("example.com"), "example.com");
        assert_eq!(short_domain("localhost"), "localhost");
    }

    #[test]
    fn loopback_resolves_without_leaving_the_mac() {
        let name = reverse_lookup("127.0.0.1".parse().unwrap());
        assert!(name.is_some_and(|n| n.contains("localhost")));
    }
}
