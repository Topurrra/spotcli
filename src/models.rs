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
    /// Smallest usable album-art URL, for the now-playing panel.
    pub cover_url: Option<String>,
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
    /// The owner's Spotify user id (empty when the payload omitted it).
    pub owner_id: String,
    /// Anyone invited can add tracks.
    pub collaborative: bool,
    pub track_count: u64,
}

/// An artist search result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Artist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub followers: Option<u64>,
}

/// An album search result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Album {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artists: String,
    /// Release year, or empty when unknown.
    pub year: String,
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
    #[serde(default)]
    pub images: Vec<RawImage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawImage {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPlaylist {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub collaborative: bool,
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
    pub id: Option<String>,
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
    #[serde(default)]
    pub artists: Option<RawPaging<Option<RawSearchArtist>>>,
    #[serde(default)]
    pub albums: Option<RawPaging<Option<RawSearchAlbum>>>,
    /// Playlist pages contain `null` holes for removed playlists.
    #[serde(default)]
    pub playlists: Option<RawPaging<Option<RawPlaylist>>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawFollowers {
    #[serde(default)]
    pub total: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSearchArtist {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub followers: Option<RawFollowers>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSearchAlbum {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub artists: Vec<RawArtist>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub total_tracks: Option<u64>,
}

/// GET /albums/{id}: the album with its first page of tracks.
#[derive(Debug, Deserialize)]
pub(crate) struct RawFullAlbum {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub images: Vec<RawImage>,
    #[serde(default)]
    pub tracks: Option<RawPaging<RawTrack>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTopTracks {
    #[serde(default)]
    pub tracks: Vec<RawTrack>,
}

pub(crate) fn map_search_artist(raw: RawSearchArtist) -> Artist {
    Artist {
        id: raw.id.unwrap_or_default(),
        uri: raw.uri.unwrap_or_default(),
        name: raw.name.unwrap_or_else(|| "(unknown artist)".to_string()),
        followers: raw.followers.and_then(|f| f.total),
    }
}

pub(crate) fn map_search_album(raw: RawSearchAlbum) -> Album {
    Album {
        id: raw.id.unwrap_or_default(),
        uri: raw.uri.unwrap_or_default(),
        name: raw.name.unwrap_or_else(|| "(unknown album)".to_string()),
        artists: artists_label(&raw.artists),
        year: raw
            .release_date
            .map(|d| d.chars().take(4).collect())
            .unwrap_or_default(),
        track_count: raw.total_tracks.unwrap_or_default(),
    }
}

fn artists_label(artists: &[RawArtist]) -> String {
    artists
        .iter()
        .filter_map(|a| a.name.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn map_track(raw: RawTrack) -> Track {
    let cover_url = raw
        .album
        .as_ref()
        .and_then(|album| smallest_cover(&album.images).and_then(|image| image.url.clone()));
    Track {
        id: raw.id.unwrap_or_default(),
        uri: raw.uri.unwrap_or_default(),
        name: raw.name.unwrap_or_else(|| "(unknown track)".to_string()),
        artists: artists_label(&raw.artists),
        album: raw.album.and_then(|a| a.name).unwrap_or_default(),
        duration_ms: raw.duration_ms.unwrap_or_default(),
        cover_url,
    }
}

/// The smallest image that is still big enough for a 16x8 cell panel; tiny
/// covers avoid pulling 640px jpegs for a two-digit pixel render.
pub(crate) fn smallest_cover(images: &[RawImage]) -> Option<&RawImage> {
    let mut usable: Vec<&RawImage> = images
        .iter()
        .filter(|image| image.url.is_some())
        .collect();
    usable.sort_by_key(|image| image.width.unwrap_or(u32::MAX));
    usable
        .iter()
        .find(|image| image.width.unwrap_or(0) >= 120)
        .or_else(|| usable.first())
        .copied()
}

pub(crate) fn map_playlist(raw: RawPlaylist) -> Playlist {
    Playlist {
        id: raw.id.unwrap_or_default(),
        uri: raw.uri.unwrap_or_default(),
        name: raw.name.unwrap_or_else(|| "(unnamed playlist)".to_string()),
        owner_id: raw.owner.as_ref().and_then(|o| o.id.clone()).unwrap_or_default(),
        owner: raw.owner.and_then(|o| o.display_name).unwrap_or_default(),
        collaborative: raw.collaborative,
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
    fn picks_the_smallest_usable_cover() {
        let images = vec![
            RawImage { url: Some("small".into()), width: Some(64) },
            RawImage { url: Some("mid".into()), width: Some(300) },
            RawImage { url: Some("big".into()), width: Some(640) },
        ];
        assert_eq!(smallest_cover(&images).unwrap().url.as_deref(), Some("mid"));
        let tiny = vec![
            RawImage { url: Some("small".into()), width: Some(64) },
        ];
        assert_eq!(smallest_cover(&tiny).unwrap().url.as_deref(), Some("small"));
    }

    #[test]
    fn maps_search_extras_and_tolerates_null_playlists() {
        let raw: RawSearch = serde_json::from_str(
            r#"{
                "artists": {"items": [{"id": "a1", "uri": "spotify:artist:a1", "name": "Band", "followers": {"total": 42}}], "total": 1},
                "albums": {"items": [{"id": "b1", "uri": "spotify:album:b1", "name": "LP", "artists": [{"name": "Band"}], "release_date": "2019-05-03", "total_tracks": 11}], "total": 1},
                "playlists": {"items": [null, {"id": "p1", "uri": "spotify:playlist:p1", "name": "Mix", "owner": {"id": "me", "display_name": "Me"}, "collaborative": true}], "total": 2}
            }"#,
        )
        .unwrap();
        let artist = map_search_artist(raw.artists.unwrap().items.into_iter().flatten().next().unwrap());
        assert_eq!((artist.name.as_str(), artist.followers), ("Band", Some(42)));
        let album = map_search_album(raw.albums.unwrap().items.into_iter().flatten().next().unwrap());
        assert_eq!((album.year.as_str(), album.track_count, album.artists.as_str()), ("2019", 11, "Band"));
        let playlists: Vec<_> = raw.playlists.unwrap().items.into_iter().flatten().map(map_playlist).collect();
        assert_eq!(playlists.len(), 1);
        assert!(playlists[0].collaborative);
        assert_eq!(playlists[0].owner_id, "me");
    }

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
