//! Outgoing requests to model providers. An address an admin types could point at the server's own network (a
//! router page, another service, a cloud metadata address); such addresses are refused unless the provider is
//! marked as local on purpose (Ollama or LM Studio on the same network).

use std::net::{IpAddr, Ipv4Addr};

/// Addresses that are not on the public internet.
pub fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => internal_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return internal_v4(v4);
            }
            let s = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link-local
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x64 && s[1] == 0xff9b) // NAT64 can reach internal IPv4
        }
    }
}

fn internal_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // includes 169.254.169.254, the cloud metadata address
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (64..128).contains(&o[1])) // shared address space (CGNAT)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (18..20).contains(&o[1])) // benchmarking
        || o[0] >= 240 // reserved
}

/// The host and port of an http(s) URL, after checking its shape.
pub fn host_port(url: &str) -> Result<(String, u16), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("The address must start with http:// or https://.")?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "https" => 443,
        "http" => 80,
        _ => return Err("Only http and https addresses are allowed.".into()),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err("Addresses with a user name or password in them are not allowed.".into());
    }
    if url.contains(char::is_whitespace) || url.contains('\\') {
        return Err("The address contains characters it should not.".into());
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6.split_once(']').ok_or("Malformed address.")?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| "Malformed port.")?,
            None if after.is_empty() => default_port,
            None => return Err("Malformed address.".into()),
        };
        (h.to_owned(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p.parse().map_err(|_| "Malformed port.")?),
            None => (authority.to_owned(), default_port),
        }
    };
    if host.is_empty() {
        return Err("The address has no host.".into());
    }
    Ok((host.to_ascii_lowercase(), port))
}

/// Check a provider address before using it: its shape, and, unless `allow_local`, that every address its name
/// resolves to is on the public internet.
pub async fn check_url(url: &str, allow_local: bool) -> Result<(), String> {
    let (host, port) = host_port(url)?;
    if allow_local {
        return Ok(());
    }
    let addrs: Vec<IpAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| format!("{host} could not be found."))?
            .map(|a| a.ip())
            .collect(),
    };
    if addrs.is_empty() {
        return Err(format!("{host} could not be found."));
    }
    if addrs.into_iter().any(is_internal) {
        return Err("This address is on a private network. Tick “local network” if you mean a server like Ollama on your own network.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "255.255.255.255",
            "64:ff9b::a00:1",
        ] {
            assert!(is_internal(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111", "104.18.0.1"] {
            assert!(!is_internal(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn url_shapes() {
        assert_eq!(
            host_port("https://api.openai.com/v1").unwrap(),
            ("api.openai.com".into(), 443)
        );
        assert_eq!(
            host_port("http://LOCALHOST:11434/v1").unwrap(),
            ("localhost".into(), 11434)
        );
        assert_eq!(
            host_port("http://[::1]:8080/v1").unwrap(),
            ("::1".into(), 8080)
        );
        for bad in [
            "ftp://x/",
            "file:///etc/passwd",
            "https://user:pw@api.openai.com/v1",
            "https://api.openai.com@127.0.0.1/",
            "gopher://x",
            "api.openai.com",
            "https:///v1",
            "https://x:99999/",
            "https://a b/",
        ] {
            assert!(host_port(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn names_that_resolve_inside_are_refused() {
        assert!(check_url("http://localhost:11434/v1", false).await.is_err());
        assert!(check_url("http://127.0.0.1/v1", false).await.is_err());
        assert!(
            check_url("http://169.254.169.254/latest/meta-data", false)
                .await
                .is_err()
        );
        assert!(check_url("http://[::ffff:7f00:1]/v1", false).await.is_err());
        assert!(
            check_url("http://localhost:11434/v1", true).await.is_ok(),
            "local on purpose"
        );
        assert!(check_url("https://8.8.8.8/v1", false).await.is_ok());
    }
}
