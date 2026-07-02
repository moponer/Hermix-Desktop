//! Property 11: SSRF protection blocks private IPs
//! **Validates: Requirement 22.2**
//!
//! For any private/reserved IPv4 address in a URL, is_safe_url returns false.
//! For any public IPv4 address in an HTTP/HTTPS URL, is_safe_url returns true.
//!
//! Now async: `is_safe_url` performs real DNS resolution to close the
//! DNS-rebinding hole. All IP inputs are literals so no real DNS is hit.

use hermix_gateway::is_safe_url;

// ---------------------------------------------------------------------------
// Helpers: sample N random IPs from a given octet range predicate.
// ---------------------------------------------------------------------------

use rand::Rng;

fn sample_private_ipv4() -> String {
    let mut rng = rand::thread_rng();
    let kind = rng.gen_range(0..5);
    match kind {
        0 => format!("10.{}.{}.{}", rng.gen::<u8>(), rng.gen::<u8>(), rng.gen::<u8>()),
        1 => {
            let b: u8 = rng.gen_range(16..=31);
            format!("172.{}.{}.{}", b, rng.gen::<u8>(), rng.gen::<u8>())
        }
        2 => format!("192.168.{}.{}", rng.gen::<u8>(), rng.gen::<u8>()),
        3 => format!("127.{}.{}.{}", rng.gen::<u8>(), rng.gen::<u8>(), rng.gen::<u8>()),
        _ => format!("169.254.{}.{}", rng.gen::<u8>(), rng.gen::<u8>()),
    }
}

fn sample_public_ipv4() -> String {
    // Hardcoded well-known public IPs to keep the test hermetic and stable.
    const PUBLICS: &[&str] = &[
        "8.8.8.8", "8.8.4.4", // Google DNS
        "1.1.1.1", "1.0.0.1", // Cloudflare
        "208.67.222.222", "208.67.220.220", // OpenDNS
        "9.9.9.9", "149.112.112.112", // Quad9
        "140.205.94.189", // Alibaba DNS
    ];
    let mut rng = rand::thread_rng();
    PUBLICS[rng.gen_range(0..PUBLICS.len())].to_string()
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

const CASES: usize = 100;

#[tokio::test]
async fn prop_private_ip_blocked() {
    for i in 0..CASES {
        let ip = sample_private_ipv4();
        let url = format!("http://{ip}/api");
        let result = is_safe_url(&url).await;
        assert!(
            !result,
            "[case {i}] Private IP {ip} should be blocked but is_safe_url returned true"
        );
    }
}

#[tokio::test]
async fn prop_public_ip_allowed() {
    for i in 0..CASES {
        let ip = sample_public_ipv4();
        let url = format!("https://{ip}/api");
        let result = is_safe_url(&url).await;
        assert!(
            result,
            "[case {i}] Public IP {ip} should be allowed but is_safe_url returned false"
        );
    }
}
