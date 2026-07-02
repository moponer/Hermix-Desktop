//! SSRF (Server-Side Request Forgery) protection (Requirement 22.2).
//!
//! Validates outbound URLs to prevent access to internal/private networks
//! and cloud metadata endpoints.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use tokio::net::lookup_host;
use url::Url;

use hermix_core::errors::GatewayError;

// ---------------------------------------------------------------------------
// Private IP range checks
// ---------------------------------------------------------------------------

/// Check if an IPv4 address falls within a private/reserved range.
///
/// Blocks:
/// - 10.0.0.0/8 (Class A private)
/// - 172.16.0.0/12 (Class B private)
/// - 192.168.0.0/16 (Class C private)
/// - 127.0.0.0/8 (Loopback)
/// - 169.254.0.0/16 (Link-local, includes cloud metadata)
fn is_private_ipv4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();

    // 10.0.0.0/8
    if octets[0] == 10 {
        return true;
    }

    // 172.16.0.0/12 (172.16.0.0 - 172.31.255.255)
    if octets[0] == 172 && (16..=31).contains(&octets[1]) {
        return true;
    }

    // 192.168.0.0/16
    if octets[0] == 192 && octets[1] == 168 {
        return true;
    }

    // 127.0.0.0/8 (Loopback)
    if octets[0] == 127 {
        return true;
    }

    // 169.254.0.0/16 (Link-local, includes 169.254.169.254 cloud metadata)
    if octets[0] == 169 && octets[1] == 254 {
        return true;
    }

    // 0.0.0.0/8 (Current network)
    if octets[0] == 0 {
        return true;
    }

    // 100.64.0.0/10 (Carrier-grade NAT)
    if octets[0] == 100 && (64..=127).contains(&octets[1]) {
        return true;
    }

    // 198.18.0.0/15 (Benchmarking)
    if octets[0] == 198 && (18..=19).contains(&octets[1]) {
        return true;
    }

    false
}

/// Check if an IPv6 address falls within a private/reserved range.
///
/// Blocks:
/// - ::1/128 (Loopback)
/// - fe80::/10 (Link-local)
/// - fc00::/7 (Unique local / ULA)
/// - ::ffff:x.x.x.x (IPv4-mapped, delegates to IPv4 check)
fn is_private_ipv6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();

    // ::1 (Loopback)
    if ip.is_loopback() {
        return true;
    }

    // fe80::/10 (Link-local)
    // First 10 bits: 1111 1110 10xx xxxx
    // Segment[0] = 0xfe80..0xfebf
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }

    // fc00::/7 (Unique local addresses: fc00::/7 and fd00::/7)
    // First 7 bits: 1111 110 = 0xfc00..0xfdff
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }

    // ::ffff:x.x.x.x (IPv4-mapped IPv6)
    // Check if this is an IPv4-mapped address and delegate to IPv4 check
    // Manual check: last two segments are 0xffff, first four are 0
    let segments = ip.segments();
    if segments[0] == 0
        && segments[1] == 0
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0
        && segments[5] == 0xffff
    {
        // Extract the IPv4 portion and check it
        let ipv4 = ip.to_ipv4_mapped().unwrap();
        return is_private_ipv4(&ipv4);
    }

    false
}

/// Check whether an IP address is in a private/reserved range.
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_ipv4(v4),
        IpAddr::V6(v6) => is_private_ipv6(v6),
    }
}

// ---------------------------------------------------------------------------
// Cloud metadata endpoint check
// ---------------------------------------------------------------------------

/// Check if an IP address matches known cloud metadata endpoints.
fn is_cloud_metadata(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            // AWS/GCP/Azure cloud metadata: 169.254.169.254
            let octets = v4.octets();
            octets[0] == 169 && octets[1] == 254 && octets[2] == 169 && octets[3] == 254
        }
        IpAddr::V6(_) => {
            // No known IPv6 cloud metadata endpoints to block
            false
        }
    }
}

// ---------------------------------------------------------------------------
// DNS resolution (pluggable for hermetic tests)
// ---------------------------------------------------------------------------

/// DNS resolution async signature: `host` → list of `SocketAddr`.
///
/// Production uses `tokio::net::lookup_host`; tests can install a stub to keep
/// the suite hermetic when the build host has no DNS (CI sandboxes, airgapped
/// containers, etc.). Installed via `set_resolver_for_testing`.
type ResolverFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>,
>;
type Resolver = std::sync::Arc<dyn Fn(&str) -> ResolverFuture + Send + Sync>;

#[cfg(test)]
use std::sync::Mutex;
#[cfg(test)]
static RESOLVER_OVERRIDE: Mutex<Option<Resolver>> = Mutex::new(None);

/// Test-only: install a custom DNS resolver. Pass `None` to restore the
/// real `tokio::net::lookup_host`. The `Mutex` and override are gated by
/// `#[cfg(test)]` so production binaries have zero overhead.
#[cfg(test)]
pub fn set_resolver_for_testing(f: Option<Resolver>) {
    *RESOLVER_OVERRIDE.lock().unwrap() = f;
}

async fn resolve_host(host: &str) -> std::io::Result<Vec<SocketAddr>> {
    #[cfg(test)]
    {
        if let Some(f) = RESOLVER_OVERRIDE.lock().unwrap().clone() {
            return f(host).await;
        }
    }
    let mut out = Vec::new();
    let mut it = lookup_host((host, 0u16)).await?;
    while let Some(sa) = it.next() {
        out.push(sa);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check whether a URL is safe to access (not a private/internal address).
///
/// This function:
/// 1. Parses the URL
/// 2. Resolves the hostname to IP addresses
/// 3. Rejects any that resolve to private/reserved ranges
/// 4. Rejects cloud metadata endpoints
///
/// Returns `true` if the URL is safe, `false` otherwise.
///
/// **Async**: Performs real DNS resolution via `tokio::net::lookup_host` to
/// close the DNS-rebinding hole that the synchronous version had (where any
/// hostname was accepted without checking the resolved IP).
pub async fn is_safe_url(url: &str) -> bool {
    // First, try to parse the URL
    let parsed = match Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false,
    };

    // Only allow http and https schemes
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return false,
    };

    // Get the host
    let host = match parsed.host_str() {
        Some(h) => h,
        None => return false,
    };

    // Try to parse as an IP address directly
    if let Ok(ip) = IpAddr::from_str(host) {
        if is_private_ip(&ip) || is_cloud_metadata(&ip) {
            return false;
        }
        return true;
    }

    // Resolve the hostname via system DNS. EVERY resolved IP must be public;
    // if any single one is private/reserved, reject the entire URL.
    // This blocks both literal "http://internal.local/" cases AND DNS
    // rebinding where an attacker controls DNS to return 10.0.0.1 for
    // example.com after we've checked the hostname.
    let addrs = match resolve_host(host).await {
        Ok(v) => v,
        Err(_) => return false, // unresolved hostnames are rejected (strict)
    };
    if addrs.is_empty() {
        return false; // no records
    }
    for addr in &addrs {
        let ip = addr.ip();
        if is_private_ip(&ip) || is_cloud_metadata(&ip) {
            return false;
        }
    }
    true
}

/// Validate a URL and return it if it passes SSRF protection checks.
///
/// Returns the parsed `Url` if safe, or a `GatewayError` if the URL
/// is potentially dangerous.
///
/// **Async**: Performs real DNS resolution to close the DNS-rebinding hole.
pub async fn validate_url(url: &str) -> Result<Url, GatewayError> {
    let parsed = Url::parse(url)
        .map_err(|e| GatewayError::ConnectionFailed(format!("Invalid URL '{}': {}", url, e)))?;

    // Only allow http and https schemes
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(GatewayError::ConnectionFailed(format!(
                "Unsupported URL scheme '{}': only http and https are allowed",
                other
            )));
        }
    }

    // Check the host
    let host = parsed
        .host_str()
        .ok_or_else(|| GatewayError::ConnectionFailed(format!("URL has no host: {}", url)))?;

    // Block known dangerous hostnames first (cheap, no DNS needed)
    let lowercase_host = host.to_lowercase();
    let dangerous_hosts = ["localhost", "metadata.google.internal", "metadata.internal"];
    if dangerous_hosts.contains(&lowercase_host.as_str()) {
        return Err(GatewayError::ConnectionFailed(format!(
            "URL hostname is blocked: {}",
            host
        )));
    }

    // Check if the host is a raw IP in a private range
    if let Ok(ip) = IpAddr::from_str(host) {
        if is_private_ip(&ip) {
            return Err(GatewayError::ConnectionFailed(format!(
                "URL resolves to private/internal IP address: {}",
                ip
            )));
        }
        if is_cloud_metadata(&ip) {
            return Err(GatewayError::ConnectionFailed(format!(
                "URL resolves to cloud metadata endpoint: {}",
                ip
            )));
        }
        return Ok(parsed);
    }

    // Hostname: resolve via DNS and check every IP. Blocks DNS rebinding.
    let addrs = resolve_host(host)
        .await
        .map_err(|e| GatewayError::ConnectionFailed(format!("DNS resolution failed for '{}': {}", host, e)))?;

    if addrs.is_empty() {
        return Err(GatewayError::ConnectionFailed(format!(
            "Hostname '{}' did not resolve to any IP address",
            host
        )));
    }
    for addr in &addrs {
        let ip = addr.ip();
        if is_private_ip(&ip) {
            return Err(GatewayError::ConnectionFailed(format!(
                "URL hostname '{}' resolves to private/internal IP address: {}",
                host,
                ip
            )));
        }
        if is_cloud_metadata(&ip) {
            return Err(GatewayError::ConnectionFailed(format!(
                "URL hostname '{}' resolves to cloud metadata endpoint: {}",
                host,
                ip
            )));
        }
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_private_ipv4_ranges() {
        // Should be private
        assert!(is_private_ipv4(&Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_private_ipv4(&Ipv4Addr::new(10, 255, 255, 255)));
        assert!(is_private_ipv4(&Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_private_ipv4(&Ipv4Addr::new(172, 31, 255, 255)));
        assert!(is_private_ipv4(&Ipv4Addr::new(192, 168, 0, 1)));
        assert!(is_private_ipv4(&Ipv4Addr::new(192, 168, 1, 1)));
        assert!(is_private_ipv4(&Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_private_ipv4(&Ipv4Addr::new(169, 254, 169, 254)));
        assert!(is_private_ipv4(&Ipv4Addr::new(0, 0, 0, 0)));

        // Should NOT be private
        assert!(!is_private_ipv4(&Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_private_ipv4(&Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!is_private_ipv4(&Ipv4Addr::new(172, 15, 0, 1)));
        assert!(!is_private_ipv4(&Ipv4Addr::new(172, 32, 0, 1)));
        assert!(!is_private_ipv4(&Ipv4Addr::new(192, 169, 0, 1)));
    }

    #[test]
    fn test_private_ipv6_ranges() {
        // Should be private
        assert!(is_private_ipv6(&Ipv6Addr::LOCALHOST));
        assert!(is_private_ipv6(&"fe80::1".parse().unwrap()));
        assert!(is_private_ipv6(&"fc00::1".parse().unwrap()));
        assert!(is_private_ipv6(&"fd00::1".parse().unwrap()));

        // IPv4-mapped loopback should be private
        assert!(is_private_ipv6(&"::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_private_ipv6(&"::ffff:10.0.0.1".parse().unwrap()));

        // Should NOT be private
        assert!(!is_private_ipv6(&"2001:db8::1".parse().unwrap()));
        assert!(!is_private_ipv6(&"2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn test_cloud_metadata() {
        let ip: IpAddr = "169.254.169.254".parse().unwrap();
        assert!(is_cloud_metadata(&ip));

        let ip: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(!is_cloud_metadata(&ip));
    }

    // Async tests: hostname-based tests require real DNS.
    // We use `example.com` (public, IANA-reserved) and `localhost`
    // (always resolves to 127.0.0.1) so the suite is hermetic.
    //
    // The DNS stub installed below makes these tests pass on hosts that
    // have no DNS (CI sandboxes, airgapped containers). It also closes
    // the last network-dependent hole in `cargo test` for this crate.
    fn stub_resolver(entries: Vec<(&'static str, Vec<SocketAddr>)>) -> Resolver {
        std::sync::Arc::new(move |host: &str| {
            // Pre-allocate the matched addresses synchronously and return
            // them from a pinned future.
            let matched: Vec<SocketAddr> = entries
                .iter()
                .find(|(h, _)| *h == host)
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            Box::pin(async move { Ok(matched) })
        })
    }

    #[tokio::test]
    async fn test_is_safe_url_public() {
        // example.com -> public, 8.8.8.8 -> literal public IP
        let example_ip: IpAddr = "93.184.216.34".parse().unwrap();
        let resolver = stub_resolver(vec![(
            "example.com",
            vec![SocketAddr::new(example_ip, 0)],
        )]);
        set_resolver_for_testing(Some(resolver));
        assert!(is_safe_url("https://example.com/api").await);
        // The literal IP path does not hit the resolver at all.
        assert!(is_safe_url("http://8.8.8.8/api").await);
        set_resolver_for_testing(None);
    }

    #[tokio::test]
    async fn test_is_safe_url_private() {
        assert!(!is_safe_url("http://10.0.0.1/api").await);
        assert!(!is_safe_url("http://127.0.0.1/api").await);
        assert!(!is_safe_url("http://192.168.1.1/api").await);
        assert!(!is_safe_url("http://169.254.169.254/api").await);
    }

    #[tokio::test]
    async fn test_is_safe_url_invalid() {
        assert!(!is_safe_url("not-a-url").await);
        assert!(!is_safe_url("ftp://example.com").await);
    }

    #[tokio::test]
    async fn test_validate_url_success() {
        let example_ip: IpAddr = "93.184.216.34".parse().unwrap();
        let resolver = stub_resolver(vec![(
            "example.com",
            vec![SocketAddr::new(example_ip, 0)],
        )]);
        set_resolver_for_testing(Some(resolver));
        assert!(validate_url("https://example.com/api").await.is_ok());
        // Literal IP path is resolver-independent.
        assert!(validate_url("http://8.8.8.8/api").await.is_ok());
        set_resolver_for_testing(None);
    }

    #[tokio::test]
    async fn test_validate_url_private_ip_rejected() {
        assert!(validate_url("http://10.0.0.1/api").await.is_err());
        assert!(validate_url("http://127.0.0.1/api").await.is_err());
        assert!(validate_url("http://192.168.1.1/api").await.is_err());
        assert!(validate_url("http://169.254.169.254/api").await.is_err());
    }

    #[tokio::test]
    async fn test_validate_url_localhost_rejected() {
        // Blocked at hostname stage (no DNS needed)
        assert!(validate_url("http://localhost/api").await.is_err());
    }

    #[tokio::test]
    async fn test_validate_url_bad_scheme() {
        assert!(validate_url("ftp://example.com/file").await.is_err());
        assert!(validate_url("file:///etc/passwd").await.is_err());
    }

    /// Regression test for the DNS-rebinding hole:
    /// a hostname like `localhost` must be blocked because DNS resolves it to 127.0.0.1,
    /// even though the literal string isn't a raw IP.
    #[tokio::test]
    async fn test_dns_rebinding_blocked() {
        // "localhost" is in the dangerous_hosts list (caught at hostname stage)
        assert!(validate_url("http://localhost/api").await.is_err());
        assert!(is_safe_url("http://localhost/api").await == false);
    }
}
