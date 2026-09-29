//! Finds the deployment URL and session token the `coder` CLI stored at `coder login`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

use crate::{Error, Result, Session};

const KEYCHAIN_SERVICE: &str = "coder-v2-credentials";
const KEYCHAIN_ACCOUNT: &str = "coder-login-credentials";

/// Everything discovery reads from the machine, so tests can supply fakes.
pub trait SessionEnv {
    fn var(&self, key: &str) -> Option<String>;
    fn config_dir(&self) -> Option<PathBuf>;
    fn read_file(&self, path: &Path) -> Option<String>;
    /// The raw base64 value stored by the `coder` CLI in the OS keychain, if any.
    fn keychain(&self) -> Option<String>;
}

struct OsEnv;

impl SessionEnv for OsEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn config_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = self.var("CODER_CONFIG_DIR") {
            return Some(PathBuf::from(dir));
        }
        let home = PathBuf::from(self.var("HOME")?);
        if cfg!(target_os = "macos") {
            return Some(home.join("Library/Application Support/coderv2"));
        }
        let base = self
            .var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Some(base.join("coderv2"))
    }

    fn read_file(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn keychain(&self) -> Option<String> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let out = std::process::Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-wa",
                KEYCHAIN_ACCOUNT,
            ])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}

#[derive(Deserialize)]
struct Credential {
    api_token: String,
}

/// Extracts the host key from raw URL text: authority after "://" up to first "/", "?", or "#",
/// with "userinfo@" prefix removed, lowercase, and trimmed.
/// Falls back to parsed Url's host[:port] if no "://" found.
fn host_key_from_text(raw_text: &str, url: &Url) -> Option<String> {
    if let Some(scheme_end) = raw_text.find("://") {
        let after_scheme = &raw_text[scheme_end + 3..];
        let authority_end = after_scheme
            .find(['/', '?', '#'])
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..authority_end];

        // Remove userinfo@ prefix (everything up to and including the last @)
        let host_part = if let Some(at_pos) = authority.rfind('@') {
            &authority[at_pos + 1..]
        } else {
            authority
        };

        return Some(host_part.trim().to_lowercase());
    }

    // Fallback to parsed Url's host[:port]
    let host = url.host_str()?.to_lowercase();
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// Core of `host_keys_agree`, taking an already-extracted raw host key. Split out so callers
/// that also need that key for a keychain lookup (`discover_with`) compute it exactly once and
/// pass the same value here and to the lookup: the checked key and the used key cannot drift.
///
/// Invariant: a raw key with no explicit port means the scheme's default port, never whatever
/// port the parsed URL happens to carry. The URL's own port cannot be used as the fallback,
/// because it can itself be steered by the same backslash-as-slash quirk this check guards
/// against: `https://dev.coder.com:8443\@dev.coder.com` parses to host `dev.coder.com` port
/// `8443`, but its raw key (read up to the last `@`) is just `dev.coder.com` with no port.
/// Comparing that port-less key against the URL's own port (8443) would trivially agree,
/// letting a token stored for `dev.coder.com`'s default port leak to port 8443 instead.
fn host_key_agrees_with_url(raw_key: &str, url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    let scheme_default_port = || {
        let mut default_port_url = url.clone();
        let _ = default_port_url.set_port(None);
        default_port_url.port_or_known_default()
    };
    // A bracketed IPv6 raw key (e.g. "[::1]:3000") has an internal colon on either side of the
    // closing bracket, so only split off a port when the text after the last colon is itself a
    // valid port number; otherwise treat the whole raw key as the host with no explicit port.
    let (raw_host, raw_port) = match raw_key.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(p) => (h.to_owned(), p),
            Err(_) => match scheme_default_port() {
                Some(p) => (raw_key.to_owned(), p),
                None => return false,
            },
        },
        None => match scheme_default_port() {
            Some(p) => (raw_key.to_owned(), p),
            None => return false,
        },
    };
    raw_host.eq_ignore_ascii_case(host) && raw_port == port
}

/// True when the raw-text host key and the parsed URL name the same host and port.
/// The raw key matches the `coder` CLI's storage; the parsed URL is where requests really go.
/// A `url` crate quirk treats `\` as `/` in special schemes like `https`, so
/// `https://evil.example\@dev.coder.com` parses to host `evil.example` even though its raw
/// text reads as `dev.coder.com` up to the last `@`. This catches that divergence.
///
/// `discover_with` does not call this directly: it extracts the raw key once and calls
/// `host_key_agrees_with_url` with that same key, so the checked key and the key used for the
/// keychain lookup cannot drift apart. This wrapper exists so tests can exercise the agreement
/// check from raw text the same way the brief's tests do, without duplicating extraction.
#[cfg(test)]
fn host_keys_agree(raw_text: &str, url: &Url) -> bool {
    host_key_from_text(raw_text, url).is_some_and(|raw_key| host_key_agrees_with_url(&raw_key, url))
}

/// Extracts the token for a specific host key from the `coder` CLI's keychain value.
pub fn token_from_keychain_for_host(blob_b64: &str, host_key: &str) -> Option<String> {
    let json = base64::engine::general_purpose::STANDARD
        .decode(blob_b64.trim())
        .ok()?;
    let creds: HashMap<String, Credential> = serde_json::from_slice(&json).ok()?;
    creds
        .get(host_key)
        .map(|c| c.api_token.clone())
        .filter(|t| !t.is_empty())
}

/// Extracts the token for `url` from the `coder` CLI's keychain value.
pub fn token_from_keychain(blob_b64: &str, url: &Url) -> Option<String> {
    token_from_keychain_for_host(blob_b64, &host_key_from_text(url.as_str(), url)?)
}

/// Discovery against an injectable environment.
pub fn discover_with(env: &dyn SessionEnv) -> Result<Session> {
    let not_logged_in =
        || Error::NotLoggedIn("no Coder session found; run `coder login` first".into());
    let config_dir = env.config_dir();
    let config_url_text = config_dir
        .as_ref()
        .and_then(|d| env.read_file(&d.join("url")));
    let env_url_text = env.var("CODER_URL");
    let url_came_from_config_file = env_url_text.is_none();
    let url_text = env_url_text
        .or_else(|| config_url_text.clone())
        .ok_or_else(not_logged_in)?;
    let trimmed_url_text = url_text.trim();
    let url: Url = trimmed_url_text
        .parse()
        .map_err(|_| Error::NotLoggedIn(format!("invalid Coder URL {:?}", trimmed_url_text)))?;

    // The session file on disk belongs to whatever host `config_dir/url` names. It is only
    // safe to read when the URL we resolved above is that same file, or when CODER_URL names
    // the same host, otherwise we would send config_dir/url's host's token to a different host.
    let session_file_matches_url = url_came_from_config_file
        || config_url_text.as_deref().is_some_and(|config_text| {
            let config_trimmed = config_text.trim();
            let Ok(config_url): std::result::Result<Url, _> = config_trimmed.parse() else {
                return false;
            };
            match (
                host_key_from_text(trimmed_url_text, &url),
                host_key_from_text(config_trimmed, &config_url),
            ) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            }
        });

    // Gate both stored-credential sources on the parsed URL actually naming the host its raw
    // text claims to, closing the `url`-crate backslash-parsing gap described on `host_keys_agree`.
    // Computed once so the key that gets validated and the key that gets looked up in the
    // keychain are always the same value; see `host_key_agrees_with_url`.
    let raw_host_key = host_key_from_text(trimmed_url_text, &url);
    let agree = raw_host_key
        .as_deref()
        .is_some_and(|key| host_key_agrees_with_url(key, &url));

    let token = env
        .var("CODER_SESSION_TOKEN")
        .or_else(|| {
            if !agree {
                return None;
            }
            env.keychain()
                .and_then(|blob| token_from_keychain_for_host(&blob, raw_host_key.as_deref()?))
        })
        .or_else(|| {
            if !agree || !session_file_matches_url {
                return None;
            }
            config_dir
                .as_ref()
                .and_then(|d| env.read_file(&d.join("session")))
        })
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .ok_or_else(not_logged_in)?;
    Ok(Session {
        url,
        token: SecretString::from(token),
    })
}

/// Finds the session the `coder` CLI stored, honoring `CODER_URL` and `CODER_SESSION_TOKEN`.
pub fn discover_session() -> Result<Session> {
    discover_with(&OsEnv)
}

#[cfg(test)]
mod tests {
    use super::{SessionEnv, discover_with, token_from_keychain};
    use base64::Engine;
    use secrecy::ExposeSecret;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeEnv {
        vars: HashMap<String, String>,
        files: HashMap<std::path::PathBuf, String>,
        keychain: Option<String>,
    }

    impl SessionEnv for FakeEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }
        fn config_dir(&self) -> Option<std::path::PathBuf> {
            Some(std::path::PathBuf::from("/cfg"))
        }
        fn read_file(&self, path: &std::path::Path) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn keychain(&self) -> Option<String> {
            self.keychain.clone()
        }
    }

    fn keychain_blob(host: &str, token: &str) -> String {
        let json =
            serde_json::json!({host: {"coder_url": format!("https://{host}"), "api_token": token}});
        base64::engine::general_purpose::STANDARD.encode(json.to_string())
    }

    #[test]
    fn env_overrides_everything() {
        let mut env = FakeEnv::default();
        env.vars
            .insert("CODER_URL".into(), "https://env.example.com".into());
        env.vars
            .insert("CODER_SESSION_TOKEN".into(), "test-token-env".into());
        env.files
            .insert("/cfg/url".into(), "https://file.example.com\n".into());
        let s = discover_with(&env).unwrap();
        assert_eq!(s.url.as_str(), "https://env.example.com/");
        assert_eq!(s.token.expose_secret(), "test-token-env");
    }

    #[test]
    fn keychain_token_wins_over_session_file() {
        let mut env = FakeEnv::default();
        env.files
            .insert("/cfg/url".into(), "https://dev.coder.com".into());
        env.files
            .insert("/cfg/session".into(), "test-token-file".into());
        env.keychain = Some(keychain_blob("dev.coder.com", "test-token-keychain"));
        assert_eq!(
            discover_with(&env).unwrap().token.expose_secret(),
            "test-token-keychain"
        );
    }

    #[test]
    fn falls_back_to_session_file_when_keychain_lacks_host() {
        let mut env = FakeEnv::default();
        env.files
            .insert("/cfg/url".into(), "https://dev.coder.com".into());
        env.files
            .insert("/cfg/session".into(), "test-token-file".into());
        env.keychain = Some(keychain_blob("other.example.com", "test-token-other"));
        assert_eq!(
            discover_with(&env).unwrap().token.expose_secret(),
            "test-token-file"
        );
    }

    #[test]
    fn normalizes_host_and_trims_files() {
        let mut env = FakeEnv::default();
        env.files
            .insert("/cfg/url".into(), "  https://Dev.Coder.com:8443/ \n".into());
        env.keychain = Some(keychain_blob("dev.coder.com:8443", "test-token-port"));
        let s = discover_with(&env).unwrap();
        assert_eq!(s.url.host_str(), Some("dev.coder.com"));
        assert_eq!(s.token.expose_secret(), "test-token-port");

        let mut env = FakeEnv::default();
        env.files
            .insert("/cfg/url".into(), "https://dev.coder.com\n".into());
        env.files
            .insert("/cfg/session".into(), "test-token-file\n".into());
        assert_eq!(
            discover_with(&env).unwrap().token.expose_secret(),
            "test-token-file"
        );
    }

    #[test]
    fn missing_login_says_run_coder_login() {
        let err = discover_with(&FakeEnv::default()).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn malformed_keychain_blob_is_ignored() {
        let url: url::Url = "https://dev.coder.com".parse().unwrap();
        assert_eq!(token_from_keychain("not base64!!", &url), None);
    }

    #[test]
    fn keychain_with_explicit_default_port() {
        let mut env = FakeEnv::default();
        env.files
            .insert("/cfg/url".into(), "https://dev.coder.com:443/".into());
        env.keychain = Some(keychain_blob(
            "dev.coder.com:443",
            "test-token-explicit-port",
        ));
        assert_eq!(
            discover_with(&env).unwrap().token.expose_secret(),
            "test-token-explicit-port"
        );
    }

    #[test]
    fn keychain_without_explicit_default_port() {
        let mut env = FakeEnv::default();
        env.files
            .insert("/cfg/url".into(), "https://dev.coder.com".into());
        env.keychain = Some(keychain_blob("dev.coder.com", "test-token-implicit-port"));
        assert_eq!(
            discover_with(&env).unwrap().token.expose_secret(),
            "test-token-implicit-port"
        );
    }

    #[test]
    fn extracts_host_key_from_url_with_userinfo_and_path() {
        let mut env = FakeEnv::default();
        env.files.insert(
            "/cfg/url".into(),
            "https://user@Dev.Coder.com:8443/path?q=1".into(),
        );
        env.keychain = Some(keychain_blob("dev.coder.com:8443", "test-token-userinfo"));
        assert_eq!(
            discover_with(&env).unwrap().token.expose_secret(),
            "test-token-userinfo"
        );
    }

    #[test]
    fn coder_url_for_different_host_than_config_ignores_session_file() {
        let mut env = FakeEnv::default();
        env.vars
            .insert("CODER_URL".into(), "https://b.example.com".into());
        env.files
            .insert("/cfg/url".into(), "https://a.example.com".into());
        env.files
            .insert("/cfg/session".into(), "test-token-file".into());
        let err = discover_with(&env).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn coder_url_matching_config_host_uses_session_file() {
        let mut env = FakeEnv::default();
        env.vars
            .insert("CODER_URL".into(), "https://Dev.Coder.com/".into());
        env.files
            .insert("/cfg/url".into(), "https://dev.coder.com".into());
        env.files
            .insert("/cfg/session".into(), "test-token-file".into());
        assert_eq!(
            discover_with(&env).unwrap().token.expose_secret(),
            "test-token-file"
        );
    }

    #[test]
    #[ignore = "reads the developer's real coder CLI session"]
    fn real_session() {
        let s = super::discover_session().expect("run `coder login` first");
        println!("found session for {}", s.url);
    }

    #[test]
    fn backslash_userinfo_never_selects_another_hosts_keychain_token() {
        let mut env = FakeEnv::default();
        env.vars.insert(
            "CODER_URL".into(),
            r"https://evil.example\@dev.coder.com".into(),
        );
        env.keychain = Some(keychain_blob("dev.coder.com", "test-token-keychain"));
        let err = discover_with(&env).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn backslash_userinfo_never_selects_another_hosts_session_file() {
        let mut env = FakeEnv::default();
        env.vars.insert(
            "CODER_URL".into(),
            r"https://evil.example\@dev.coder.com".into(),
        );
        env.files
            .insert("/cfg/url".into(), "https://dev.coder.com".into());
        env.files
            .insert("/cfg/session".into(), "test-token-file".into());
        let err = discover_with(&env).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn host_keys_agree_matches_default_ports_and_rejects_mismatch() {
        let ok: url::Url = "https://dev.coder.com:443/".parse().unwrap();
        assert!(super::host_keys_agree("https://dev.coder.com:443/", &ok));
        let bad: url::Url = r"https://evil.example\@dev.coder.com".parse().unwrap();
        assert!(!super::host_keys_agree(
            r"https://evil.example\@dev.coder.com",
            &bad
        ));
    }

    #[test]
    fn missing_host_keys_are_not_a_match() {
        let url: url::Url = "file:///tmp/x".parse().unwrap();
        assert!(!super::host_keys_agree("file:///tmp/x", &url));
    }

    #[test]
    fn host_keys_agree_handles_bracketed_ipv6_with_port() {
        let url: url::Url = "https://[::1]:3000/".parse().unwrap();
        assert_eq!(url.host_str(), Some("[::1]"));
        assert!(super::host_keys_agree("https://[::1]:3000/", &url));
        let mismatched: url::Url = "https://[::1]:4000/".parse().unwrap();
        assert!(!super::host_keys_agree("https://[::1]:3000/", &mismatched));
    }

    #[test]
    fn host_keys_agree_handles_bracketed_ipv6_default_port() {
        let url: url::Url = "https://[::1]/".parse().unwrap();
        assert!(super::host_keys_agree("https://[::1]/", &url));
    }

    // The `\@` trick from the earlier backslash tests also works when the injected authority
    // adds a non-default port: the raw key (read up to the last `@`) loses the port entirely,
    // so a naive "no port in the raw key means trust the URL's port" fallback would trivially
    // agree with whatever port the URL parsed to. `host_key_agrees_with_url` must instead
    // compare a port-less raw key against the scheme's default port.

    #[test]
    fn backslash_port_injection_never_selects_another_hosts_keychain_token() {
        let mut env = FakeEnv::default();
        env.vars.insert(
            "CODER_URL".into(),
            r"https://dev.coder.com:8443\@dev.coder.com".into(),
        );
        env.keychain = Some(keychain_blob("dev.coder.com", "test-token-keychain"));
        let err = discover_with(&env).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn backslash_port_injection_never_selects_another_hosts_session_file() {
        let mut env = FakeEnv::default();
        env.vars.insert(
            "CODER_URL".into(),
            r"https://dev.coder.com:8443\@dev.coder.com".into(),
        );
        env.files
            .insert("/cfg/url".into(), "https://dev.coder.com".into());
        env.files
            .insert("/cfg/session".into(), "test-token-file".into());
        let err = discover_with(&env).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn backslash_port_injection_ipv6_never_selects_another_hosts_keychain_token() {
        let mut env = FakeEnv::default();
        env.vars
            .insert("CODER_URL".into(), r"https://[::1]:8443\@[::1]".into());
        env.keychain = Some(keychain_blob("[::1]", "test-token-keychain"));
        let err = discover_with(&env).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn backslash_port_injection_ipv6_never_selects_another_hosts_session_file() {
        let mut env = FakeEnv::default();
        env.vars
            .insert("CODER_URL".into(), r"https://[::1]:8443\@[::1]".into());
        env.files.insert("/cfg/url".into(), "https://[::1]".into());
        env.files
            .insert("/cfg/session".into(), "test-token-file".into());
        let err = discover_with(&env).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn keychain_with_nondefault_port_still_finds_token() {
        let mut env = FakeEnv::default();
        env.files
            .insert("/cfg/url".into(), "https://dev.coder.com:8443/".into());
        env.keychain = Some(keychain_blob(
            "dev.coder.com:8443",
            "test-token-nondefault-port",
        ));
        assert_eq!(
            discover_with(&env).unwrap().token.expose_secret(),
            "test-token-nondefault-port"
        );
    }
}
