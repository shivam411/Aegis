//! Hostname validation for domains mapped to apps.

/// Normalizes and validates a domain name for the reverse proxy.
///
/// The result is lowercase without a trailing dot. Accepted names follow
/// RFC 1123: at least two labels of 1-63 letters, digits or hyphens (not at
/// either end of a label), at most 253 characters in total. Wildcards, IP
/// addresses, ports and non-ASCII names (use the `xn--` form) are refused.
pub fn normalize_domain(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    let name = trimmed.strip_suffix('.').unwrap_or(trimmed);
    if name.is_empty() {
        return Err("The domain must not be empty".into());
    }
    if name.contains('*') {
        return Err("A wildcard (*) is not allowed here".into());
    }
    if name.contains(':') || name.contains('/') {
        return Err("Give only the domain name, without a scheme, port or path".into());
    }
    if !name.is_ascii() {
        return Err("Internationalized domains must be given in their ASCII form (xn--...)".into());
    }
    let name = name.to_ascii_lowercase();
    if name.len() > 253 {
        return Err("The domain is longer than 253 characters".into());
    }
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() < 2 {
        return Err(format!(
            "'{}' is not a fully qualified domain name (e.g. app.example.com)",
            name
        ));
    }
    for label in &labels {
        if label.is_empty() || label.len() > 63 {
            return Err(format!(
                "'{}' has an empty label or one longer than 63 characters",
                name
            ));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!(
                "'{}' may contain only letters, digits, hyphens and dots",
                name
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!(
                "Labels of '{}' must not start or end with a hyphen",
                name
            ));
        }
    }
    // A numeric top-level label means an IP address (or nonsense).
    if labels
        .last()
        .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(format!(
            "'{}' looks like an IP address; map a domain name",
            name
        ));
    }
    Ok(name)
}

/// Like [`normalize_domain`], but also accepts a wildcard subdomain,
/// `*.example.com`: every name exactly one label below `example.com`.
pub fn normalize_route_domain(input: &str) -> Result<String, String> {
    match input.trim().strip_prefix("*.") {
        Some(base) => normalize_domain(base).map(|b| format!("*.{}", b)),
        None if input.contains('*') => {
            Err("A wildcard must be the whole first label, as in *.example.com".into())
        }
        None => normalize_domain(input),
    }
}

/// The wildcard name that would cover `host`: `a.example.com` gives
/// `*.example.com`. `None` when what is left has fewer than two labels.
pub fn wildcard_of(host: &str) -> Option<String> {
    let (label, base) = host.split_once('.')?;
    (!label.is_empty() && label != "*" && base.contains('.')).then(|| format!("*.{}", base))
}

#[cfg(test)]
mod tests {
    use super::{normalize_domain, normalize_route_domain, wildcard_of};

    #[test]
    fn route_domains_may_be_wildcards() {
        assert_eq!(
            normalize_route_domain(" *.Example.COM. ").unwrap(),
            "*.example.com"
        );
        assert_eq!(
            normalize_route_domain("a.example.com").unwrap(),
            "a.example.com"
        );
        for bad in [
            "*",
            "*.com",
            "a.*.example.com",
            "*a.example.com",
            "*.*.a.com",
        ] {
            assert!(
                normalize_route_domain(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn wildcard_covers_one_label() {
        assert_eq!(
            wildcard_of("a.example.com").as_deref(),
            Some("*.example.com")
        );
        assert_eq!(
            wildcard_of("a.b.example.com").as_deref(),
            Some("*.b.example.com")
        );
        assert_eq!(wildcard_of("example.com"), None);
        assert_eq!(wildcard_of("*.example.com"), None);
    }

    #[test]
    fn accepts_and_normalizes_valid_names() {
        assert_eq!(
            normalize_domain("App.Example.COM").unwrap(),
            "app.example.com"
        );
        assert_eq!(normalize_domain(" example.com. ").unwrap(), "example.com");
        assert_eq!(
            normalize_domain("a-b.c1.example.io").unwrap(),
            "a-b.c1.example.io"
        );
        assert_eq!(
            normalize_domain("xn--bcher-kva.example").unwrap(),
            "xn--bcher-kva.example"
        );
        let long_label = "a".repeat(63);
        assert!(normalize_domain(&format!("{long_label}.example.com")).is_ok());
    }

    #[test]
    fn rejects_invalid_names() {
        for bad in [
            "",
            ".",
            "localhost",
            "*.example.com",
            "example.com:8080",
            "https://example.com",
            "example..com",
            "-example.com",
            "example-.com",
            "exa_mple.com",
            "exa mple.com",
            "bücher.example",
            "10.0.0.1",
            "1.2.3.4.",
            "[::1]",
            "example.com/path",
        ] {
            assert!(normalize_domain(bad).is_err(), "{bad:?} should be rejected");
        }
        let long_label = "a".repeat(64);
        assert!(normalize_domain(&format!("{long_label}.example.com")).is_err());
        let too_long = format!("{}.com", ["abcdefghij"; 25].join("."));
        assert!(too_long.len() > 253);
        assert!(normalize_domain(&too_long).is_err());
    }
}
