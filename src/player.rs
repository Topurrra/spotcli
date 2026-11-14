//! The built-in streaming player.
//!
//! The Web API can only *command* external Spotify devices; it never sends
//! audio. This module adds the audio: here spotcli becomes the Spotify
//! device itself through [librespot](https://github.com/librespot-org/librespot)
//! (the open-source Spotify client library), streaming straight to the
//! machine's speakers via rodio/cpal.
//!
//! Authentication reuses the streaming credential banked by `spotcli login`.
//! The player keeps its own queue (the list shown in the TUI), auto-advances
//! at the end of a track, and reports state changes to the TUI event loop as
//! [`LocalSignal`]s.
//!
//! A librespot [`Session`] cannot be reused once its connection drops
//! (network change, sleep, a missed keep-alive). Left alone, every load after
//! that fails as "unavailable" and the queue skips itself to the end, so the
//! player rebuilds its session and player on a dead connection and retries
//! the track instead of skipping it.
//!
//! Streaming tracks requires a Spotify Premium account; with a Free account
//! tracks fail to load and surface as `LocalSignal::Error`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use librespot::core::Session;
use librespot::core::config::SessionConfig;
use librespot::core::spotify_uri::SpotifyUri;
use librespot::playback::audio_backend;
use librespot::playback::config::{AudioFormat, PlayerConfig};
use librespot::playback::mixer::Mixer as _;
use librespot::playback::mixer::softmixer::SoftMixer;
use librespot::playback::player::{Player, PlayerEvent};
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedSender;

use crate::models::Track;

/// Load failures in a row (with no track starting in between) before the
/// player suspects the connection and reconnects; after one more failure it
/// stops instead of skipping through the whole queue.
const MAX_FAILURES: u32 = 3;

/// Connection attempts per reconnect, with a growing pause between them.
const RECONNECT_ATTEMPTS: u32 = 4;

/// State changes the TUI should redraw for.
#[derive(Clone, Debug)]
pub enum LocalSignal {
    /// The playing track, progress or pause state may have moved.
    Status,
    /// The queue ran to its end.
    Ended,
    /// The last requested track could not be streamed.
    Error(String),
    /// Something worth a status-line message that is not a failure.
    Notice(String),
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

/// What happens when a track finishes in the built-in player.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Repeat {
    /// Play the queue once, then hand over to radio.
    #[default]
    Off,
    /// Start the queue over after its last track.
    All,
    /// Play the current track again.
    One,
}

#[derive(Default)]
struct PlayerState {
    repeat: Repeat,
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
    /// Tracks that failed to load since the last one started playing.
    failures: u32,
    /// The player was rebuilt while paused and holds no track yet: resuming
    /// must load the current entry first.
    needs_load: bool,
}

impl PlayerState {
    /// The playhead now, interpolated while playing.
    fn live_position(&self) -> u64 {
        let elapsed = if self.is_playing {
            u64::try_from(self.at.map(|at| at.elapsed().as_millis()).unwrap_or(0)).unwrap_or(0)
        } else {
            0
        };
        self.position_ms.saturating_add(elapsed)
    }
}

/// One connected librespot session and the player streaming through it.
struct Core {
    session: Session,
    player: Arc<Player>,
}

impl Core {
    fn is_dead(&self) -> bool {
        self.session.is_invalid() || self.player.is_invalid()
    }
}

struct Shared {
    dir: PathBuf,
    handle: Handle,
    core: RwLock<Arc<Core>>,
    /// Bumped on every reconnect; watchers of an older player go quiet.
    generation: AtomicU64,
    reconnecting: AtomicBool,
    mixer: SoftMixer,
    state: Mutex<PlayerState>,
    notify: Mutex<Option<UnboundedSender<LocalSignal>>>,
}

impl Shared {
    fn core(&self) -> Arc<Core> {
        self.core.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, PlayerState> {
        // The only lock contention is between the watcher task and short UI
        // actions; a poisoned lock means a panic while holding it, which the
        // next action should still survive.
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn send(&self, signal: LocalSignal) {
        if let Some(tx) = self.notify.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            let _ = tx.send(signal);
        }
    }

    /// Whether `id` belongs to the most recent load.
    fn is_current(&self, id: u64) -> bool {
        let guard = self.state();
        guard.pending_loads == 0 && guard.request_id == Some(id)
    }

    /// Load the current queue entry at `position_ms`. On a dead connection
    /// this reconnects first, and the reconnect loads it. Returns `false`
    /// when there is no current entry or its uri does not parse.
    fn load_current(self: &Arc<Self>, start_playing: bool, position_ms: u64) -> bool {
        let track = {
            let guard = self.state();
            guard.queue.get(guard.index).cloned()
        };
        let Some(track) = track else { return false };
        let Ok(track_uri) = SpotifyUri::from_uri(&track.uri) else {
            return false;
        };
        let core = self.core();
        if core.is_dead() {
            self.spawn_reconnect(true);
            return true;
        }
        {
            let mut guard = self.state();
            guard.pending_loads += 1;
            guard.request_id = None;
            guard.needs_load = false;
        }
        core.player
            .load(track_uri, start_playing, u32::try_from(position_ms).unwrap_or(u32::MAX));
        true
    }

    /// Move to the next playable queue entry and start it, skipping entries
    /// whose uri does not parse. Returns `true` when the queue is exhausted.
    /// `finished` is true when the current track played to its end
    /// (repeat-one replays it then; an unavailable track is always skipped).
    fn advance(self: &Arc<Self>, finished: bool) -> bool {
        // Bounded so a queue of nothing but unplayable entries cannot spin
        // forever under repeat-all.
        let attempts = self.state().queue.len().max(1);
        for attempt in 0..attempts {
            {
                let mut guard = self.state();
                let index = match guard.repeat {
                    Repeat::One if finished && attempt == 0 => guard.index,
                    Repeat::All if guard.index + 1 >= guard.queue.len() => 0,
                    _ => guard.index + 1,
                };
                if index >= guard.queue.len() {
                    guard.is_playing = false;
                    return true;
                }
                guard.index = index;
                guard.is_playing = true;
                guard.position_ms = 0;
                guard.at = Some(Instant::now());
            }
            if self.load_current(true, 0) {
                return false;
            }
        }
        self.state().is_playing = false;
        true
    }

    /// Rebuild the session and player in the background. With `reload`, the
    /// current entry is loaded again where it was once connected.
    fn spawn_reconnect(self: &Arc<Self>, reload: bool) {
        if self.reconnecting.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = self.clone();
        self.handle.spawn(async move { shared.reconnect(reload).await });
    }

    async fn reconnect(self: Arc<Self>, reload: bool) {
        self.send(LocalSignal::Notice("Connection to Spotify lost; reconnecting…".into()));
        // Silence the old watcher before stopping its player, so the stop
        // does not read as a finished track.
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.core().player.stop();

        let mut last_error = None;
        let mut fresh = None;
        for attempt in 0..RECONNECT_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2 * u64::from(attempt))).await;
            }
            match open_core(&self.dir, &self.mixer).await {
                Ok(core) => {
                    fresh = Some(core);
                    break;
                }
                Err(e) => last_error = Some(e),
            }
        }
        let Some(core) = fresh else {
            self.state().is_playing = false;
            self.reconnecting.store(false, Ordering::SeqCst);
            let e = last_error.map(|e| e.to_string()).unwrap_or_default();
            self.send(LocalSignal::Error(format!(
                "Could not reconnect to Spotify ({e}); press space to try again"
            )));
            self.send(LocalSignal::Status);
            return;
        };

        let core = Arc::new(core);
        let old = std::mem::replace(
            &mut *self.core.write().unwrap_or_else(|p| p.into_inner()),
            core.clone(),
        );
        if !old.session.is_invalid() {
            old.session.shutdown();
        }
        // Dropping a player joins its thread: keep that off the async workers.
        self.handle.spawn_blocking(move || drop(old));
        spawn_watcher(&self, &core, generation);

        let (playing, position) = {
            let mut guard = self.state();
            // Loads issued to the old player never get their id announced.
            guard.pending_loads = 0;
            guard.request_id = None;
            let position = guard.live_position();
            guard.position_ms = position;
            guard.at = Some(Instant::now());
            (guard.is_playing, position)
        };
        self.reconnecting.store(false, Ordering::SeqCst);
        if reload {
            self.load_current(playing, position);
        } else {
            self.state().needs_load = true;
        }
        self.send(LocalSignal::Notice("Reconnected to Spotify".into()));
        self.send(LocalSignal::Status);
    }

    fn handle_event(self: &Arc<Self>, event: PlayerEvent) {
        if let PlayerEvent::PlayRequestIdChanged { play_request_id } = event {
            let mut guard = self.state();
            guard.pending_loads = guard.pending_loads.saturating_sub(1);
            if guard.pending_loads == 0 {
                guard.request_id = Some(play_request_id);
            }
            return;
        }
        if let Some(id) = event.get_play_request_id() {
            if !self.is_current(id) {
                return;
            }
        }
        match event {
            PlayerEvent::Playing { position_ms, .. } => {
                let mut guard = self.state();
                guard.is_playing = true;
                guard.failures = 0;
                guard.position_ms = u64::from(position_ms);
                guard.at = Some(Instant::now());
                drop(guard);
                self.send(LocalSignal::Status);
            }
            PlayerEvent::Paused { position_ms, .. } => {
                let mut guard = self.state();
                guard.is_playing = false;
                guard.position_ms = u64::from(position_ms);
                guard.at = Some(Instant::now());
                drop(guard);
                self.send(LocalSignal::Status);
            }
            PlayerEvent::Stopped { .. } | PlayerEvent::EndOfTrack { .. } => {
                let ended = self.advance(true);
                self.send(if ended { LocalSignal::Ended } else { LocalSignal::Status });
            }
            PlayerEvent::Unavailable { track_id, .. } => {
                let failures = {
                    let mut guard = self.state();
                    guard.failures += 1;
                    guard.failures
                };
                if failures > MAX_FAILURES {
                    // Even a fresh connection could not play anything: stop
                    // rather than burn through the rest of the queue.
                    {
                        let mut guard = self.state();
                        guard.is_playing = false;
                        guard.failures = 0;
                    }
                    self.send(LocalSignal::Error(format!(
                        "{failures} tracks in a row failed to load; stopped. Press space to retry"
                    )));
                } else if self.core().is_dead() || failures == MAX_FAILURES {
                    // A dropped connection, or several failures in a row
                    // that look like one: reconnect and retry this track.
                    {
                        let mut guard = self.state();
                        guard.position_ms = 0;
                        guard.at = Some(Instant::now());
                    }
                    self.spawn_reconnect(true);
                } else {
                    let ended = self.advance(false);
                    self.send(if ended {
                        LocalSignal::Ended
                    } else {
                        LocalSignal::Error(format!("{track_id} is unavailable; skipping"))
                    });
                }
            }
            _ => {}
        }
    }
}

/// Connect a session with the stored streaming credential and open a
/// player on it.
async fn open_core(dir: &Path, mixer: &SoftMixer) -> anyhow::Result<Core> {
    let credentials = crate::auth::streaming_credentials(dir).await?;
    let cache = librespot::core::cache::Cache::new(Some(dir.join("streaming")), None, None, None)?;
    let session = Session::new(SessionConfig::default(), Some(cache));
    session.connect(credentials, true).await.map_err(|e| {
        if e.to_string().contains("Bad credentials") {
            anyhow::anyhow!(
                "Spotify refused the streaming session (the stored grant was \
                 rejected). Run `spotcli logout` and then `spotcli login` again."
            )
        } else {
            anyhow::anyhow!("could not open a streaming session: {e}")
        }
    })?;
    let sink_builder = audio_backend::find(None)
        .ok_or_else(|| anyhow::anyhow!("no audio backend available in this build"))?;
    let player = Player::new(
        PlayerConfig::default(),
        session.clone(),
        mixer.get_soft_volume(),
        move || sink_builder(None, AudioFormat::default()),
    );
    Ok(Core { session, player })
}

/// Forward `core`'s player events into the shared state until that player
/// is replaced or dropped. Holds only a weak handle, so it never keeps the
/// player alive on its own.
fn spawn_watcher(shared: &Arc<Shared>, core: &Core, generation: u64) {
    let mut events = core.player.get_player_event_channel();
    let weak: Weak<Shared> = Arc::downgrade(shared);
    shared.handle.spawn(async move {
        while let Some(event) = events.recv().await {
            let Some(shared) = weak.upgrade() else { break };
            if shared.generation.load(Ordering::SeqCst) != generation {
                break;
            }
            shared.handle_event(event);
        }
    });
}

#[derive(Clone)]
pub struct LocalPlayer {
    shared: Arc<Shared>,
}

impl LocalPlayer {
    /// Authenticate with the reusable streaming credential banked by
    /// `spotcli login`, and open the audio device.
    pub async fn create(dir: &Path) -> anyhow::Result<Self> {
        let mixer = SoftMixer::open(Default::default())?;
        let core = open_core(dir, &mixer).await?;
        Ok(Self {
            shared: Arc::new(Shared {
                dir: dir.to_path_buf(),
                handle: Handle::current(),
                core: RwLock::new(Arc::new(core)),
                generation: AtomicU64::new(0),
                reconnecting: AtomicBool::new(false),
                mixer,
                state: Mutex::new(PlayerState::default()),
                notify: Mutex::new(None),
            }),
        })
    }

    /// Forward player events to `notify` while updating the shared state.
    /// Call once; reconnects keep reporting to the same `notify`.
    pub fn watch(&self, notify: UnboundedSender<LocalSignal>) {
        *self.shared.notify.lock().unwrap_or_else(|p| p.into_inner()) = Some(notify);
        let core = self.shared.core();
        spawn_watcher(&self.shared, &core, self.shared.generation.load(Ordering::SeqCst));
    }

    /// The session back-end, for protocol-level catalogue reads that the
    /// Web API refuses to development-mode apps (playlist contents). A dead
    /// session while nothing plays is replaced in the background, so the
    /// next read works again.
    pub fn session(&self) -> Session {
        let core = self.shared.core();
        if core.session.is_invalid() && !self.shared.state().is_playing {
            self.shared.spawn_reconnect(false);
        }
        core.session.clone()
    }

    /// Start playing the queue built from `tracks`, beginning at `start`.
    pub fn play_queue(&self, tracks: Vec<Track>, start: usize) -> anyhow::Result<()> {
        let Some(track) = tracks.get(start) else {
            anyhow::bail!("nothing selected to play");
        };
        let uri = track.uri.clone();
        {
            let mut guard = self.shared.state();
            guard.queue = tracks;
            guard.index = start;
            guard.is_playing = true;
            guard.position_ms = 0;
            guard.failures = 0;
            guard.at = Some(Instant::now());
        }
        if !self.shared.load_current(true, 0) {
            anyhow::bail!("not a playable Spotify uri: {uri}");
        }
        Ok(())
    }

    /// Pause or resume the current local track.
    pub fn toggle(&self) {
        let (was_playing, needs_load, position) = {
            let guard = self.shared.state();
            (guard.is_playing, guard.needs_load, guard.position_ms)
        };
        let core = self.shared.core();
        if was_playing {
            core.player.pause();
        } else if needs_load || core.is_dead() {
            // Nothing loaded on a rebuilt player, or a dead connection: load
            // the track again where it was paused.
            {
                let mut guard = self.shared.state();
                guard.is_playing = true;
                guard.failures = 0;
                guard.at = Some(Instant::now());
            }
            self.shared.load_current(true, position);
            return;
        } else {
            core.player.play();
        }
        let mut guard = self.shared.state();
        guard.is_playing = !was_playing;
        guard.at = Some(Instant::now());
    }

    /// Jump forward or backward in the queue. Going backward from the first
    /// entry wraps to the end, like the playlist cursors do.
    pub fn skip(&self, forward: bool) {
        let (index, len) = {
            let guard = self.shared.state();
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
        {
            let mut guard = self.shared.state();
            if index >= guard.queue.len() {
                return;
            }
            guard.index = index;
            guard.is_playing = true;
            guard.position_ms = 0;
            guard.failures = 0;
            guard.at = Some(Instant::now());
        }
        self.shared.load_current(true, 0);
    }

    /// Pause the current local track, keeping the position.
    pub fn pause(&self) {
        self.shared.core().player.pause();
        let mut guard = self.shared.state();
        guard.position_ms = guard.live_position();
        guard.is_playing = false;
        guard.at = Some(Instant::now());
    }

    /// The current queue entry, even when it finished playing.
    pub fn current(&self) -> Option<Track> {
        let guard = self.shared.state();
        guard.queue.get(guard.index).cloned()
    }

    /// The uris this queue visited so far, oldest first, for autoplay seed
    /// context.
    pub fn played_uris(&self) -> Vec<String> {
        let guard = self.shared.state();
        guard.queue[..= guard.index.min(guard.queue.len().saturating_sub(1))]
            .iter()
            .map(|track| track.uri.clone())
            .collect()
    }

    /// Choose what happens when a track ends.
    pub fn set_repeat(&self, repeat: Repeat) {
        self.shared.state().repeat = repeat;
    }

    /// Shuffle the part of the queue that has not played yet.
    pub fn shuffle_queue(&self) {
        use rand::seq::SliceRandom as _;
        let mut guard = self.shared.state();
        let range = guard.index.saturating_add(1)..guard.queue.len();
        if range.len() < 2 {
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
        let start = {
            let mut guard = self.shared.state();
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
            start
        };
        self.shared.load_current(true, 0);
        Some(start)
    }

    /// Nudge the playhead by `delta_ms` (negative rewinds), clamped to the
    /// current track.
    pub fn seek_by(&self, delta_ms: i64) {
        let mut guard = self.shared.state();
        let Some(track) = guard.queue.get(guard.index) else {
            return;
        };
        let duration = track.duration_ms as i64;
        let target = (guard.live_position() as i64 + delta_ms).clamp(0, duration) as u64;
        guard.position_ms = target;
        guard.at = Some(Instant::now());
        let needs_load = guard.needs_load;
        drop(guard);
        // A rebuilt player with nothing loaded picks the position up on resume.
        if !needs_load {
            self.shared
                .core()
                .player
                .seek(u32::try_from(target).unwrap_or(u32::MAX));
        }
    }

    /// Set the volume as a percentage, 0-100.
    pub fn set_volume_percent(&self, percent: u8) {
        let raw = (u32::from(percent.min(100)) * 65_535 / 100) as u16;
        self.shared.mixer.set_volume(raw);
    }

    /// Stop local playback and drop the connection to Spotify.
    pub fn shutdown(&self) {
        // Keep late watcher events and reconnects from restarting anything.
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.shared.reconnecting.store(true, Ordering::SeqCst);
        let core = self.shared.core();
        core.player.stop();
        core.session.shutdown();
    }

    /// The current local playback state, if any track is queued.
    pub fn snapshot(&self) -> Option<LocalStatus> {
        let guard = self.shared.state();
        let track = guard.queue.get(guard.index)?.clone();
        let volume_percent =
            (u32::from(self.shared.mixer.volume()) * 100 / 65_535).min(100) as u8;
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
