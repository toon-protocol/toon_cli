//! A relay URL, `ws://` or `wss://`: the one place that says what scheme, host and port
//! a relay is dialled at, and where its information document is served.

/// A relay URL taken apart. The relay is always named as it was given; this is only how it
/// is dialled.
#[derive(Debug, PartialEq)]
pub struct RelayUrl {
    /// Whether the websocket runs inside TLS (`wss://`).
    pub tls: bool,
    /// The host name or address, without the brackets of an IPv6 address.
    pub host: String,
    /// The port, `80` for `ws://` and `443` for `wss://` when the URL names none.
    pub port: u16,
}

/// The message for a URL whose scheme is neither `ws://` nor `wss://`.
fn not_a_relay(relay: &str) -> String {
    format!("{relay} is not a ws:// or wss:// URL.")
}

/// The scheme of `relay`, and what follows it.
fn split(relay: &str) -> Result<(bool, &str), String> {
    if let Some(rest) = relay.strip_prefix("ws://") {
        Ok((false, rest))
    } else if let Some(rest) = relay.strip_prefix("wss://") {
        Ok((true, rest))
    } else {
        Err(not_a_relay(relay))
    }
}

impl RelayUrl {
    pub fn parse(relay: &str) -> Result<Self, String> {
        let (tls, rest) = split(relay)?;
        let default = if tls { 443 } else { 80 };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let (host, port) = match authority.strip_prefix('[') {
            Some(bracketed) => {
                let (host, after) = bracketed
                    .split_once(']')
                    .ok_or_else(|| format!("{relay} has no valid host."))?;
                (host, after.strip_prefix(':'))
            }
            None => match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            },
        };
        if host.is_empty() {
            return Err(format!("{relay} names no host."));
        }
        let port = match port {
            Some(port) => port
                .parse()
                .map_err(|_| format!("{relay} has no valid port."))?,
            None => default,
        };
        Ok(Self {
            tls,
            host: host.to_owned(),
            port,
        })
    }
}

/// The HTTP form of a relay URL, at the same authority and path: `http://` for `ws://`
/// and `https://` for `wss://`.
pub fn http_form(relay: &str) -> Result<String, String> {
    RelayUrl::parse(relay)?;
    let (tls, rest) = split(relay)?;
    Ok(format!("{}://{rest}", if tls { "https" } else { "http" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(tls: bool, host: &str, port: u16) -> Result<RelayUrl, String> {
        Ok(RelayUrl {
            tls,
            host: host.to_owned(),
            port,
        })
    }

    #[test]
    fn ws_without_a_port_is_80_and_wss_is_443() {
        assert_eq!(
            RelayUrl::parse("ws://relay.example"),
            at(false, "relay.example", 80)
        );
        assert_eq!(
            RelayUrl::parse("wss://relay.example"),
            at(true, "relay.example", 443)
        );
    }

    #[test]
    fn an_explicit_port_is_kept() {
        assert_eq!(
            RelayUrl::parse("ws://127.0.0.1:7100"),
            at(false, "127.0.0.1", 7100)
        );
        assert_eq!(
            RelayUrl::parse("wss://relay.example:8443"),
            at(true, "relay.example", 8443)
        );
        assert_eq!(RelayUrl::parse("ws://[::1]:7100"), at(false, "::1", 7100));
        assert_eq!(RelayUrl::parse("wss://[::1]/x"), at(true, "::1", 443));
    }

    #[test]
    fn a_path_is_not_part_of_the_authority() {
        assert_eq!(
            RelayUrl::parse("wss://relay.example/nostr?x=1"),
            at(true, "relay.example", 443)
        );
        assert_eq!(
            RelayUrl::parse("ws://relay.example:81/a:b"),
            at(false, "relay.example", 81)
        );
    }

    #[test]
    fn no_host_or_a_bad_port_is_an_error() {
        assert!(RelayUrl::parse("ws://").unwrap_err().contains("no host"));
        assert!(RelayUrl::parse("wss:///path")
            .unwrap_err()
            .contains("no host"));
        assert!(RelayUrl::parse("wss://relay.example:x")
            .unwrap_err()
            .contains("port"));
    }

    #[test]
    fn another_scheme_is_refused_naming_both() {
        for relay in [
            "http://relay.example",
            "https://relay.example",
            "relay.example",
        ] {
            let error = RelayUrl::parse(relay).unwrap_err();
            assert!(
                error.contains("ws://") && error.contains("wss://"),
                "{error}"
            );
        }
    }

    #[test]
    fn the_http_form_keeps_the_authority_and_path() {
        assert_eq!(http_form("ws://h:1/p").as_deref(), Ok("http://h:1/p"));
        assert_eq!(http_form("wss://h/p").as_deref(), Ok("https://h/p"));
        assert!(http_form("https://h").is_err());
        assert!(http_form("wss://").is_err());
    }
}
