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

fn normalized_host(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_lowercase();
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// Extracts the token for `url` from the `coder` CLI's keychain value.
pub fn token_from_keychain(blob_b64: &str, url: &Url) -> Option<String> {
    let json = base64::engine::general_purpose::STANDARD
        .decode(blob_b64.trim())
        .ok()?;
    let creds: HashMap<String, Credential> = serde_json::from_slice(&json).ok()?;
    creds
        .get(&normalized_host(url)?)
        .map(|c| c.api_token.clone())
        .filter(|t| !t.is_empty())
}

/// Discovery against an injectable environment.
pub fn discover_with(env: &dyn SessionEnv) -> Result<Session> {
    let not_logged_in =
        || Error::NotLoggedIn("no Coder session found; run `coder login` first".into());
    let config_dir = env.config_dir();
    let url_text = env
        .var("CODER_URL")
        .or_else(|| {
            config_dir
                .as_ref()
                .and_then(|d| env.read_file(&d.join("url")))
        })
        .ok_or_else(not_logged_in)?;
    let url: Url = url_text
        .trim()
        .parse()
        .map_err(|_| Error::NotLoggedIn(format!("invalid Coder URL {:?}", url_text.trim())))?;
    let token = env
        .var("CODER_SESSION_TOKEN")
        .or_else(|| {
            env.keychain()
                .and_then(|blob| token_from_keychain(&blob, &url))
        })
        .or_else(|| {
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
    #[ignore = "reads the developer's real coder CLI session"]
    fn real_session() {
        let s = super::discover_session().expect("run `coder login` first");
        println!("found session for {}", s.url);
    }
}
