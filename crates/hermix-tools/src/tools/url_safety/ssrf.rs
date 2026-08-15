//! SSRF guard: blocks outbound URLs to internal/loopback/link-local ranges.
use serde::{Deserialize, Serialize};

// Server-Side Request Forgery protection for tools that fetch URLs on the
// LLM's behalf (`web_extract`, `browser_navigate`, …). The guard rejects:
// - Schemes other than http/https (file://, gopher://, dict://, ftp://)
// - Loopback (127.0.0.0/8, ::1)
// - Link-local (169.254.0.0/16, fe80::/10) — includes the AWS/GCP/Azure
// instance metadata endpoint at 169.254.169.254
// - RFC 1918 private (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16)
// - IPv6 unique-local (fc00::/7)
// - IPv4-mapped IPv6 (::ffff:0:0/96 — bypasses naive v4-only checks)
// - Multicast (224.0.0.0/4, ff00::/8) and broadcast (255.255.255.255)
// - Reserved 240.0.0.0/4 and unspecified (0.0.0.0, ::)
// Hostname-based checks are done literally for the well-known names
// (`localhost`, `*.internal`, `*.local`, `metadata.google.internal`); DNS
// resolution to IP is the **caller's** responsibility. The HTTP backend is
// the right place to do `lookup_host` + re-check with this guard, and
// blocking the runtime to do a sync `ToSocketAddrs::lookup_host` here
// would tank the agent's response time. The contract is: **IP literals
// are classified here; hostnames pass through with no DNS work done.**

use std::net::{Ipv4Addr, Ipv6Addr};

/// Category of an SSRF block. Stable for telemetry / log parsing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SsrfCategory {
    /// scheme is not http/https (e.g. file://, gopher://, dict://, ftp://)
    ForbiddenScheme,
    /// 127.0.0.0/8 or ::1
    Loopback,
    /// 169.254.0.0/16 (cloud metadata!) or fe80::/10
    LinkLocal,
    /// 10/8, 172.16/12, 192.168/16, fc00::/7, *.internal, *.local
    PrivateNetwork,
    /// 224.0.0.0/4 or ff00::/8
    Multicast,
    /// 240.0.0.0/4 (excl. broadcast)
    Reserved,
    /// 0.0.0.0 or ::
    UnspecifiedAddress,
    ///ffff:0:0/96 — caller wrapped a private v4 in v6 to bypass checks
    Ipv4MappedIpv6,
}

/// A URL the SSRF guard refused to allow.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SsrfViolation {
    pub url: String,
    pub reason: String,
    pub category: SsrfCategory,
}

impl std::fmt::Display for SsrfViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SSRF block ({}): {}", self.category, self.reason)
    }
}

impl std::error::Error for SsrfViolation {}

impl std::fmt::Display for SsrfCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ForbiddenScheme => "forbidden_scheme",
            Self::Loopback => "loopback",
            Self::LinkLocal => "link_local",
            Self::PrivateNetwork => "private_network",
            Self::Multicast => "multicast",
            Self::Reserved => "reserved",
            Self::UnspecifiedAddress => "unspecified",
            Self::Ipv4MappedIpv6 => "ipv4_mapped_ipv6",
        })
    }
}

/// Stateless SSRF guard. Cheap to construct (`SsrfGuard::new()`) and
/// cheap to call (`check(url)`). Currently empty; the rules are encoded
/// in [`SsrfGuard::check`] and [`SsrfGuard::classify_ip`]. Wrap in an
/// `Arc` if you want to share across handlers.
#[derive(Debug, Clone, Copy, Default)]
pub struct SsrfGuard;

impl SsrfGuard {
    pub fn new() -> Self {
        Self
    }

    /// Check a URL against the SSRF blocklist.
    /// Returns `Ok(())` if the URL is safe to dispatch to a fetcher,
    /// `Err(SsrfViolation)` otherwise. **No DNS resolution is
    /// performed** — hostnames pass through; IP literals are
    /// classified. See the module-level docs.
    pub fn check(&self, url: &str) -> Result<(), SsrfViolation> {
        let parsed = url::Url::parse(url).map_err(|e| SsrfViolation {
            url: url.to_string(),
            reason: format!("unparseable URL: {}", e),
            category: SsrfCategory::ForbiddenScheme,
        })?;

        // 1. Scheme gate.
        let scheme = parsed.scheme();
        if !matches!(scheme, "http" | "https") {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("scheme `{}` is not allowed; only http/https", scheme),
                category: SsrfCategory::ForbiddenScheme,
            });
        }

        // 2. Host classification. `url::Host` is a typed enum, so we
        // don't have to round-trip through `host_str().parse()` —
        // that path was unreliable for IPv6 (the `url` crate
        // historically returned the bracketed form, which doesn't
        // parse as `IpAddr`).
        let host = parsed.host().ok_or_else(|| SsrfViolation {
            url: url.to_string(),
            reason: "URL has no host".into(),
            category: SsrfCategory::ForbiddenScheme,
        })?;

        match host {
            url::Host::Ipv4(v4) => Self::classify_v4(url, v4)?,
            url::Host::Ipv6(v6) => {
                // IPv4-mapped IPv6: ::ffff:a.b.c.d — always a
                // violation. The category says the wrapper is
                // Ipv4MappedIpv6; the reason names the inner v4 so
                // the user knows which private IP was being smuggled.
                if let Some(v4) = v6.to_ipv4_mapped() {
                    return Err(SsrfViolation {
                        url: url.to_string(),
                        reason: format!(
                            "IPv4-mapped IPv6 wraps {} (RFC 4291 §2.5.5.2 bypass attempt)",
                            v4
                        ),
                        category: SsrfCategory::Ipv4MappedIpv6,
                    });
                }
                Self::classify_v6(url, v6)?;
            }
            url::Host::Domain(d) => {
                // 3a. Hostname literal checks (no DNS).
                let lower = d.to_ascii_lowercase();
                if matches!(lower.as_str(), "localhost" | "ip6-localhost" | "ip6-loopback") {
                    return Err(SsrfViolation {
                        url: url.to_string(),
                        reason: format!("hostname `{}` is loopback", d),
                        category: SsrfCategory::Loopback,
                    });
                }
                if lower == "metadata.google.internal"
                    || lower.ends_with(".internal")
                    || lower.ends_with(".local")
                {
                    return Err(SsrfViolation {
                        url: url.to_string(),
                        reason: format!("hostname `{}` is an internal/mDNS name", d),
                        category: SsrfCategory::PrivateNetwork,
                    });
                }
                // 3b. Hostname passes through. DNS resolution +
                // re-check on the resolved IP is the **caller's**
                // responsibility (see module-level docs).
            }
        }

        Ok(())
    }

    /// Classify a single IPv4 address against the SSRF blocklist.
    /// Public so HTTP backends that do their own DNS resolution can
    /// re-check the resolved IP with the same rules `check` applies
    /// to literal IPv4 hosts in URLs.
    pub fn classify_v4(url: &str, v4: Ipv4Addr) -> Result<(), SsrfViolation> {
        if v4.is_unspecified() {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: "0.0.0.0 is the unspecified address".into(),
                category: SsrfCategory::UnspecifiedAddress,
            });
        }
        if v4.is_loopback() {
            // 127.0.0.0/8
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is loopback (127.0.0.0/8)", v4),
                category: SsrfCategory::Loopback,
            });
        }
        if v4.is_link_local() {
            // 169.254.0.0/16 — includes AWS/GCP/Azure metadata at
            // 169.254.169.254. THIS IS THE ONE THAT MATTERS MOST.
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!(
                    "{} is link-local (169.254.0.0/16) — cloud metadata lives here",
                    v4
                ),
                category: SsrfCategory::LinkLocal,
            });
        }
        if v4.is_private() {
            // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is RFC 1918 private", v4),
                category: SsrfCategory::PrivateNetwork,
            });
        }
        if v4.is_multicast() {
            // 224.0.0.0/4
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is multicast (224.0.0.0/4)", v4),
                category: SsrfCategory::Multicast,
            });
        }
        if v4.is_broadcast() {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is broadcast", v4),
                category: SsrfCategory::Multicast,
            });
        }
        let o = v4.octets();
        if (o[0] & 0xF0) == 0xF0 {
            // 240.0.0.0/4 (reserved for future use) — broadcast was
            // already caught above
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is in the reserved 240.0.0.0/4 range", v4),
                category: SsrfCategory::Reserved,
            });
        }
        Ok(())
    }

    fn classify_v6(url: &str, v6: Ipv6Addr) -> Result<(), SsrfViolation> {
        if v6.is_unspecified() {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: ":: is the unspecified address".into(),
                category: SsrfCategory::UnspecifiedAddress,
            });
        }
        if v6.is_loopback() {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is IPv6 loopback (::1)", v6),
                category: SsrfCategory::Loopback,
            });
        }
        // fe80::/10 — link-local. `Ipv6Addr::is_link_local` is still
        // unstable (tracking rust-lang/rust#27709) as of rustc 1.92, so
        // we hand-roll the mask: first 10 bits must be `1111_1110_10`.
        let s = v6.segments();
        if (s[0] & 0xFFC0) == 0xFE80 {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is IPv6 link-local (fe80::/10)", v6),
                category: SsrfCategory::LinkLocal,
            });
        }
        // fc00::/7 — unique-local. Stable since rustc 1.80.
        if v6.is_unique_local() {
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is IPv6 unique-local (fc00::/7)", v6),
                category: SsrfCategory::PrivateNetwork,
            });
        }
        if v6.is_multicast() {
            // ff00::/8
            return Err(SsrfViolation {
                url: url.to_string(),
                reason: format!("{} is IPv6 multicast (ff00::/8)", v6),
                category: SsrfCategory::Multicast,
            });
        }
        Ok(())
    }
}

