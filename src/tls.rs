//! TLS for a relay: the roots a `wss://` or `https://` relay's certificate is checked
//! against. They are the roots compiled into the binary (webpki's), and, when
//! `TOON_TRUSTED_ROOT` names a PEM file, the certificates in it as well. The variable is a
//! seam for tests and private deployments; it never turns verification off.

use std::sync::Arc;

use tungstenite::Connector;

/// The variable that names a PEM file of certificates to trust in addition.
pub const ROOT_VARIABLE: &str = "TOON_TRUSTED_ROOT";

/// The DER of each certificate in the file `TOON_TRUSTED_ROOT` names; none if it is unset.
/// A file that cannot be read or holds no certificate is an error.
pub fn extra_roots() -> Result<Vec<Vec<u8>>, String> {
    let Some(path) = std::env::var_os(ROOT_VARIABLE).filter(|path| !path.is_empty()) else {
        return Ok(Vec::new());
    };
    let shown = std::path::Path::new(&path).display().to_string();
    let file = std::fs::File::open(&path).map_err(|error| {
        format!("{ROOT_VARIABLE} names {shown}, which cannot be read: {error}.")
    })?;
    let certificates =
        rustls_pemfile::certs(&mut std::io::BufReader::new(file)).map_err(|error| {
            format!("{ROOT_VARIABLE} names {shown}, which cannot be read: {error}.")
        })?;
    if certificates.is_empty() {
        return Err(format!(
            "{ROOT_VARIABLE} names {shown}, which holds no certificate."
        ));
    }
    Ok(certificates)
}

/// The TLS connector a websocket is dialled with.
pub fn connector() -> Result<Connector, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.iter().map(|anchor| {
        rustls::OwnedTrustAnchor::from_subject_spki_name_constraints(
            anchor.subject,
            anchor.spki,
            anchor.name_constraints,
        )
    }));
    for der in extra_roots()? {
        roots.add(&rustls::Certificate(der)).map_err(|error| {
            format!("{ROOT_VARIABLE} holds a certificate that is not usable: {error}.")
        })?;
    }
    let config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Connector::Rustls(Arc::new(config)))
}

/// An error and everything it was caused by, on one line.
pub fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Whether an error's text says that a certificate did not verify.
pub fn is_certificate(text: &str) -> bool {
    text.to_ascii_lowercase().contains("certificate")
}
