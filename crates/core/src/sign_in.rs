//! Signing in to a provider on a Gentle Shell server from this app: the
//! provider's page opens in the app's browser, and when it redirects to the
//! loopback callback the login on the server waits for (`redirectPrefix`,
//! e.g. `http://localhost:1455/`), that navigation is the app's to catch and
//! send to the server instead of loading it (nothing listens on this
//! computer's loopback).

/// Whether a navigation to `url` is the redirect a login waits for: `url`
/// starts with `prefix`. The scheme and host compare without case, as URLs
/// do. A prefix that does not end the address (no trailing `/`, `?` or `#`)
/// only matches whole: `http://localhost:1455` is not a prefix of
/// `http://localhost:14550/`.
pub fn is_login_redirect(prefix: &str, url: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    let Some((head, rest)) = url.split_at_checked(prefix.len()) else {
        return false;
    };
    head.eq_ignore_ascii_case(prefix)
        && (prefix.ends_with(['/', '?', '#'])
            || rest.is_empty()
            || rest.starts_with(['/', '?', '#']))
}

#[cfg(test)]
mod tests {
    use super::is_login_redirect;

    #[test]
    fn the_callback_and_anything_under_it_is_caught() {
        let prefix = "http://localhost:1455/";
        assert!(is_login_redirect(
            prefix,
            "http://localhost:1455/auth/callback?code=abc&state=xyz"
        ));
        assert!(is_login_redirect(prefix, "http://localhost:1455/"));
        assert!(is_login_redirect(prefix, "HTTP://LocalHost:1455/cb?code=1"));
    }

    #[test]
    fn other_pages_load() {
        let prefix = "http://localhost:1455/";
        for url in [
            "https://auth.openai.com/authorize?redirect_uri=http://localhost:1455/",
            "http://localhost:1456/cb",
            "http://127.0.0.1:1455/cb",
            "https://localhost:1455/cb",
            "http://localhost:1455",
            "about:blank",
            "",
        ] {
            assert!(!is_login_redirect(prefix, url), "{url}");
        }
        assert!(!is_login_redirect("", "http://localhost:1455/"));
    }

    #[test]
    fn a_prefix_without_a_slash_matches_only_whole() {
        let prefix = "http://localhost:1455";
        assert!(is_login_redirect(prefix, "http://localhost:1455"));
        assert!(is_login_redirect(prefix, "http://localhost:1455/cb"));
        assert!(is_login_redirect(prefix, "http://localhost:1455?code=1"));
        assert!(!is_login_redirect(prefix, "http://localhost:14550/cb"));
        assert!(!is_login_redirect(
            prefix,
            "http://localhost:1455.evil.example/"
        ));
        // A multi-byte character where the prefix would end is no match.
        assert!(!is_login_redirect("http://a/é", "http://a/e"));
    }
}
