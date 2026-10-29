//! Relay address normalization (iOS `NostrRelayURL.normalized`).

/// Trims, lowercases scheme and host, maps https/http to wss/ws, drops
/// default ports, a bare `/` path and any fragment. `None` if unusable.
pub fn normalize(raw: &str, default_scheme: Option<&str>) -> Option<String> {
    let mut value = raw.trim().to_string();
    if value.is_empty() {
        return None;
    }
    if !value.contains("://") {
        value = format!("{}://{value}", default_scheme?);
    }
    let (scheme, rest) = value.split_once("://")?;
    let (scheme, default_port) = match scheme.to_ascii_lowercase().as_str() {
        "wss" | "https" => ("wss", "443"),
        "ws" | "http" => ("ws", "80"),
        _ => return None,
    };
    let rest = rest.split('#').next().unwrap_or("");
    let split = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(split);
    if authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (h, Some(p))
        }
        _ => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let mut out = format!("{scheme}://{host}");
    if let Some(p) = port
        && p != default_port
    {
        out.push(':');
        out.push_str(p);
    }
    // A bare "/" path is dropped; any other path or query is kept.
    match tail.strip_prefix('/') {
        Some(after) if after.is_empty() || after.starts_with('?') => out.push_str(after),
        _ => out.push_str(tail),
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_like_ios() {
        assert_eq!(
            normalize(" wss://Relay.Damus.io/ ", None).as_deref(),
            Some("wss://relay.damus.io")
        );
        assert_eq!(
            normalize("https://nos.lol:443", None).as_deref(),
            Some("wss://nos.lol")
        );
        assert_eq!(
            normalize("ws://localhost:8080", None).as_deref(),
            Some("ws://localhost:8080")
        );
        assert_eq!(
            normalize("relay.example.com", Some("wss")).as_deref(),
            Some("wss://relay.example.com")
        );
        assert_eq!(
            normalize("wss://host/path#frag", None).as_deref(),
            Some("wss://host/path")
        );
        assert_eq!(normalize("relay.example.com", None), None);
        assert_eq!(normalize("ftp://x", None), None);
        assert_eq!(normalize("", None), None);
    }
}
