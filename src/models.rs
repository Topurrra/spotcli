use serde::Deserialize;


/// A playable track, flattened from whatever endpoint produced it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub duration_ms: u64,
}

impl Track {
    pub fn duration_label(&self) -> String {
        format_duration(self.duration_ms)
    }
}

/// A playlist entry in the library view.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub owner: String,
    pub track_count: u64,
}

/// A Spotify Connect device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub is_active: bool,
    pub volume_percent: Option<u8>,
}

/// The signed-in user's profile, reduced to what the UI shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct User {
    pub id: String,
    pub display_name: String,
    pub country: String,
    pub product: String,
}

/// The playback state this client cares about, from GET /me/player.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playback {
    pub track: Option<Track>,
    pub is_playing: bool,
    pub progress_ms: u64,
    pub duration_ms: u64,
    pub shuffle: bool,
    pub repeat: String,
    pub volume_percent: Option<u8>,
    pub device_name: String,
}

impl Playback {
    pub fn progress_label(&self) -> String {
        format!("{} / {}", format_duration(self.progress_ms), format_duration(self.duration_ms))
    }
}

/// `mm:ss`, or `h:mm:ss` for anything an hour or longer.
pub fn format_duration(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

// --- Raw Spotify payloads -------------------------------------------------
// The Web API omits or nulls fields freely, so every field is optional and
// mapped into the flat models above.

#[derive(Debug, Deserialize)]
pub(crate) struct RawTrack {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub artists: Vec<RawArtist>,
    #[serde(default)]
    pub album: Option<RawAlbum>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawArtist {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawAlbum {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPlaylist {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub owner: Option<RawOwner>,
    #[serde(default)]
    pub tracks: Option<RawPlaylistTracks>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawOwner {
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPlaylistTracks {
    #[serde(default)]
    pub total: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPaging<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub total: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPlaylistItem {
    #[serde(default)]
    pub track: Option<RawTrack>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSavedTrack {
    #[serde(default)]
    pub track: Option<RawTrack>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawUser {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawDevice {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub volume_percent: Option<u8>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPlayback {
    #[serde(default)]
    pub device: Option<RawDevice>,
    #[serde(default)]
    pub is_playing: bool,
    #[serde(default)]
    pub shuffle_state: bool,
    #[serde(default)]
    pub repeat_state: Option<String>,
    #[serde(default)]
    pub progress_ms: Option<u64>,
    #[serde(default)]
    pub item: Option<RawTrack>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSearch {
    #[serde(default)]
    pub tracks: Option<RawPaging<RawTrack>>,
}

fn artists_label(artists: &[RawArtist]) -> String {
    artists
        .iter()
        .filter_map(|a| a.name.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn map_track(raw: RawTrack) -> Track {
    Track {
        id: raw.id.unwrap_or_default(),
        uri: raw.uri.unwrap_or_default(),
        name: raw.name.unwrap_or_else(|| "(unknown track)".to_string()),
        artists: artists_label(&raw.artists),
        album: raw.album.and_then(|a| a.name).unwrap_or_default(),
        duration_ms: raw.duration_ms.unwrap_or_default(),
    }
}

pub(crate) fn map_playlist(raw: RawPlaylist) -> Playlist {
    Playlist {
        id: raw.id.unwrap_or_default(),
        uri: raw.uri.unwrap_or_default(),
        name: raw.name.unwrap_or_else(|| "(unnamed playlist)".to_string()),
        owner: raw.owner.and_then(|o| o.display_name).unwrap_or_default(),
        track_count: raw.tracks.map(|t| t.total).unwrap_or_default(),
    }
}

pub(crate) fn map_device(raw: RawDevice) -> Device {
    Device {
        id: raw.id.unwrap_or_default(),
        name: raw.name.unwrap_or_else(|| "(unknown device)".to_string()),
        kind: raw.kind.unwrap_or_default(),
        is_active: raw.is_active,
        volume_percent: raw.volume_percent,
    }
}

pub(crate) fn map_playback(raw: RawPlayback) -> Playback {
    let device_name = raw
        .device
        .as_ref()
        .map(|d| d.name.clone().unwrap_or_default())
        .unwrap_or_default();
    let volume_percent = raw.device.as_ref().and_then(|d| d.volume_percent);
    let duration_ms = raw.item.as_ref().and_then(|t| t.duration_ms).unwrap_or_default();
    Playback {
        track: raw.item.map(map_track),
        is_playing: raw.is_playing,
        progress_ms: raw.progress_ms.unwrap_or_default(),
        duration_ms,
        shuffle: raw.shuffle_state,
        repeat: raw.repeat_state.unwrap_or_else(|| "off".to_string()),
        volume_percent,
        device_name,
    }
}

/// Parse a track list from a search or playlist response, skipping null
/// entries (local files and removed tracks arrive as `"track": null`).
#[cfg(test)]
pub(crate) fn tracks_from_items<T>(items: Vec<T>, f: impl Fn(T) -> Option<RawTrack>) -> Vec<Track> {
    items.into_iter().filter_map(f).map(map_track).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(65_000), "1:05");
        assert_eq!(format_duration(3_600_000), "1:00:00");
        assert_eq!(format_duration(3_725_000), "1:02:05");
    }

    #[test]
    fn maps_a_track_payload() {
        let raw: RawTrack = serde_json::from_str(
            r#"{
                "id": "abc", "uri": "spotify:track:abc", "name": "Song",
                "duration_ms": 200000,
                "artists": [{"name": "A"}, {"name": "B"}],
                "album": {"name": "Album"}
            }"#,
        )
        .unwrap();
        let track = map_track(raw);
        assert_eq!(track.name, "Song");
        assert_eq!(track.artists, "A, B");
        assert_eq!(track.album, "Album");
        assert_eq!(track.duration_ms, 200_000);
    }

    #[test]
    fn maps_a_playback_payload() {
        let raw: RawPlayback = serde_json::from_str(
            r#"{
                "device": {"id": "d1", "name": "Laptop", "type": "Computer", "is_active": true, "volume_percent": 70},
                "is_playing": true, "shuffle_state": false, "repeat_state": "off",
                "progress_ms": 30000,
                "item": {"id": "abc", "uri": "spotify:track:abc", "name": "Song", "duration_ms": 200000}
            }"#,
        )
        .unwrap();
        let pb = map_playback(raw);
        assert_eq!(pb.device_name, "Laptop");
        assert_eq!(pb.volume_percent, Some(70));
        assert_eq!(pb.track.as_ref().unwrap().name, "Song");
        assert_eq!(pb.duration_ms, 200_000);
    }

    #[test]
    fn skips_null_playlist_tracks() {
        let page: RawPaging<RawPlaylistItem> = serde_json::from_str(
            r#"{"items": [{"track": null}, {"track": {"id": "x", "name": "Real"}}], "total": 2}"#,
        )
        .unwrap();
        let tracks = tracks_from_items(page.items, |i| i.track);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].name, "Real");
    }
}
