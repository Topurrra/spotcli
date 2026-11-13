//! Authentication with Spotify's Authorization Code + PKCE flow.
//!
//! The browser handles the login; this process only ever sees the one-time
//! authorization code that Spotify redirects back to a loopback listener.
//! Tokens are kept in a per-user file with owner-only permissions.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::api::Result;

const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

/// The loopback redirect Spotify calls after login. It must match, to the
/// character, the redirect URI registered in the Spotify developer dashboard.
pub const REDIRECT_PORT: u16 = 8989;
pub const REDIRECT_URI: &str = "http://127.0.0.1:8989/callback";

/// Scopes the app needs: playback control, library and playlist reads.
/// `user-read-private` lifts the plan (Free or Premium) from `/me`, which
/// decides whether local streaming is offered at all.
pub const SCOPES: &[&str] = &[
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-library-read",
    "user-library-modify",
    "playlist-modify-public",
    "playlist-modify-private",
    "user-top-read",
    "user-read-playback-state",
    "user-read-currently-playing",
    "user-modify-playback-state",
    "user-read-private",
];

/// Refresh this long before the access token actually expires.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// How long the loopback listener waits for the browser to come back.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

// --- Streaming credentials (built-in player) ------------------------------
//
// Spotify's streaming access points only accept tokens from Spotify's own
// OAuth clients, not tokens minted for a developer-dashboard app (the same
// lesson Spotifast encodes in its "playback grant"). The built-in player
// therefore keeps a separate grant: Spotify's desktop-app client id, the
// `streaming` scope, and a loopback redirect on port 8898. The one-time
// token is exchanged inside librespot for a durable reusable credential,
// saved in `streaming/credentials.json`; sessions after the first never
// touch the OAuth flow again.

/// Spotify's own desktop client identity, the one librespot streams with.
pub const PLAYBACK_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
/// The port the streaming consent redirect uses.
pub const STREAMING_REDIRECT_PORT: u16 = 8898;
const STREAMING_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";

fn streaming_authorize_url(challenge: &str, state: &str) -> String {
    // show_dialog forces a full consent, so a browser that is already signed
    // in cannot silently approve without the streaming scope.
    let mut url: reqwest::Url = AUTHORIZE_URL.parse().expect("static URL");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", PLAYBACK_CLIENT_ID)
        .append_pair("scope", "streaming")
        .append_pair("redirect_uri", STREAMING_REDIRECT_URI)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", challenge)
        .append_pair("state", state)
        .append_pair("show_dialog", "true");
    url.to_string()
}

/// Run the streaming grant: a second browser consent whose code is exchanged
/// for an access token with the `streaming` scope, then traded for a durable
/// reusable librespot credential.
pub async fn login_streaming(dir: &Path) -> anyhow::Result<()> {
    let (verifier, challenge) = pkce_pair();
    let state = random_state();
    let url = streaming_authorize_url(&challenge, &state);
    println!("Opening your browser once more to enable local streaming...");
    let _ = open_browser(&url);
    let code = wait_for_code(STREAMING_REDIRECT_PORT, "/login", &state).await?;
    let http = reqwest::Client::new();
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", STREAMING_REDIRECT_URI),
        ("client_id", PLAYBACK_CLIENT_ID),
        ("code_verifier", verifier.as_str()),
    ];
    let tokens = token_request(&http, &params).await?;
    // Prove the grant streams, and bank the reusable credential librespot
    // mints from it, so later sessions skip this flow entirely.
    let cache = librespot::core::cache::Cache::new(Some(dir.join("streaming")), None, None, None)?;
    let session = librespot::core::Session::new(librespot::core::config::SessionConfig::default(), Some(cache));
    session
        .connect(
            librespot::core::authentication::Credentials::with_access_token(tokens.access_token),
            true,
        )
        .await
        .map_err(|e| anyhow::anyhow!("Spotify refused the streaming session: {e}"))?;
    Ok(())
}

/// Credentials for the built-in player: the reusable grant banked by
/// `login_streaming`, if it exists.
pub async fn streaming_credentials(dir: &Path) -> anyhow::Result<librespot::core::authentication::Credentials> {
    let cache = librespot::core::cache::Cache::new(Some(dir.join("streaming")), None, None, None)?;
    cache.credentials().ok_or_else(|| {
        anyhow::anyhow!(
            "local streaming is not enabled. Run `spotcli login` and approve              the streaming permission in the browser."
        )
    })
}

/// Remove stored streaming credentials (logout).
pub fn clear_streaming(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir.join("streaming"));
}


/// A login rejected by Spotify, or a listener that nobody answered.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("login timed out after {0}s; no authorization answer arrived")]
    Timeout(u64),
    #[error("Spotify returned an error: {0}")]
    Rejected(String),
}

/// A random, URL-safe OAuth `state` value.
pub fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE verifier and its S256 challenge, per RFC 7636.
pub fn pkce_pair() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// The URL the user opens in a browser.
pub fn authorize_url(client_id: &str, challenge: &str, state: &str) -> String {
    let mut url: reqwest::Url = AUTHORIZE_URL.parse().expect("static URL");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("scope", &SCOPES.join(" "))
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", challenge)
        .append_pair("state", state);
    url.to_string()
}

/// Wait for Spotify's redirect on the loopback listener and return its code.
///
/// Binds `127.0.0.1:{port}` and accepts connections until one requests
/// `expected_path` (browser preconnects, favicon fetches and the like get a
/// 404), giving up after [`LOGIN_TIMEOUT`] overall. The callback must carry
/// `expected_state`, or the login is rejected.
pub async fn wait_for_code(
    port: u16,
    expected_path: &'static str,
    expected_state: &str,
) -> std::result::Result<String, AuthError> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| AuthError::Rejected(format!("could not bind port {port}: {e}")))?;

    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    loop {
        let (socket, _) = tokio::time::timeout_at(deadline, listener.accept())
            .await
            .map_err(|_| AuthError::Timeout(LOGIN_TIMEOUT.as_secs()))?
            .map_err(|e| AuthError::Rejected(format!("listener failed: {e}")))?;
        let handled = tokio::time::timeout_at(
            deadline,
            handle_callback(socket, expected_path, expected_state),
        )
        .await
        .map_err(|_| AuthError::Timeout(LOGIN_TIMEOUT.as_secs()))?;
        if let Some(result) = handled {
            return result;
        }
    }
}

/// Serve one connection. `None` means it was not the callback (answered 404
/// or sent nothing).
async fn handle_callback(
    mut socket: tokio::net::TcpStream,
    expected_path: &str,
    expected_state: &str,
) -> Option<std::result::Result<String, AuthError>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 8 * 1024];
    let n = match tokio::time::timeout(Duration::from_secs(10), socket.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => n,
        _ => return None,
    };
    let request = String::from_utf8_lossy(&buf[..n]);
    let target = request.split_whitespace().nth(1).unwrap_or_default();
    let path = target.split('?').next().unwrap_or_default();
    if path != expected_path {
        let _ = socket
            .write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .await;
        let _ = socket.shutdown().await;
        return None;
    }

    let query = target.split_once('?').map(|(_, q)| q).unwrap_or_default();
    let mut code = None;
    let mut error = None;
    let mut state = None;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        match (parts.next().unwrap_or_default(), parts.next().unwrap_or_default()) {
            ("code", value) if !value.is_empty() => code = Some(value.to_string()),
            ("error", value) if !value.is_empty() => error = Some(value.to_string()),
            ("state", value) => state = Some(value.to_string()),
            _ => {}
        }
    }

    let state_ok = state.as_deref() == Some(expected_state);
    let page = if error.is_some() || code.is_none() || !state_ok {
        "Login failed. You can close this tab and try again."
    } else {
        "Login complete. You can close this tab and return to the terminal."
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        page.len(),
        page
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;

    if let Some(error) = error {
        return Some(Err(AuthError::Rejected(error)));
    }
    if !state_ok {
        return Some(Err(AuthError::Rejected(
            "the login response did not match this login attempt (state mismatch)".to_string(),
        )));
    }
    Some(code.ok_or_else(|| AuthError::Rejected("no authorization code arrived".to_string())))
}

/// Exchanged tokens, as Spotify returns them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds after which the access token is no longer valid.
    pub expires_at: i64,
}

/// Persisted credentials plus the app identity they belong to.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredCredentials {
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// A file-backed credential store with owner-only permissions.
pub struct TokenStore {
    path: PathBuf,
    credentials: tokio::sync::Mutex<StoredCredentials>,
}

impl TokenStore {
    /// Load from `dir/credentials.json`, or `None` when not logged in yet.
    pub fn load(dir: &Path) -> anyhow::Result<Option<Self>> {
        Ok(Self::read(dir)?.map(|credentials| Self {
            path: dir.join("credentials.json"),
            credentials: tokio::sync::Mutex::new(credentials),
        }))
    }

    /// Read the stored credentials without wrapping them in the store.
    pub fn read(dir: &Path) -> anyhow::Result<Option<StoredCredentials>> {
        let path = dir.join("credentials.json");
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        let credentials: StoredCredentials = serde_json::from_slice(&raw)
            .map_err(|e| anyhow::anyhow!("{} is corrupt: {e}", path.display()))?;
        Ok(Some(credentials))
    }

    /// Save freshly exchanged tokens, creating the directory if needed.
    pub fn save(dir: &Path, credentials: &StoredCredentials) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("credentials.json");
        let tmp = dir.join("credentials.json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let write = (|| -> std::io::Result<()> {
            let mut file = options.open(&tmp)?;
            file.write_all(&serde_json::to_vec_pretty(credentials)?)?;
            file.sync_all()?;
            drop(file);
            // Replaces an existing file on both unix and Windows.
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = write {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    }

    /// Remove stored credentials (logout).
    pub fn clear(dir: &Path) {
        let _ = std::fs::remove_file(dir.join("credentials.json"));
    }

    /// A valid access token, refreshing first when it is close to expiry.
    /// `rejected` is the token a request just got a 401 for: a refresh is
    /// forced even before expiry, unless the stored token has already changed
    /// since (another request refreshed it), in which case that one is reused.
    pub async fn access_token(&self, http: &reqwest::Client, rejected: Option<&str>) -> Result<String> {
        let mut credentials = self.credentials.lock().await;
        let stale = rejected.is_some_and(|token| token == credentials.access_token);
        if !stale && (now_unix() + REFRESH_MARGIN.as_secs() as i64) < credentials.expires_at {
            return Ok(credentials.access_token.clone());
        }
        let old_refresh = credentials.refresh_token.clone();
        let refreshed = refresh(http, &credentials.client_id, &old_refresh)
            .await
            .map_err(|e| {
                if e.downcast_ref::<InvalidGrant>().is_some() {
                    crate::api::ApiError::SessionExpired
                } else {
                    crate::api::ApiError::Network(e.to_string())
                }
            })?;
        // Spotify omits refresh_token on a refresh grant; keep the old one then.
        let refresh_token = if refreshed.refresh_token.is_empty() {
            old_refresh
        } else {
            refreshed.refresh_token
        };
        *credentials = StoredCredentials {
            client_id: credentials.client_id.clone(),
            access_token: refreshed.access_token.clone(),
            refresh_token,
            expires_at: refreshed.expires_at,
        };
        if let Some(dir) = self.path.parent() {
            let _ = Self::save(dir, &credentials);
        }
        Ok(credentials.access_token.clone())
    }

}

/// Run the full login flow: open a browser, wait for the redirect, exchange
/// the code, and persist the tokens.
pub async fn login(
    http: &reqwest::Client,
    client_id: &str,
    dir: &Path,
) -> anyhow::Result<StoredCredentials> {
    let (verifier, challenge) = pkce_pair();
    let state = random_state();
    let url = authorize_url(client_id, &challenge, &state);
    println!("Opening your browser to sign in to Spotify...");
    println!("(If it does not open, visit this URL yourself:)");
    println!("  {url}");
    let _ = open_browser(&url);

    let code = wait_for_code(REDIRECT_PORT, "/callback", &state).await?;
    let tokens = exchange(http, client_id, &code, &verifier)
        .await
        .map_err(|e| crate::api::ApiError::Network(e.to_string()))?;
    let credentials = StoredCredentials {
        client_id: client_id.to_string(),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_at,
    };
    TokenStore::save(dir, &credentials)?;
    Ok(credentials)
}

/// Exchange an authorization code for tokens.
pub async fn exchange(
    http: &reqwest::Client,
    client_id: &str,
    code: &str,
    verifier: &str,
) -> anyhow::Result<Tokens> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", client_id),
        ("code_verifier", verifier),
    ];
    token_request(http, &params).await
}

/// Refresh an access token.
pub async fn refresh(
    http: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> anyhow::Result<Tokens> {
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    token_request(http, &params).await
}

/// The refresh token was revoked or expired: the user must log in again.
#[derive(Debug, thiserror::Error)]
#[error("your Spotify session expired; run `spotcli login` again")]
struct InvalidGrant;

async fn token_request(
    http: &reqwest::Client,
    params: &[(&str, &str)],
) -> anyhow::Result<Tokens> {
    let reply = http.post(TOKEN_URL).form(params).send().await?;
    let status = reply.status();
    if !status.is_success() {
        let body = reply.text().await.unwrap_or_default();
        if body.contains("invalid_grant") {
            return Err(InvalidGrant.into());
        }
        anyhow::bail!("Spotify refused the token request ({status}); check the client id");
    }
    let response: serde_json::Value = reply.json().await?;
    let access_token = response
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("token response had no access_token"))?
        .to_string();
    // Spotify omits refresh_token on a refresh grant; keep the old one then.
    let refresh_token = response
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let expires_in = response
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);
    Ok(Tokens {
        access_token,
        refresh_token: refresh_token.unwrap_or_default(),
        expires_at: now_unix() + expires_in,
    })
}

/// Open a URL in the default browser, best effort.
fn open_browser(url: &str) -> anyhow::Result<()> {
    open::that(url)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_pair_is_well_formed() {
        let (verifier, challenge) = pkce_pair();
        assert_eq!(verifier.len(), 43);
        assert!(verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
    }

    #[test]
    fn pkce_pairs_differ() {
        let a = pkce_pair();
        let b = pkce_pair();
        assert_ne!(a.0, b.0);
    }

    #[test]
    fn authorize_url_carries_the_pkce_parameters() {
        let url = authorize_url("client123", "challenge-abc", "state-xyz");
        assert!(url.starts_with("https://accounts.spotify.com/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=client123"));
        assert!(url.contains(&format!("redirect_uri={}", urlencode(REDIRECT_URI))));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("code_challenge=challenge-abc"));
        assert!(url.contains("scope=playlist-read-private"));
        assert!(url.contains("state=state-xyz"));
    }

    fn urlencode(value: &str) -> String {
        // reqwest percent-encodes the ':' and '/' in the redirect URI.
        value
            .replace(':', "%3A")
            .replace('/', "%2F")
    }
}
