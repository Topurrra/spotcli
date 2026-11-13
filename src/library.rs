//! Catalogue reads over the streaming session's own protocol.
//!
//! The streaming session speaks the protocol Spotify's own clients use,
//! which has no per-app quota. Web API playlist-track reads are restricted
//! for apps in development mode (every playlist answers 403 Forbidden), so
//! playlist contents come from here instead — the same reason Spotifast
//! grew its `session_reads`. The Web API still handles what it can: liked
//! tracks, search and playback control.

use futures::StreamExt;
use futures::stream;
use librespot::core::Session;
use librespot::core::spotify_id::SpotifyId;
use librespot::core::spotify_uri::SpotifyUri;
use librespot::metadata::Track as SessionTrack;
use librespot::metadata::Metadata as _;
use librespot::protocol::playlist4_external::SelectedListContent;
use protobuf::Message as _;

use crate::models::Track;

/// Read a playlist's tracks over the streaming session. Episodes and local
/// files are skipped; the result is ordered like the playlist.
pub async fn playlist_tracks(
    session: &Session,
    playlist_id: &str,
    limit: usize,
) -> anyhow::Result<Vec<Track>> {
    anyhow::ensure!(!playlist_id.is_empty(), "playlist has no id");
    let id = SpotifyId::from_base62(playlist_id)
        .map_err(|e| anyhow::anyhow!("not a valid playlist id: {e}"))?;
    let bytes = session.spclient().get_playlist(&id).await?;
    let list = SelectedListContent::parse_from_bytes(&bytes)?;

    let contents = list
        .contents
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("playlist response had no contents"))?;
    let uris: Vec<(usize, String)> = contents
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| Some((index, item.uri.clone()?)))
        .take(limit)
        .collect();

    // Metadata arrives per track from spclient; a little parallelism keeps
    // opening a playlist quick, and results go back into playlist order.
    let fetches = stream::iter(uris)
        .map(|(index, uri)| {
            let session = session.clone();
            async move {
                let Ok(track_uri) = SpotifyUri::from_uri(&uri) else {
                    return None;
                };
                if !matches!(track_uri, SpotifyUri::Track { .. }) {
                    // Episodes, local files and other non-tracks cannot
                    // stream here.
                    return None;
                }
                let track = SessionTrack::get(&session, &track_uri).await.ok()?;
                Some((
                    index,
                    Track {
                        id: track_uri.to_string(),
                        uri,
                        name: track.name.clone(),
                        artists: artists_label(&track.artists),
                        album: track.album.name.clone(),
                        duration_ms: u64::try_from(track.duration.max(0)).unwrap_or(0),
                        cover_url: cover_url_from_covers(&track.album.covers),
                    },
                ))
            }
        })
        .buffer_unordered(8)
        .filter_map(|entry| async move { entry })
        .collect::<Vec<_>>()
        .await;

    let mut tracks: Vec<(usize, Track)> = fetches;
    tracks.sort_by_key(|(index, _)| *index);
    Ok(tracks.into_iter().map(|(_, track)| track).collect())
}

/// The image URL of the smallest usable cover in an album's cover group.
fn cover_url_from_covers(covers: &librespot::metadata::image::Images) -> Option<String> {
    let mut covers: Vec<_> = covers.0.iter().collect();
    covers.sort_by_key(|image| image.width);
    covers
        .iter()
        .find(|image| image.width >= 120)
        .or_else(|| covers.first())
        .map(|image| {
            let file_id = image.id;
            // FileId renders as base16 hex, the form the image CDN accepts.
            format!("https://i.scdn.co/image/{file_id}")
        })
}

fn artists_label(artists: &librespot::metadata::artist::Artists) -> String {
    artists
        .0
        .iter()
        .map(|artist| artist.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Search tracks over the streaming session (no Web API quota, so no
/// development-mode 403s or shared 503s). Results resolve the top `limit`
/// matches with full metadata, in search order.
pub async fn search(session: &Session, query: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
    anyhow::ensure!(!query.trim().is_empty(), "empty query");
    let context_uri = format!("spotify:search:{}", query.replace(' ', "+"));
    let context = session.spclient().get_context(&context_uri).await?;

    let mut uris: Vec<String> = Vec::new();
    for page in &context.pages {
        for track in &page.tracks {
            // The context carries the match order; entries without a uri are
            // placeholder rows (ads, empty slots).
            let Some(uri) = track.uri.as_ref() else {
                continue;
            };
            if matches!(SpotifyUri::from_uri(uri), Ok(SpotifyUri::Track { .. }))
                && !uris.contains(uri)
            {
                uris.push(uri.clone());
                if uris.len() >= limit {
                    break;
                }
            }
        }
        if uris.len() >= limit {
            break;
        }
    }

    let fetches = stream::iter(uris.into_iter().take(limit))
        .map(|uri| {
            let session = session.clone();
            async move {
                let Ok(track_uri) = SpotifyUri::from_uri(&uri) else {
                    return None;
                };
                let track = SessionTrack::get(&session, &track_uri).await.ok()?;
                Some(Track {
                    id: track_uri.to_string(),
                    uri,
                    name: track.name.clone(),
                    artists: artists_label(&track.artists),
                    album: track.album.name.clone(),
                    duration_ms: u64::try_from(track.duration.max(0)).unwrap_or(0),
                    cover_url: cover_url_from_covers(&track.album.covers),
                })
            }
        })
        .buffer_unordered(8)
        .filter_map(|entry| async move { entry })
        .collect::<Vec<_>>()
        .await;
    Ok(fetches)
}

/// Resolve spclient track metadata for `uris`, in order, skipping entries
/// that fail (unavailable region-locked or removed tracks).
async fn resolve(session: &Session, uris: Vec<String>) -> Vec<Track> {
    stream::iter(uris.into_iter())
        .map(|uri| {
        let session = session.clone();
        async move {
            let Ok(track_uri) = SpotifyUri::from_uri(&uri) else {
                return None;
            };
            if !matches!(track_uri, SpotifyUri::Track { .. }) {
                return None;
            }
            let track = SessionTrack::get(&session, &track_uri).await.ok()?;
            Some((
                track_uri.to_string(),
                Track {
                    id: track_uri.to_string(),
                    uri,
                    name: track.name.clone(),
                    artists: artists_label(&track.artists),
                    album: track.album.name.clone(),
                    duration_ms: u64::try_from(track.duration.max(0)).unwrap_or(0),
                    cover_url: cover_url_from_covers(&track.album.covers),
                },
            ))
            }
        })
        .buffer_unordered(8)
        .filter_map(|entry| async move { entry })
        .map(|(_, track)| track)
        .collect::<Vec<_>>()
        .await
}

/// What Spotify itself would queue after `seed_uri`: its radio continuation
/// for the seed (a track, album or playlist uri), minus the `recent` uris.
pub async fn radio_tracks(
    session: &Session,
    seed_uri: &str,
    recent: Vec<String>,
) -> anyhow::Result<Vec<Track>> {
    let request = librespot::protocol::autoplay_context_request::AutoplayContextRequest {
        context_uri: Some(seed_uri.to_string()),
        recent_track_uri: recent.clone(),
        ..Default::default()
    };
    let context = session.spclient().get_autoplay_context(&request).await?;
    let mut uris: Vec<String> = Vec::new();
    for page in &context.pages {
        for track in &page.tracks {
            let Some(uri) = track.uri.as_ref() else {
                continue;
            };
            if matches!(SpotifyUri::from_uri(uri), Ok(SpotifyUri::Track { .. }))
                && !recent.contains(uri)
                && !uris.contains(uri)
            {
                uris.push(uri.clone());
                if uris.len() >= 30 {
                    break;
                }
            }
        }
        if uris.len() >= 30 {
            break;
        }
    }
    Ok(resolve(session, uris).await)
}

/// An artist's name and their most streamed tracks, over the session.
pub async fn artist_top_tracks(
    session: &Session,
    artist_uri: &str,
) -> anyhow::Result<(String, Vec<Track>)> {
    let uri = SpotifyUri::from_uri(artist_uri)
        .map_err(|e| anyhow::anyhow!("not an artist uri: {artist_uri} ({e})"))?;
    if !matches!(uri, SpotifyUri::Artist { .. }) {
        anyhow::bail!("not an artist uri: {artist_uri}");
    }
    let artist = librespot::metadata::Artist::get(session, &uri).await?;
    let uris = artist
        .top_tracks
        .0
        .first()
        .map(|top| top.tracks.0.clone())
        .unwrap_or_default();
    let uris = uris
        .iter()
        .take(20)
        .map(|track_uri| track_uri.to_string())
        .collect();
    let tracks = resolve(session, uris).await;
    Ok((artist.name, tracks))
}

/// The View tab for a playing track: its artist's name and top tracks,
/// resolved through the track's metadata.
pub async fn view_catalog(
    session: &Session,
    track_uri: &str,
) -> anyhow::Result<(String, Vec<Track>)> {
    let uri = SpotifyUri::from_uri(track_uri)
        .map_err(|e| anyhow::anyhow!("not a track uri: {track_uri} ({e})"))?;
    let track = librespot::metadata::Track::get(session, &uri).await?;
    let artist_uri = track
        .artists
        .0
        .first()
        .map(|artist| artist.id.to_string())
        .ok_or_else(|| anyhow::anyhow!("track has no artist: {track_uri}"))?;
    artist_top_tracks(session, &artist_uri).await
}

/// Synced lyrics for a track, as the ColorLyrics endpoint serves them.
#[derive(Clone, Debug)]
pub struct Lyrics {
    pub provider: String,
    /// `(start time in ms, text)`, ordered by time.
    pub lines: Vec<(u64, String)>,
}

impl Lyrics {
    /// The index of the line active at `progress_ms`.
    pub fn active_line(&self, progress_ms: u64) -> usize {
        self.lines
            .iter()
            .rposition(|(start, _)| *start <= progress_ms)
            .unwrap_or(0)
    }
}

pub async fn lyrics(session: &Session, track_uri: &str) -> anyhow::Result<Option<Lyrics>> {
    let track_id: SpotifyId = track_uri
        .split(':')
        .next_back()
        .and_then(|id| SpotifyId::from_base62(id).ok())
        .ok_or_else(|| anyhow::anyhow!("not a track uri: {track_uri}"))?;
    let bytes = session.spclient().get_lyrics(&track_id).await?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let lines = value
        .pointer("/lyrics/lines")
        .and_then(|lines| lines.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|line| {
                    let time = line
                        .get("startTimeMs")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<u64>().ok())?;
                    let content = line.get("words")?.as_str()?.to_string();
                    Some((time, content))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if lines.is_empty() {
        return Ok(None);
    }
    let provider = value
        .pointer("/lyrics/provider")
        .and_then(|v| v.as_str())
        .unwrap_or("Musixmatch")
        .to_string();
    Ok(Some(Lyrics { provider, lines }))
}
