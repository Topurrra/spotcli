//! An authenticated, rate-limit-aware transport for the Spotify Web API.

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client as HttpClient, Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::auth;
use crate::models::{
    Device, Playback, Playlist, RawDevice, RawPaging, RawPlayback, RawPlaylist, RawPlaylistItem,
    RawSavedTrack, RawSearch, RawUser, Track, User, map_device, map_playlist,
    map_playback, map_track,
};

const BASE_URL: &str = "https://api.spotify.com/v1";

pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("not signed in")]
    NotSignedIn,
    #[error("Spotify returned {status}: {message}")]
    Status { status: u16, message: String },
    #[error("Spotify is rate limiting requests; try again in a moment")]
    RateLimited { retry_after: Option<Duration> },
    #[error("your Spotify session expired; run `spotcli login` again")]
    SessionExpired,
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected response from Spotify: {0}")]
    Decode(String),
}

impl ApiError {
    /// A 401, which a token refresh may fix.
    pub fn is_auth(&self) -> bool {
        matches!(self, ApiError::SessionExpired)
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(error: reqwest::Error) -> Self {
        if error.is_decode() {
            ApiError::Decode(error.to_string())
        } else {
            ApiError::Network(error.to_string())
        }
    }
}

/// An authenticated Web API client. Cheap to clone; clones share one token
/// refresh lock, so concurrent requests never refresh at the same time.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: HttpClient,
    tokens: auth::TokenStore,
}

impl Client {
    pub fn new(tokens: auth::TokenStore) -> Self {
        Self {
            inner: Arc::new(Inner {
                http: HttpClient::builder()
                    .timeout(Duration::from_secs(15))
                    .build()
                    .expect("static client configuration"),
                tokens,
            }),
        }
    }

    /// Load the stored credentials from `dir`, or fail with login advice.
    pub fn from_dir(dir: &std::path::Path) -> anyhow::Result<Self> {
        let tokens = auth::TokenStore::load(dir)?
            .ok_or_else(|| anyhow::anyhow!("not signed in. Run `spotcli login` first."))?;
        Ok(Self::new(tokens))
    }

    fn http(&self) -> &HttpClient {
        &self.inner.http
    }

    fn tokens(&self) -> &auth::TokenStore {
        &self.inner.tokens
    }

    /// GET a JSON document, refreshing the token once on a 401.
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        for attempt in 0..2 {
            let token = self.tokens().access_token(self.http(), attempt > 0).await?;
            let response = self
                .http()
                .get(format!("{BASE_URL}{path}"))
                .bearer_auth(token)
                .send()
                .await?;
            match Self::check(response).await {
                Ok(response) => return response.json().await.map_err(ApiError::from),
                Err(e) if e.is_auth() && attempt == 0 => continue,
                Err(e) => return Err(e),
            }
        }
        unreachable!("the loop returns on every branch");
    }

    /// PUT or POST, with an optional JSON body, ignoring the empty reply.
    async fn send(&self, method: Method, path: &str, body: Option<serde_json::Value>) -> Result<()> {
        for attempt in 0..2 {
            let token = self.tokens().access_token(self.http(), attempt > 0).await?;
            let mut request = self
                .http()
                .request(method.clone(), format!("{BASE_URL}{path}"))
                .bearer_auth(token);
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request.send().await?;
            match Self::check_empty(response).await {
                Ok(()) => return Ok(()),
                Err(e) if e.is_auth() && attempt == 0 => continue,
                Err(e) => return Err(e),
            }
        }
        unreachable!("the loop returns on every branch");
    }

    async fn check(response: reqwest::Response) -> Result<reqwest::Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::SessionExpired);
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs);
            return Err(ApiError::RateLimited { retry_after });
        }
        let message = response.text().await.unwrap_or_default();
        Err(ApiError::Status {
            status: status.as_u16(),
            message,
        })
    }

    async fn check_empty(response: reqwest::Response) -> Result<()> {
        Self::check(response).await.map(|_| ())
    }

    // --- Endpoints ---------------------------------------------------------

    /// The signed-in user's profile.
    pub async fn me(&self) -> Result<User> {
        let raw: RawUser = self.get("/me").await?;
        Ok(User {
            id: raw.id.clone().unwrap_or_default(),
            display_name: raw
                .display_name
                .clone()
                .unwrap_or_else(|| raw.id.unwrap_or_default()),
            country: raw.country.unwrap_or_default(),
            product: raw.product.unwrap_or_default(),
        })
    }

    /// The user's playlists (first page of 50).
    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        let page: RawPaging<RawPlaylist> = self.get("/me/playlists?limit=50").await?;
        Ok(page.items.into_iter().map(map_playlist).collect())
    }

    /// A playlist's tracks, following pagination up to `limit` entries.
    pub async fn playlist_tracks(&self, id: &str, limit: u32) -> Result<Vec<Track>> {
        let mut tracks = Vec::new();
        let mut offset: u32 = 0;
        loop {
            let remaining = limit.saturating_sub(tracks.len() as u32);
            if remaining == 0 {
                break;
            }
            let page_size = remaining.min(50);
            let page: RawPaging<RawPlaylistItem> = self
                .get(&format!("/playlists/{id}/tracks?limit={page_size}&offset={offset}"))
                .await?;
            let received = page.items.len() as u32;
            for item in page.items {
                if let Some(raw) = item.track {
                    if raw.uri.is_some() {
                        tracks.push(map_track(raw));
                    }
                }
            }
            offset += page_size;
            if received < page_size || u64::from(offset) >= page.total {
                break;
            }
        }
        Ok(tracks)
    }

    /// The user's saved (liked) tracks.
    pub async fn saved_tracks(&self, limit: u32) -> Result<Vec<Track>> {
        let page: RawPaging<RawSavedTrack> =
            self.get(&format!("/me/tracks?limit={}", limit.min(50))).await?;
        Ok(page
            .items
            .into_iter()
            .filter_map(|saved| saved.track)
            .map(map_track)
            .collect())
    }

    /// Search tracks, up to `limit` results.
    pub async fn search_tracks(&self, query: &str, limit: u32) -> Result<Vec<Track>> {
        let encoded = urlencode(query);
        let raw: RawSearch = self
            .get(&format!("/search?type=track&limit={limit}&q={encoded}"))
            .await?;
        Ok(raw
            .tracks
            .map(|page| page.items.into_iter().map(map_track).collect())
            .unwrap_or_default())
    }

    /// The playback state, or `None` when nothing is playing anywhere.
    pub async fn playback(&self) -> Result<Option<Playback>> {
        for attempt in 0..2 {
            let token = self.tokens().access_token(self.http(), attempt > 0).await?;
            let response = self
                .http()
                .get(format!("{BASE_URL}/me/player"))
                .bearer_auth(token)
                .send()
                .await?;
            if response.status() == StatusCode::NO_CONTENT {
                return Ok(None);
            }
            match Self::check(response).await {
                Ok(response) => {
                    let raw: RawPlayback = response.json().await.map_err(ApiError::from)?;
                    return Ok(Some(map_playback(raw)));
                }
                Err(e) if e.is_auth() && attempt == 0 => continue,
                Err(e) => return Err(e),
            }
        }
        unreachable!("the loop returns on every branch");
    }

    /// Start playing `uris` on `device_id`, or on the active device.
    pub async fn play(&self, device_id: Option<&str>, uris: &[String]) -> Result<()> {
        let mut body = serde_json::Map::new();
        if !uris.is_empty() {
            body.insert("uris".to_string(), serde_json::json!(uris));
        }
        self.send(
            Method::PUT,
            &format!("/me/player/play{}", device_query(device_id)),
            Some(serde_json::Value::Object(body)),
        )
        .await
    }

    /// Play a context (a playlist or album), optionally starting at `offset_uri`.
    pub async fn play_context(
        &self,
        device_id: Option<&str>,
        context_uri: &str,
        offset_uri: Option<&str>,
    ) -> Result<()> {
        let mut body = serde_json::json!({ "context_uri": context_uri });
        if let Some(offset_uri) = offset_uri {
            body["offset"] = serde_json::json!({ "uri": offset_uri });
        }
        self.send(
            Method::PUT,
            &format!("/me/player/play{}", device_query(device_id)),
            Some(body),
        )
        .await
    }

    pub async fn pause(&self, device_id: Option<&str>) -> Result<()> {
        self.send(Method::PUT, &format!("/me/player/pause{}", device_query(device_id)), None)
            .await
    }

    pub async fn next(&self, device_id: Option<&str>) -> Result<()> {
        self.send(Method::POST, &format!("/me/player/next{}", device_query(device_id)), None)
            .await
    }

    pub async fn previous(&self, device_id: Option<&str>) -> Result<()> {
        self.send(
            Method::POST,
            &format!("/me/player/previous{}", device_query(device_id)),
            None,
        )
        .await
    }

    pub async fn set_volume(&self, percent: u8, device_id: Option<&str>) -> Result<()> {
        self.send(
            Method::PUT,
            &format!(
                "/me/player/volume?volume_percent={}{}",
                percent,
                device_query(device_id)
            ),
            None,
        )
        .await
    }

    pub async fn toggle_shuffle(&self, state: bool, device_id: Option<&str>) -> Result<()> {
        self.send(
            Method::PUT,
            &format!("/me/player/shuffle?state={state}{}", device_query(device_id)),
            None,
        )
        .await
    }

    pub async fn set_repeat(&self, mode: &str, device_id: Option<&str>) -> Result<()> {
        self.send(
            Method::PUT,
            &format!("/me/player/repeat?state={mode}{}", device_query(device_id)),
            None,
        )
        .await
    }

    /// The user's available Spotify Connect devices.
    pub async fn devices(&self) -> Result<Vec<Device>> {
        let raw: RawDeviceList = self.get("/me/player/devices").await?;
        Ok(raw.devices.into_iter().map(map_device).collect())
    }

    /// Transfer playback to `device_id`, optionally keeping it playing.
    pub async fn transfer(&self, device_id: &str, play: bool) -> Result<()> {
        let body = serde_json::json!({ "device_ids": [device_id], "play": play });
        self.send(Method::PUT, "/me/player", Some(body)).await
    }

    /// Add a track to the queue.
    pub async fn queue_add(&self, uri: &str) -> Result<()> {
        self.send(
            Method::POST,
            &format!("/me/player/queue?uri={}", urlencode(uri)),
            None,
        )
        .await
    }
}

#[derive(Debug, Deserialize)]
struct RawDeviceList {
    #[serde(default)]
    devices: Vec<RawDevice>,
}

fn device_query(device_id: Option<&str>) -> String {
    device_id.map(|d| format!("?device_id={d}")).unwrap_or_default()
}

/// Percent-encode a query parameter value.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
