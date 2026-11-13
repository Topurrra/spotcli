//! An authenticated, rate-limit-aware transport for the Spotify Web API.

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client as HttpClient, Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::auth;
use crate::models::{
    Album, Artist, Device, Playback, Playlist, RawDevice, RawFullAlbum, RawPaging, RawPlayback,
    RawPlaylist, RawPlaylistItem, RawSavedTrack, RawSearch, RawTopTracks, RawUser, Track, User,
    map_device, map_playback, map_playlist, map_search_album, map_search_artist, map_track,
};

const BASE_URL: &str = "https://api.spotify.com/v1";
const PAGE_SIZE: u32 = 50;
/// Upper bound on playlists loaded for one user.
const PLAYLIST_LIMIT: u32 = 500;

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

    /// An HTTP status of `code`.
    fn has_status(&self, code: u16) -> bool {
        matches!(self, ApiError::Status { status, .. } if *status == code)
    }

    /// Turn a failed write call (like, add to playlist) into a user-facing
    /// error: a refused token needs the new scopes from a fresh login.
    pub fn into_write_error(self) -> anyhow::Error {
        match self {
            ApiError::SessionExpired | ApiError::Status { status: 401 | 403, .. } => {
                anyhow::anyhow!("Missing permission — run `spotcli login` again")
            }
            other => other.into(),
        }
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

    /// Send one authenticated request, handling the two retryable failures:
    /// a 401 (refresh the token once, unless another request already did) and,
    /// for idempotent GETs only, a 429 (honor `Retry-After`, capped, once).
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response> {
        let idempotent = method == Method::GET;
        let mut rejected: Option<String> = None;
        let mut refreshed = false;
        let mut rate_retried = false;
        loop {
            let token = self
                .tokens()
                .access_token(self.http(), rejected.as_deref())
                .await?;
            let mut request = self
                .http()
                .request(method.clone(), format!("{BASE_URL}{path}"))
                .bearer_auth(&token);
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await?;
            match Self::check(response).await {
                Ok(response) => return Ok(response),
                Err(ApiError::SessionExpired) if !refreshed => {
                    refreshed = true;
                    rejected = Some(token);
                }
                Err(ApiError::RateLimited { retry_after }) if idempotent && !rate_retried => {
                    rate_retried = true;
                    let wait = retry_after.unwrap_or(Duration::from_secs(1));
                    tokio::time::sleep(wait.min(Duration::from_secs(5))).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// GET a JSON document.
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.request(Method::GET, path, None).await?;
        response.json().await.map_err(ApiError::from)
    }

    /// PUT or POST, with an optional JSON body, ignoring the empty reply.
    async fn send(&self, method: Method, path: &str, body: Option<serde_json::Value>) -> Result<()> {
        self.request(method, path, body.as_ref()).await.map(|_| ())
    }

    /// Fetch up to `limit` items of a paged collection (`path` has no query
    /// string). The first page reports `total`; the rest are fetched a few at
    /// a time and kept in order.
    async fn paged<T: DeserializeOwned>(&self, path: &str, limit: u32) -> Result<Vec<T>> {
        use futures::StreamExt as _;
        let first_size = limit.clamp(1, PAGE_SIZE);
        let first: RawPaging<T> = self
            .get(&format!("{path}?limit={first_size}&offset=0"))
            .await?;
        let wanted = u64::from(limit).min(first.total);
        let mut items = first.items;
        let mut offsets = Vec::new();
        let mut offset = u64::from(first_size);
        while offset < wanted {
            offsets.push(offset);
            offset += u64::from(PAGE_SIZE);
        }
        let pages: Vec<Result<RawPaging<T>>> = futures::stream::iter(offsets)
            .map(|offset| async move {
                self.get::<RawPaging<T>>(&format!("{path}?limit={PAGE_SIZE}&offset={offset}"))
                    .await
            })
            .buffered(4)
            .collect()
            .await;
        for page in pages {
            let page = page?;
            let done = page.items.is_empty();
            items.extend(page.items);
            if done {
                break;
            }
        }
        items.truncate(limit as usize);
        Ok(items)
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

    /// The user's playlists, following pagination (capped at 500).
    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        let items: Vec<RawPlaylist> = self.paged("/me/playlists", PLAYLIST_LIMIT).await?;
        Ok(items.into_iter().map(map_playlist).collect())
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
                    if is_playable(&raw) {
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

    /// The user's saved (liked) tracks, following pagination up to `limit`.
    pub async fn saved_tracks(&self, limit: u32) -> Result<Vec<Track>> {
        let items: Vec<RawSavedTrack> = self.paged("/me/tracks", limit).await?;
        Ok(items
            .into_iter()
            .filter_map(|saved| saved.track)
            .filter(is_playable)
            .map(map_track)
            .collect())
    }

    /// Search tracks, up to `limit` results.
    pub async fn search_tracks(&self, query: &str, limit: u32) -> Result<Vec<Track>> {
        Ok(self.search_tracks_page(query, limit, 0).await?.0)
    }

    /// One page of track search results starting at `offset`, plus the offset
    /// of the next page when more matches exist. Development-mode apps may be
    /// capped to smaller pages (a 400 for the limit), so a refused limit is
    /// retried once at 10.
    pub async fn search_tracks_page(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Track>, Option<u32>)> {
        let raw = self.search_raw("track", query, limit, offset).await?;
        let Some(page) = raw.tracks else {
            return Ok((Vec::new(), None));
        };
        let received = page.items.len() as u32;
        let next = (received > 0 && u64::from(offset + received) < page.total)
            .then_some(offset + received);
        let tracks = page.items.into_iter().filter(is_playable).map(map_track).collect();
        Ok((tracks, next))
    }

    /// Artists, albums and playlists matching `query` in one request.
    pub async fn search_extras(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<(Vec<Artist>, Vec<Album>, Vec<Playlist>)> {
        let raw = self.search_raw("artist,album,playlist", query, limit, 0).await?;
        let artists = raw
            .artists
            .map(|p| p.items.into_iter().flatten().map(map_search_artist).collect())
            .unwrap_or_default();
        let albums = raw
            .albums
            .map(|p| p.items.into_iter().flatten().map(map_search_album).collect())
            .unwrap_or_default();
        let playlists = raw
            .playlists
            .map(|p| p.items.into_iter().flatten().map(map_playlist).collect())
            .unwrap_or_default();
        Ok((artists, albums, playlists))
    }

    async fn search_raw(&self, kinds: &str, query: &str, limit: u32, offset: u32) -> Result<RawSearch> {
        let encoded = urlencode(query);
        let path = |limit: u32| {
            format!("/search?type={kinds}&limit={limit}&offset={offset}&q={encoded}")
        };
        match self.get(&path(limit)).await {
            Err(e) if e.has_status(400) && limit > 10 => self.get(&path(10)).await,
            other => other,
        }
    }

    /// An album's tracks (the first 50), each carrying the album's name and art.
    pub async fn album_tracks(&self, id: &str) -> Result<Vec<Track>> {
        let album: RawFullAlbum = self.get(&format!("/albums/{id}")).await?;
        let cover = crate::models::smallest_cover(&album.images).and_then(|i| i.url.clone());
        let name = album.name.unwrap_or_default();
        Ok(album
            .tracks
            .map(|page| page.items)
            .unwrap_or_default()
            .into_iter()
            .filter(is_playable)
            .map(|raw| {
                let mut track = map_track(raw);
                track.album = name.clone();
                track.cover_url = cover.clone();
                track
            })
            .collect())
    }

    /// An artist's top tracks in the user's market.
    pub async fn artist_top_tracks(&self, id: &str) -> Result<Vec<Track>> {
        let top: RawTopTracks = self
            .get(&format!("/artists/{id}/top-tracks?market=from_token"))
            .await?;
        Ok(top.tracks.into_iter().filter(is_playable).map(map_track).collect())
    }

    /// Save tracks to the user's Liked Songs. Falls back to the newer
    /// library endpoint when `/me/tracks` is gone (404).
    pub async fn save_tracks(&self, ids: &[String]) -> Result<()> {
        let body = serde_json::json!({ "ids": ids });
        match self.send(Method::PUT, "/me/tracks", Some(body)).await {
            Err(e) if e.has_status(404) => {
                let uris: Vec<String> = ids.iter().map(|id| format!("spotify:track:{id}")).collect();
                self.send(
                    Method::PUT,
                    &format!("/me/library?uris={}", urlencode(&uris.join(","))),
                    None,
                )
                .await
            }
            other => other,
        }
    }

    /// Append tracks to a playlist. Uses `/items`, retrying the legacy
    /// `/tracks` path once when the first answers 404.
    pub async fn playlist_add(&self, playlist_id: &str, uris: &[String]) -> Result<()> {
        let body = serde_json::json!({ "uris": uris });
        match self
            .send(Method::POST, &format!("/playlists/{playlist_id}/items"), Some(body.clone()))
            .await
        {
            Err(e) if e.has_status(404) => {
                self.send(Method::POST, &format!("/playlists/{playlist_id}/tracks"), Some(body))
                    .await
            }
            other => other,
        }
    }

    /// The playback state, or `None` when nothing is playing anywhere.
    pub async fn playback(&self) -> Result<Option<Playback>> {
        let response = self.request(Method::GET, "/me/player", None).await?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let raw: RawPlayback = response.json().await.map_err(ApiError::from)?;
        Ok(Some(map_playback(raw)))
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

/// A track the player can stream: a real `spotify:track:` uri (local files
/// and null-id entries have none).
fn is_playable(raw: &crate::models::RawTrack) -> bool {
    raw.uri
        .as_deref()
        .is_some_and(|uri| uri.starts_with("spotify:track:") && uri.len() > "spotify:track:".len())
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
