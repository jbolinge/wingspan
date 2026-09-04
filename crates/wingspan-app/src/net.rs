//! Who is on the other end of the socket — the one question the LAN gate
//! (`[server] lan_only`) turns into a yes or a no.
//!
//! There are exactly two sources for a client address and they are not equally
//! trustworthy:
//!
//! * the **TCP peer**, which the kernel reports and nobody can forge, and
//! * **`X-Forwarded-For`**, which is just a header — anyone who can reach the
//!   port can write whatever they like in it.
//!
//! So the header is read only when the peer is one of the `trusted_proxies`
//! the operator named. For every other peer the header is ignored outright: a
//! host on the internet cannot promise it is on the LAN. When the peer *is* a
//! trusted proxy the chain is walked **right to left** — the rightmost entry
//! is the one the nearest proxy appended and therefore the only one it
//! vouches for — skipping further trusted proxies, and the first address that
//! is not ours is the client. Everything the walk cannot make sense of is a
//! refusal, never a guess: a proxy that forwards nothing, or a chain with
//! rubbish in it, means we do not know who is calling, and "we do not know" is
//! not "let them in".
//!
//! IPv4-mapped IPv6 (`::ffff:192.168.1.20`, what a dual-stack listener reports
//! for an IPv4 connection) is folded back to IPv4 everywhere before any
//! comparison — otherwise `192.168.0.0/16` would not match the very peers it
//! exists for.

use std::net::IpAddr;
use std::net::SocketAddr;

use ipnet::IpNet;

/// The blocks `[server] lan_networks` means when it is left empty: RFC 1918,
/// loopback, IPv6 link-local and unique-local.
///
/// Deliberately **not** including `100.64.0.0/10` — carrier-grade NAT, which
/// is also what Tailscale hands out. It is a fine network to trust, but it is
/// somebody else's decision to make, so it goes in the config rather than in
/// this list.
pub const PRIVATE_NETWORKS: [&str; 7] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "127.0.0.0/8",
    "::1/128",
    "fe80::/10",
    "fc00::/7",
];

/// [`PRIVATE_NETWORKS`], parsed.
///
/// # Panics
/// Never: the constants above are checked by a unit test in this module.
#[must_use]
pub fn private_networks() -> Vec<IpNet> {
    PRIVATE_NETWORKS
        .iter()
        .filter_map(|text| text.parse().ok())
        .collect()
}

/// Parse a list of IPs and CIDR blocks. A bare address is its own /32 (/128),
/// which is what an operator writing a single HAProxy address means.
///
/// The error is the offending entry, so the caller can name the config key.
pub fn parse_networks(entries: &[String]) -> Result<Vec<IpNet>, String> {
    entries
        .iter()
        .map(|raw| {
            let text = raw.trim();
            if let Ok(net) = text.parse::<IpNet>() {
                return Ok(net);
            }
            text.parse::<IpAddr>()
                .map(IpNet::from)
                .map_err(|_| raw.clone())
        })
        .collect()
}

/// Fold an IPv4-mapped IPv6 address back to the IPv4 address it carries.
#[must_use]
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

/// Whether `ip` falls in any of `nets` (both canonicalised first).
#[must_use]
pub fn contains(nets: &[IpNet], ip: IpAddr) -> bool {
    let ip = canonical_ip(ip);
    nets.iter().any(|net| match (net, ip) {
        (IpNet::V4(net), IpAddr::V4(ip)) => net.contains(&ip),
        (IpNet::V6(net), IpAddr::V6(ip)) => net.contains(&ip),
        _ => false,
    })
}

/// Why a forwarded chain yielded no client address. Both are refusals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardedError {
    /// A trusted proxy forwarded nothing usable — it is misconfigured, and we
    /// will not fall back to the proxy's own address and call it the client.
    Missing,
    /// The chain holds something that is not an address, so every entry to its
    /// left is unattributable. The entry, truncated for the log line.
    Unreadable(String),
}

/// The longest fragment of a header we will repeat back in an error.
const ECHO_LIMIT: usize = 60;

/// Resolve the client address behind `peer`.
///
/// `forwarded` is every `X-Forwarded-For` header value, in the order they
/// arrived; each may itself hold a comma-separated list.
pub fn resolve_client(
    peer: IpAddr,
    forwarded: &[&str],
    trusted: &[IpNet],
) -> Result<IpAddr, ForwardedError> {
    let peer = canonical_ip(peer);
    if !contains(trusted, peer) {
        // Not a proxy we know: the socket is the only thing worth believing.
        return Ok(peer);
    }
    let entries: Vec<&str> = forwarded
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .collect();
    for raw in entries.iter().rev() {
        if raw.is_empty() {
            continue;
        }
        match parse_forwarded_entry(raw) {
            // Another hop of ours: keep walking left towards the client.
            Some(ip) if contains(trusted, ip) => {}
            Some(ip) => return Ok(ip),
            None => return Err(ForwardedError::Unreadable(truncate(raw))),
        }
    }
    Err(ForwardedError::Missing)
}

/// One `X-Forwarded-For` entry as an address.
///
/// Written by hand rather than `parse::<IpAddr>()` alone because real proxies
/// emit ports (`10.0.0.9:53124`, `[2001:db8::1]:443`) and RFC 7239's
/// obfuscated identifiers (`_hidden`, `unknown`) — the first two are an
/// address we can use, the last two are not, and telling them apart is the
/// whole job.
#[must_use]
pub fn parse_forwarded_entry(raw: &str) -> Option<IpAddr> {
    let text = raw.trim().trim_matches('"').trim();
    if text.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix('[') {
        // `[2001:db8::1]` or `[2001:db8::1]:443`.
        let inner = rest.split(']').next()?;
        return inner.parse::<IpAddr>().ok().map(canonical_ip);
    }
    // A bare literal, v4 or v6 — this is the common case and the only one
    // that can hold more than one colon.
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Some(canonical_ip(ip));
    }
    if let Ok(sock) = text.parse::<SocketAddr>() {
        return Some(canonical_ip(sock.ip()));
    }
    None
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= ECHO_LIMIT {
        return text.to_string();
    }
    let head: String = text.chars().take(ECHO_LIMIT).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nets(entries: &[&str]) -> Vec<IpNet> {
        parse_networks(&entries.iter().map(ToString::to_string).collect::<Vec<_>>())
            .expect("test networks parse")
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("an address")
    }

    #[test]
    fn the_builtin_private_set_parses_and_covers_the_usual_suspects() {
        let private = private_networks();
        assert_eq!(private.len(), PRIVATE_NETWORKS.len());
        for inside in [
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.20",
            "127.0.0.1",
            "::1",
            "fe80::1",
            "fd00::5",
        ] {
            assert!(contains(&private, ip(inside)), "{inside}");
        }
        for outside in ["203.0.113.5", "172.32.0.1", "8.8.8.8", "2001:db8::1"] {
            assert!(!contains(&private, ip(outside)), "{outside}");
        }
        // Tailscale/CGNAT is deliberately not in the default set.
        assert!(!contains(&private, ip("100.64.1.1")));
        assert!(contains(&nets(&["100.64.0.0/10"]), ip("100.64.1.1")));
    }

    #[test]
    fn a_bare_address_is_its_own_host_route_and_junk_is_named() {
        let parsed = nets(&["192.168.1.9", " 10.0.0.0/8 ", "2001:db8::1"]);
        assert!(contains(&parsed, ip("192.168.1.9")));
        assert!(!contains(&parsed, ip("192.168.1.10")));
        assert!(contains(&parsed, ip("10.9.9.9")));
        assert!(contains(&parsed, ip("2001:db8::1")));

        let bad = parse_networks(&["10.0.0.0/33".to_string()]).unwrap_err();
        assert_eq!(bad, "10.0.0.0/33");
        assert_eq!(
            parse_networks(&["not-an-ip".to_string()]).unwrap_err(),
            "not-an-ip"
        );
    }

    /// A dual-stack listener reports every IPv4 connection this way.
    #[test]
    fn an_ipv4_mapped_peer_is_an_ipv4_address() {
        assert_eq!(canonical_ip(ip("::ffff:192.168.1.20")), ip("192.168.1.20"));
        assert_eq!(canonical_ip(ip("::1")), ip("::1"));
        assert!(contains(&private_networks(), ip("::ffff:192.168.1.20")));
        assert_eq!(
            parse_forwarded_entry("::ffff:203.0.113.5"),
            Some(ip("203.0.113.5"))
        );
    }

    #[test]
    fn a_header_from_a_peer_we_do_not_know_is_not_even_read() {
        let trusted = nets(&["10.9.9.9"]);
        // The spoof the gate exists to stop: a WAN client claiming the LAN.
        assert_eq!(
            resolve_client(ip("203.0.113.5"), &["192.168.1.20"], &trusted),
            Ok(ip("203.0.113.5"))
        );
        // And rubbish from such a peer is not an error either — it is ignored.
        assert_eq!(
            resolve_client(ip("192.168.1.20"), &["nonsense"], &trusted),
            Ok(ip("192.168.1.20"))
        );
    }

    #[test]
    fn a_trusted_proxy_is_believed_only_about_what_it_appended() {
        let trusted = nets(&["10.9.9.9", "10.9.9.10"]);
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &["192.168.1.20"], &trusted),
            Ok(ip("192.168.1.20"))
        );
        // HAProxy set to *append*: the client wrote a LAN address, the proxy
        // added the address it really saw. Rightmost wins, so the lie loses.
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &["192.168.1.20, 203.0.113.5"], &trusted),
            Ok(ip("203.0.113.5"))
        );
        // Two proxies of ours in the chain: skip both, take the client.
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &["192.168.1.20, 10.9.9.10"], &trusted),
            Ok(ip("192.168.1.20"))
        );
        // Repeated headers are one list, in arrival order.
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &["192.168.1.20", "10.9.9.10"], &trusted),
            Ok(ip("192.168.1.20"))
        );
        // Ports, brackets and quotes are all things real proxies emit.
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &["\"192.168.1.20:53124\""], &trusted),
            Ok(ip("192.168.1.20"))
        );
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &["[fd00::5]:443"], &trusted),
            Ok(ip("fd00::5"))
        );
    }

    #[test]
    fn a_proxy_that_forwards_nothing_usable_is_refused() {
        let trusted = nets(&["10.9.9.9"]);
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &[], &trusted),
            Err(ForwardedError::Missing)
        );
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &["  ", ", ,"], &trusted),
            Err(ForwardedError::Missing)
        );
        // A chain that is nothing but our own proxies names no client.
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &["10.9.9.9"], &trusted),
            Err(ForwardedError::Missing)
        );
        // Rubbish stops the walk where it stands, rather than being stepped
        // over to reach something an attacker put to its left.
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &["192.168.1.20, unknown"], &trusted),
            Err(ForwardedError::Unreadable("unknown".into()))
        );
        assert_eq!(
            resolve_client(ip("10.9.9.9"), &["_secret"], &trusted),
            Err(ForwardedError::Unreadable("_secret".into()))
        );
    }

    #[test]
    fn an_echoed_entry_is_bounded() {
        let trusted = nets(&["10.9.9.9"]);
        let long = "x".repeat(500);
        let Err(ForwardedError::Unreadable(echoed)) =
            resolve_client(ip("10.9.9.9"), &[long.as_str()], &trusted)
        else {
            panic!("a 500-character entry is not an address");
        };
        assert_eq!(echoed.chars().count(), ECHO_LIMIT + 1, "{echoed}");
    }

    #[test]
    fn with_no_trusted_proxies_the_socket_is_the_only_answer() {
        assert_eq!(
            resolve_client(ip("192.168.1.20"), &["203.0.113.5"], &[]),
            Ok(ip("192.168.1.20"))
        );
    }
}
