//! Where a provider transcription may be sent: an `https://` URL, or plain
//! `http://` only for a service on this machine. Endpoints that carry
//! credentials or a fragment are rejected before anything is sent.

use url::Url;

/// Parses `raw` as a transcription endpoint the app may send audio to:
/// `https://`, or `http://` on the loopback only, with no user, password or
/// fragment. The `Url` it returns is what the request goes to.
pub fn validate(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim())
        .map_err(|_| "The endpoint is not a valid URL (https://host/path).".to_string())?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return Err(
            "The endpoint must use https:// (or http:// for a service on this machine)."
                .to_string(),
        );
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("The endpoint cannot carry a user or a password.".to_string());
    }
    if url.fragment().is_some() {
        return Err("The endpoint cannot carry a fragment (#…).".to_string());
    }
    let host = url.host_str().unwrap_or_default();
    if host.is_empty() {
        return Err("The endpoint needs a host.".to_string());
    }
    let loopback = matches!(host, "localhost" | "[::1]")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if url.scheme() == "http" && !loopback {
        return Err(
            "Plain http:// is only allowed for a service on this machine (localhost).".to_string(),
        );
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn accepts_https_and_normalizes_it() {
        let url =
            validate(" https://example.com/v1/transcriptions ").expect("a plain https endpoint");
        assert_eq!(url.as_str(), "https://example.com/v1/transcriptions");
    }

    #[test]
    fn accepts_http_only_on_this_machine() {
        assert!(validate("http://localhost:8080/transcribe").is_ok());
        assert!(validate("http://127.0.0.1:9000/transcribe").is_ok());
        assert!(validate("http://[::1]:9000/transcribe").is_ok());
        assert!(validate("http://example.com/transcribe").is_err());
        assert!(validate("http://192.168.1.10/transcribe").is_err());
    }

    #[test]
    fn rejects_other_schemes_and_garbage() {
        assert!(validate("ftp://example.com/audio").is_err());
        assert!(validate("file:///tmp/audio").is_err());
        assert!(validate("not a url").is_err());
        assert!(validate("").is_err());
    }

    #[test]
    fn rejects_endpoints_that_carry_credentials_or_a_fragment() {
        let errors = [
            validate("https://user:pass@example.com/transcribe"),
            validate("https://user@example.com/transcribe"),
            validate("https://example.com/transcribe#crop"),
        ];
        assert!(errors.iter().all(Result::is_err));
    }
}
