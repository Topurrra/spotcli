//! The built-in streaming player.
//!
//! The Web API can only *command* external Spotify devices; it never sends
//! audio. This module adds the audio: here spotcli becomes the Spotify
//! device itself through [librespot](https://github.com/librespot-org/librespot)
//! (the open-source Spotify client library), streaming straight to the
//! machine's speakers via rodio/cpal.
//!
//! Authentication reuses the OAuth access token from `spotcli login`, passed
//! to librespot as a Spotify token credential. The player keeps its own
//! queue (the list shown in the TUI), auto-advances at the end of a track,
//! and reports state changes to the TUI event loop as [`LocalSignal`]s.
//!
//! Streaming tracks requires a Spotify Premium account; with a Free account
//! tracks fail to load and surface as `LocalSignal::Error`.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use librespot::core::Session;
use librespot::core::config::SessionConfig;
use librespot::core::spotify_uri::SpotifyUri;
use librespot::playback::audio_backend;
use librespot::playback::config::{AudioFormat, PlayerConfig};
use librespot::playback::mixer::Mixer as _;
use librespot::playback::mixer::softmixer::SoftMixer;
use librespot::playback::player::{Player, PlayerEvent};
use tokio::runtime::Handle;

use crate::models::Track;

/// State changes the TUI should redraw for.
#[derive(Clone, Debug)]
pub enum LocalSignal {
    /// The playing track, progress or pause state may have moved.
    Status,
    /// The queue ran to its end.
    Ended,
    /// The last requested track could not be streamed.
    Error(String),
}

/// What the TUI sees of local playback at one instant.
#[derive(Clone, Debug)]
pub struct LocalStatus {
    pub track: Track,
    pub is_playing: bool,
    pub position_ms: u64,
    /// When `position_ms` was confirmed; the UI interpolates from here.
    pub progress_at: Option<Instant>,
    pub volume_percent: u8,
    pub queue_index: usize,
    pub queue_len: usize,
}

#[derive(Default)]
struct PlayerState {
    queue: Vec<Track>,
    index: usize,
    is_playing: bool,
    position_ms: u64,
    /// When `position_ms` was last confirmed by an event; the UI
    /// interpolates from here between events.
    at: Option<Instant>,
    /// `Player::load` calls whose `PlayRequestIdChanged` has not arrived yet.
    pending_loads: u32,
    /// The play request id of the most recent load; events for any other id
    /// are stale (a skipped track) and ignored.
    request_id: Option<u64>,
}

/// Note that a load is about to be issued: until its request id is announced,
/// every event is treated as stale.
fn begin_load(state: &Mutex<PlayerState>) {
    let mut guard = state_guard(state);
    guard.pending_loads += 1;
    guard.request_id = None;
}

/// Whether `id` belongs to the most recent load.
fn is_current(state: &Mutex<PlayerState>, id: u64) -> bool {
    let guard = state_guard(state);
    guard.pending_loads == 0 && guard.request_id == Some(id)
}

fn state_guard(state: &Mutex<PlayerState>) -> std::sync::MutexGuard<'_, PlayerState> {
    // The only lock contention is between the watcher task and short UI
    // actions; a poisoned lock means a panic while holding it, which the
    // next action should still survive.
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Move `state` to the next playable queue entry and start it, skipping
/// entries whose uri does not parse. Returns `true` when the queue is
/// exhausted.
fn advance(state: &Mutex<PlayerState>, player: &Arc<Player>) -> bool {
    loop {
        let next = {
            let mut guard = state_guard(state);
            let index = guard.index + 1;
            let Some(next) = guard.queue.get(index).cloned() else {
                guard.is_playing = false;
                return true;
            };
            guard.index = index;
            guard.is_playing = true;
            guard.position_ms = 0;
            guard.at = Some(Instant::now());
            next
        };
        if let Ok(track_uri) = SpotifyUri::from_uri(&next.uri) {
            begin_load(state);
            player.load(track_uri, true, 0);
            return false;
        }
    }
}

#[derive(Clone)]
pub struct LocalPlayer {
    inner: Arc<Inner>,
    handle: Handle,
}

struct Inner {
    session: Session,
    player: Arc<Player>,
    mixer: SoftMixer,
    state: Arc<Mutex<PlayerState>>,
}

impl LocalPlayer {
    /// Authenticate with the reusable streaming credential banked by
    /// `spotcli login`, and open the audio device.
    pub async fn create(dir: &std::path::Path) -> anyhow::Result<Self> {
        let credentials = crate::auth::streaming_credentials(dir).await?;
        let cache = librespot::core::cache::Cache::new(Some(dir.join("streaming")), None, None, None)?;
        let session = Session::new(SessionConfig::default(), Some(cache));
        session
            .connect(credentials, true)
            .await
            .map_err(|e| {
                if e.to_string().contains("Bad credentials") {
                    anyhow::anyhow!(
                        "Spotify refused the streaming session (the stored grant was \
                         rejected). Run `spotcli logout` and then `spotcli login` again."
                    )
                } else {
                    anyhow::anyhow!("could not open a streaming session: {e}")
                }
            })?;
        let mixer = SoftMixer::open(Default::default())?;
        let sink_builder = audio_backend::find(None)
            .ok_or_else(|| anyhow::anyhow!("no audio backend available in this build"))?;
        let player = Player::new(
            PlayerConfig::default(),
            session.clone(),
            mixer.get_soft_volume(),
            move || sink_builder(None, AudioFormat::default()),
        );
        Ok(Self {
            inner: Arc::new(Inner {
                session,
                player,
                mixer,
                state: Arc::new(Mutex::new(PlayerState::default())),
            }),
            handle: Handle::current(),
        })
    }

    /// Forward player events to `notify` while updating the shared state.
    /// The watcher lives until the player is dropped.
    pub fn watch(&self, notify: tokio::sync::mpsc::UnboundedSender<LocalSignal>) {
        let player = self.inner.player.clone();
        let state = self.inner.state.clone();
        let mut events = player.get_player_event_channel();
        self.handle.spawn(async move {
            while let Some(event) = events.recv().await {
                if let PlayerEvent::PlayRequestIdChanged { play_request_id } = event {
                    let mut guard = state_guard(&state);
                    guard.pending_loads = guard.pending_loads.saturating_sub(1);
                    if guard.pending_loads == 0 {
                        guard.request_id = Some(play_request_id);
                    }
                    continue;
                }
                if let Some(id) = event.get_play_request_id() {
                    if !is_current(&state, id) {
                        continue;
                    }
                }
                match event {
                    PlayerEvent::Playing { position_ms, .. } => {
                        let mut guard = state_guard(&state);
                        guard.is_playing = true;
                        guard.position_ms = u64::from(position_ms);
                        guard.at = Some(Instant::now());
                        drop(guard);
                        let _ = notify.send(LocalSignal::Status);
                    }
                    PlayerEvent::Paused { position_ms, .. } => {
                        let mut guard = state_guard(&state);
                        guard.is_playing = false;
                        guard.position_ms = u64::from(position_ms);
                        guard.at = Some(Instant::now());
                        drop(guard);
                        let _ = notify.send(LocalSignal::Status);
                    }
                    PlayerEvent::Stopped { .. } | PlayerEvent::EndOfTrack { .. } => {
                        let ended = advance(&state, &player);
                        let _ = notify.send(if ended {
                            LocalSignal::Ended
                        } else {
                            LocalSignal::Status
                        });
                    }
                    PlayerEvent::Unavailable { track_id, .. } => {
                        let ended = advance(&state, &player);
                        let _ = notify.send(if ended {
                            LocalSignal::Ended
                        } else {
                            LocalSignal::Error(format!("{track_id} is unavailable; skipping"))
                        });
                    }
                    _ => {}
                }
            }
        });
    }

    /// The session back-end, for protocol-level catalogue reads that the
    /// Web API refuses to development-mode apps (playlist contents).
    pub fn session(&self) -> Session {
        self.inner.session.clone()
    }

    /// Start playing the queue built from `tracks`, beginning at `start`.
    pub fn play_queue(&self, tracks: Vec<Track>, start: usize) -> anyhow::Result<()> {
        let Some(track) = tracks.get(start) else {
            anyhow::bail!("nothing selected to play");
        };
        let uri = track.uri.clone();
        {
            let mut guard = state_guard(&self.inner.state);
            guard.queue = tracks;
            guard.index = start;
            guard.is_playing = true;
            guard.position_ms = 0;
            guard.at = Some(Instant::now());
        }
        self.load_uri(&uri, true)
    }

    /// Pause or resume the current local track.
    pub fn toggle(&self) {
        let was_playing = state_guard(&self.inner.state).is_playing;
        if was_playing {
            self.inner.player.pause();
        } else {
            self.inner.player.play();
        }
        let mut guard = state_guard(&self.inner.state);
        guard.is_playing = !was_playing;
        guard.at = Some(Instant::now());
    }

    /// Jump forward or backward in the queue. Going backward from the first
    /// entry wraps to the end, like the playlist cursors do.
    pub fn skip(&self, forward: bool) {
        let (index, len) = {
            let guard = state_guard(&self.inner.state);
            (guard.index, guard.queue.len())
        };
        if len == 0 {
            return;
        }
        let next = wrap(index as i64 + if forward { 1 } else { -1 }, len as i64) as usize;
        self.jump_to(next);
    }

    /// Restart playback at `index` of the current queue.
    pub fn jump_to(&self, index: usize) {
        let track = state_guard(&self.inner.state).queue.get(index).cloned();
        let Some(track) = track else { return };
        {
            let mut guard = state_guard(&self.inner.state);
            guard.index = index;
            guard.is_playing = true;
            guard.position_ms = 0;
            guard.at = Some(Instant::now());
        }
        let _ = self.load_uri(&track.uri, true);
    }

    /// Pause the current local track, keeping the position.
    pub fn pause(&self) {
        self.inner.player.pause();
        let mut guard = state_guard(&self.inner.state);
        guard.is_playing = false;
        guard.at = Some(Instant::now());
    }

    /// The current queue entry, even when it finished playing.
    pub fn current(&self) -> Option<Track> {
        let guard = state_guard(&self.inner.state);
        guard.queue.get(guard.index).cloned()
    }

    /// The uris this queue visited so far, oldest first, for autoplay seed
    /// context.
    pub fn played_uris(&self) -> Vec<String> {
        let guard = state_guard(&self.inner.state);
        guard.queue[..= guard.index.min(guard.queue.len().saturating_sub(1))]
            .iter()
            .map(|track| track.uri.clone())
            .collect()
    }

    /// Shuffle the part of the queue that has not played yet.
    pub fn shuffle_queue(&self) {
        use rand::seq::SliceRandom as _;
        let mut guard = state_guard(&self.inner.state);
        let range = guard.index.saturating_add(1)..guard.queue.len();
        if range.len() < 2 {
            drop(guard);
            return;
        }
        let start = guard.index + 1;
        let mut unplayed = guard.queue.split_off(start);
        unplayed.shuffle(&mut rand::rng());
        guard.queue.append(&mut unplayed);
    }

    /// Append `tracks` to the queue. When nothing is playing (the queue ran
    /// out), the first appended track starts immediately and its index in
    /// the queue is returned.
    pub fn extend_queue(&self, tracks: Vec<Track>) -> Option<usize> {
        if tracks.is_empty() {
            return None;
        }
        let mut guard = state_guard(&self.inner.state);
        let start = guard.queue.len();
        guard.queue.extend(tracks);
        if guard.is_playing {
            return None;
        }
        // Queue had run to its end: carry straight on with the new tracks.
        guard.index = start;
        guard.is_playing = true;
        guard.position_ms = 0;
        guard.at = Some(Instant::now());
        let next = guard.queue.get(start)?.clone();
        drop(guard);
        let _ = self.load_uri(&next.uri, true);
        Some(start)
    }

    /// Nudge the playhead by `delta_ms` (negative rewinds), clamped to the
    /// current track.
    pub fn seek_by(&self, delta_ms: i64) {
        let mut guard = state_guard(&self.inner.state);
        let Some(track) = guard.queue.get(guard.index) else {
            return;
        };
        let elapsed = if guard.is_playing {
            u64::try_from(guard.at.map(|at| at.elapsed().as_millis()).unwrap_or(0)).unwrap_or(0)
        } else {
            0
        };
        let current = guard.position_ms.saturating_add(elapsed);
        let target = (current as i64 + delta_ms)
            .clamp(0, track.duration_ms as i64) as u64;
        guard.position_ms = target;
        guard.at = Some(Instant::now());
        drop(guard);
        self.inner.player.seek(u32::try_from(target).unwrap_or(u32::MAX));
    }

    /// Set the volume as a percentage, 0-100.
    pub fn set_volume_percent(&self, percent: u8) {
        let raw = (u32::from(percent.min(100)) * 65_535 / 100) as u16;
        self.inner.mixer.set_volume(raw);
    }

    /// Stop local playback and drop the connection to Spotify.
    pub fn shutdown(&self) {
        self.inner.player.stop();
        self.inner.session.shutdown();
    }

    /// The current local playback state, if any track is queued.
    pub fn snapshot(&self) -> Option<LocalStatus> {
        let guard = state_guard(&self.inner.state);
        let track = guard.queue.get(guard.index)?.clone();
        let volume_percent = (u32::from(self.inner.mixer.volume()) * 100 / 65_535).min(100) as u8;
        Some(LocalStatus {
            track,
            is_playing: guard.is_playing,
            position_ms: guard.position_ms,
            progress_at: guard.at,
            volume_percent,
            queue_index: guard.index,
            queue_len: guard.queue.len(),
        })
    }

    fn load_uri(&self, uri: &str, start_playing: bool) -> anyhow::Result<()> {
        let track_uri = SpotifyUri::from_uri(uri)
            .map_err(|e| anyhow::anyhow!("not a playable Spotify uri: {uri} ({e})"))?;
        begin_load(&self.inner.state);
        self.inner.player.load(track_uri, start_playing, 0);
        Ok(())
    }
}

/// Wrap an index around `len`, in either direction.
fn wrap(index: i64, len: i64) -> i64 {
    index.rem_euclid(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_percent_maps_to_u16() {
        // Percentages at the edges map to the edges of librespot's u16 scale.
        assert_eq!((u32::from(100u8) * 65_535 / 100) as u16, 65_535);
        assert_eq!((u32::from(0u8) * 65_535 / 100) as u16, 0);
        assert_eq!((u32::from(50u8) * 65_535 / 100) as u16, 32_767);
    }

    #[test]
    fn wrap_matches_playlist_cursors() {
        assert_eq!(wrap(0, 3), 0);
        assert_eq!(wrap(-1, 3), 2);
        assert_eq!(wrap(3, 3), 0);
    }
}

