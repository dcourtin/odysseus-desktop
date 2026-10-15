//! Adresses réseau lues directement auprès du système (getifaddrs) : aucune
//! commande à lancer, rien qui dépende du PATH (`ip` est dans /usr/sbin).

use std::ffi::CStr;

/// Première adresse IPv4 d'une interface, en texte (« 192.168.1.76 »)
pub fn ipv4(interface: &str) -> Option<String> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return None;
    }
    let mut found = None;
    let mut cur = list;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() {
            continue;
        }
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) };
        if name.to_bytes() != interface.as_bytes()
            || unsafe { (*ifa.ifa_addr).sa_family } as i32 != libc::AF_INET
        {
            continue;
        }
        let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
        found = Some(std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)).to_string());
        break;
    }
    unsafe { libc::freeifaddrs(list) };
    found
}

#[cfg(test)]
mod tests {
    #[test]
    fn loopback() {
        assert_eq!(super::ipv4("lo").as_deref(), Some("127.0.0.1"));
        assert_eq!(super::ipv4("pas-une-interface"), None);
    }
}
