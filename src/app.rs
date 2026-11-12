//! Terminal event loop and application state.
//!
//! The UI thread never blocks on the network: every request runs as a task
//! on the tokio runtime and reports back through a channel that the event
//! loop drains between draws. Controls update optimistically, the way
//! Spotifast does: the shown state moves the moment a key is pressed, and
//! Spotify's own state catches up behind it.

use std::future::Future;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::runtime::Handle;

use crate::api::Client;
use crate::models::{Device, Playback, Playlist, Track};

/// How long the event loop waits for terminal input between draws.
const TICK: Duration = Duration::from_millis(100);
/// How often the real playback state is fetched.
const PLAYBACK_POLL: Duration = Duration::from_secs(5);
/// How long a transient status message stays on screen.
pub const STATUS_TTL: Duration = Duration::from_secs(4);

/// How many liked tracks are loaded.
const LIBRARY_LIMIT: u32 = 200;
/// How many search results are fetched.
const SEARCH_LIMIT: u32 = 30;

/// The key of the built-in liked-tracks list in the library view.
pub const LIKED_KEY: &str = "liked";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Library,
    Search,
}

/// A message from a background task.
pub enum AppEvent {
    Playback(Result<Option<Playback>>),
    Playlists(Result<Vec<Playlist>>),
    Tracks { key: String, tracks: Vec<Track> },
    SearchResults(Vec<Track>),
    Devices(Vec<Device>),
    Done { action: &'static str, outcome: Result<()> },
}

type EventTx = mpsc::Sender<AppEvent>;

/// Submits background jobs against the API client.
#[derive(Clone)]
struct Jobs {
    client: Client,
    handle: Handle,
    tx: EventTx,
}

impl Jobs {
    fn submit<T, M>(&self, job: impl Future<Output = Result<T>> + Send + 'static, map: M)
    where
        T: Send + 'static,
        M: FnOnce(Result<T>) -> AppEvent + Send + 'static,
    {
        let tx = self.tx.clone();
        self.handle.spawn(async move {
            let _ = tx.send(map(job.await));
        });
    }
}

pub struct App {
    pub tab: Tab,
    pub should_quit: bool,

    pub(crate) playlists: Vec<Playlist>,
    pub(crate) playlists_cursor: usize,
    pub(crate) tracks: Vec<Track>,
    pub(crate) tracks_cursor: usize,
    pub(crate) tracks_key: Option<String>,
    pub(crate) tracks_focused: bool,

    pub(crate) search_input: String,
    pub(crate) search_results: Vec<Track>,
    pub(crate) search_cursor: usize,

    pub(crate) playback: Option<Playback>,
    pub(crate) playback_at: Option<Instant>,
    pub(crate) devices: Vec<Device>,
    pub(crate) device_index: usize,

    pub(crate) status: Option<(String, Instant)>,
    pub(crate) pending: usize,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            tab: Tab::Library,
            should_quit: false,
            playlists: Vec::new(),
            playlists_cursor: 0,
            tracks: Vec::new(),
            tracks_cursor: 0,
            tracks_key: None,
            tracks_focused: false,
            search_input: String::new(),
            search_results: Vec::new(),
            search_cursor: 0,
            playback: None,
            playback_at: None,
            devices: Vec::new(),
            device_index: 0,
            status: None,
            pending: 0,
        }
    }

    /// Run the event loop until the user quits.
    pub fn run(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
        client: Client,
        handle: Handle,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel::<AppEvent>();
        let jobs = Jobs { client, handle, tx };
        self.bootstrap(&jobs);

        let mut last_poll = Instant::now();
        while !self.should_quit {
            terminal.draw(|frame| crate::ui::draw(frame, self))?;

            if event::poll(TICK)? {
                if let TermEvent::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key, &jobs);
                    }
                }
            }

            while let Ok(event) = rx.try_recv() {
                self.on_event(event);
            }

            if last_poll.elapsed() >= PLAYBACK_POLL {
                self.refresh_playback(&jobs);
                last_poll = Instant::now();
            }
        }
        Ok(())
    }

    fn bootstrap(&mut self, jobs: &Jobs) {
        self.refresh_playlists(jobs);
        self.refresh_playback(jobs);
        self.refresh_devices(jobs);
    }

    fn say(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }

    // --- Background refreshes -------------------------------------------------

    fn refresh_playlists(&mut self, jobs: &Jobs) {
        self.pending += 1;
        let client = jobs.client.clone();
        jobs.submit(async move { client.playlists().await.map_err(anyhow::Error::from) }, AppEvent::Playlists);
    }

    fn refresh_playback(&mut self, jobs: &Jobs) {
        self.pending += 1;
        let client = jobs.client.clone();
        jobs.submit(async move { client.playback().await.map_err(anyhow::Error::from) }, AppEvent::Playback);
    }

    fn refresh_devices(&mut self, jobs: &Jobs) {
        self.pending += 1;
        let client = jobs.client.clone();
        jobs.submit(
            async move { client.devices().await.map_err(anyhow::Error::from) },
            |result| match result {
                Ok(devices) => AppEvent::Devices(devices),
                Err(e) => AppEvent::Done {
                    action: "devices",
                    outcome: Err(e),
                },
            },
        );
    }

    fn load_tracks(&mut self, jobs: &Jobs, key: &str) {
        self.pending += 1;
        self.tracks_key = Some(key.to_string());
        let client = jobs.client.clone();
        let key = key.to_string();
        jobs.submit(
            async move {
                let tracks = if key == LIKED_KEY {
                    client.saved_tracks(LIBRARY_LIMIT).await
                } else {
                    client.playlist_tracks(&key, LIBRARY_LIMIT).await
                };
                tracks
                    .map(|tracks| (key, tracks))
                    .map_err(anyhow::Error::from)
            },
            |result| match result {
                Ok((key, tracks)) => AppEvent::Tracks { key, tracks },
                Err(e) => AppEvent::Done {
                    action: "load tracks",
                    outcome: Err(e),
                },
            },
        );
    }

    fn submit_search(&mut self, jobs: &Jobs) {
        let query = self.search_input.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.pending += 1;
        let client = jobs.client.clone();
        jobs.submit(
            async move { client.search_tracks(&query, SEARCH_LIMIT).await.map_err(anyhow::Error::from) },
            |result| match result {
                Ok(tracks) => AppEvent::SearchResults(tracks),
                Err(e) => AppEvent::Done {
                    action: "search",
                    outcome: Err(e),
                },
            },
        );
    }

    // --- Keys ------------------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent, jobs: &Jobs) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('c') {
                self.should_quit = true;
            }
            return;
        }
        match self.tab {
            Tab::Library => self.on_key_library(key, jobs),
            Tab::Search => self.on_key_search(key, jobs),
        }
    }

    fn on_key_library(&mut self, key: KeyEvent, jobs: &Jobs) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => self.tab = Tab::Search,
            KeyCode::Left | KeyCode::Char('h') => self.tracks_focused = false,
            KeyCode::Right | KeyCode::Char('l') => self.tracks_focused = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Enter => self.play_selection(jobs),
            KeyCode::Char(' ') => self.toggle_playback(jobs),
            KeyCode::Char('n') => self.skip(jobs, true),
            KeyCode::Char('p') => self.skip(jobs, false),
            KeyCode::Char('s') => self.toggle_shuffle(jobs),
            KeyCode::Char('r') => self.cycle_repeat(jobs),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_volume(jobs, 5),
            KeyCode::Char('-') => self.change_volume(jobs, -5),
            KeyCode::Char('a') => self.queue_selection(jobs),
            KeyCode::Char('d') => self.next_device(jobs),
            _ => {}
        }
    }

    fn on_key_search(&mut self, key: KeyEvent, jobs: &Jobs) {
        match key.code {
            KeyCode::Char('q') if self.search_input.is_empty() => self.should_quit = true,
            KeyCode::Tab => self.tab = Tab::Library,
            KeyCode::Esc => {
                self.search_input.clear();
                self.search_cursor = 0;
            }
            KeyCode::Enter => self.submit_search(jobs),
            KeyCode::Backspace => {
                self.search_input.pop();
                self.search_cursor = 0;
            }
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Char(' ') => self.toggle_playback(jobs),
            KeyCode::Char('n') => self.skip(jobs, true),
            KeyCode::Char('p') => self.skip(jobs, false),
            KeyCode::Char(c) => {
                self.search_input.push(c);
                self.search_cursor = 0;
            }
            _ => {}
        }
    }

    /// Move the focused list's cursor by `delta`, wrapping at the ends.
    fn move_cursor(&mut self, delta: i32) {
        let (cursor, len) = match self.tab {
            Tab::Library => {
                if self.tracks_focused {
                    (&mut self.tracks_cursor, self.tracks.len())
                } else {
                    (&mut self.playlists_cursor, self.playlists.len())
                }
            }
            Tab::Search => (&mut self.search_cursor, self.search_results.len()),
        };
        if len == 0 {
            return;
        }
        *cursor = wrap(*cursor as i64 + delta as i64, len as i64) as usize;
    }

    // --- Playback actions (optimistic) ------------------------------------------

    fn selected_track(&self) -> Option<Track> {
        match self.tab {
            Tab::Library if self.tracks_focused => self.tracks.get(self.tracks_cursor).cloned(),
            Tab::Search => self.search_results.get(self.search_cursor).cloned(),
            _ => None,
        }
    }

    fn play_selection(&mut self, jobs: &Jobs) {
        match self.tab {
            Tab::Library if !self.tracks_focused => {
                let Some(playlist) = self.playlists.get(self.playlists_cursor) else {
                    return;
                };
                let name = playlist.name.clone();
                let uri = playlist.uri.clone();
                let id = playlist.id.clone();
                let jobs = jobs.clone();
                self.tracks_focused = true;
                self.tracks_cursor = 0;
                self.load_tracks(&jobs, &id);
                self.say(format!("Opening {name}..."));
                let client = jobs.client.clone();
                jobs.submit(
                    async move { client.play_context(None, &uri, None).await.map_err(anyhow::Error::from) },
                    done("play playlist"),
                );
            }
            _ => {
                let Some(track) = self.selected_track() else {
                    return;
                };
                let uri = track.uri.clone();
                self.set_playing_track(&track);
                let client = jobs.client.clone();
                jobs.submit(
                    async move { client.play(None, &[uri]).await.map_err(anyhow::Error::from) },
                    done("play track"),
                );
            }
        }
    }

    fn set_playing_track(&mut self, track: &Track) {
        let playback = self.playback.get_or_insert_with(Playback::default);
        playback.track = Some(track.clone());
        playback.is_playing = true;
        playback.progress_ms = 0;
        playback.duration_ms = track.duration_ms;
        self.playback_at = Some(Instant::now());
    }

    fn toggle_playback(&mut self, jobs: &Jobs) {
        let Some(playback) = self.playback.as_mut() else {
            self.say("Nothing playing yet; press Enter on a track first");
            return;
        };
        let now_playing = playback.is_playing;
        playback.is_playing = !now_playing;
        if playback.is_playing {
            self.playback_at = Some(Instant::now());
        }
        let client = jobs.client.clone();
        jobs.submit(
            async move {
                if now_playing {
                    client.pause(None).await
                } else {
                    client.play(None, &[]).await
                }
                .map_err(anyhow::Error::from)
            },
            done("toggle playback"),
        );
    }

    fn skip(&mut self, jobs: &Jobs, forward: bool) {
        if let Some(playback) = self.playback.as_mut() {
            playback.progress_ms = 0;
            self.playback_at = Some(Instant::now());
        }
        let client = jobs.client.clone();
        jobs.submit(
            async move {
                if forward {
                    client.next(None).await
                } else {
                    client.previous(None).await
                }
                .map_err(anyhow::Error::from)
            },
            done("skip"),
        );
    }

    fn toggle_shuffle(&mut self, jobs: &Jobs) {
        let Some(playback) = self.playback.as_mut() else {
            return;
        };
        playback.shuffle = !playback.shuffle;
        let state = playback.shuffle;
        let client = jobs.client.clone();
        jobs.submit(
            async move { client.toggle_shuffle(state, None).await.map_err(anyhow::Error::from) },
            done("shuffle"),
        );
    }

    fn cycle_repeat(&mut self, jobs: &Jobs) {
        let Some(playback) = self.playback.as_mut() else {
            return;
        };
        playback.repeat = match playback.repeat.as_str() {
            "off" => "context".to_string(),
            "context" => "track".to_string(),
            _ => "off".to_string(),
        };
        let mode = playback.repeat.clone();
        let client = jobs.client.clone();
        jobs.submit(
            async move { client.set_repeat(&mode, None).await.map_err(anyhow::Error::from) },
            done("repeat"),
        );
    }

    fn change_volume(&mut self, jobs: &Jobs, delta: i32) {
        let Some(playback) = self.playback.as_mut() else {
            return;
        };
        let current = playback.volume_percent.unwrap_or(50) as i32;
        let next = (current + delta).clamp(0, 100) as u8;
        playback.volume_percent = Some(next);
        let client = jobs.client.clone();
        jobs.submit(
            async move { client.set_volume(next, None).await.map_err(anyhow::Error::from) },
            done("volume"),
        );
    }

    fn queue_selection(&mut self, jobs: &Jobs) {
        let Some(track) = self.selected_track() else {
            return;
        };
        self.say(format!("Queued {}", track.name));
        let uri = track.uri.clone();
        let client = jobs.client.clone();
        jobs.submit(async move { client.queue_add(&uri).await.map_err(anyhow::Error::from) }, done("queue"));
    }

    /// Transfer playback to the next known device.
    fn next_device(&mut self, jobs: &Jobs) {
        if self.devices.len() < 2 {
            self.say("No other Spotify Connect device found");
            return;
        }
        self.device_index = (self.device_index + 1) % self.devices.len();
        let device = self.devices[self.device_index].clone();
        self.say(format!("Switching to {}", device.name));
        let id = device.id.clone();
        let client = jobs.client.clone();
        jobs.submit(
            async move { client.transfer(&id, true).await.map_err(anyhow::Error::from) },
            done("transfer"),
        );
    }

    // --- Events -----------------------------------------------------------------

    fn on_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Playback(result) => {
                self.pending = self.pending.saturating_sub(1);
                match result {
                    Ok(Some(playback)) => {
                        self.playback_at = Some(Instant::now());
                        self.playback = Some(playback);
                    }
                    Ok(None) => {}
                    Err(e) => self.say(format!("Playback: {e}")),
                }
            }
            AppEvent::Playlists(result) => {
                self.pending = self.pending.saturating_sub(1);
                match result {
                    Ok(playlists) => self.playlists = playlists,
                    Err(e) => self.say(format!("Playlists: {e}")),
                }
            }
            AppEvent::Tracks { key, tracks } => {
                self.pending = self.pending.saturating_sub(1);
                if self.tracks_key.as_deref() == Some(key.as_str()) {
                    self.tracks = tracks;
                    self.tracks_cursor = self.tracks_cursor.min(self.tracks.len().saturating_sub(1));
                }
            }
            AppEvent::SearchResults(tracks) => {
                self.pending = self.pending.saturating_sub(1);
                let count = tracks.len();
                self.search_results = tracks;
                self.search_cursor = 0;
                self.say(format!("{count} result{}", if count == 1 { "" } else { "s" }));
            }
            AppEvent::Devices(devices) => {
                self.pending = self.pending.saturating_sub(1);
                self.devices = devices;
            }
            AppEvent::Done { action, outcome } => {
                self.pending = self.pending.saturating_sub(1);
                if let Err(e) = outcome {
                    self.say(format!("{action}: {e}"));
                }
            }
        }
    }
}

/// Map a finished job into a generic `Done` event.
fn done(action: &'static str) -> impl FnOnce(Result<()>) -> AppEvent {
    move |outcome| AppEvent::Done { action, outcome }
}

/// Wrap an index around `len`, in either direction.
fn wrap(index: i64, len: i64) -> i64 {
    index.rem_euclid(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_moves_in_both_directions() {
        assert_eq!(wrap(0, 3), 0);
        assert_eq!(wrap(-1, 3), 2);
        assert_eq!(wrap(3, 3), 0);
        assert_eq!(wrap(4, 3), 1);
    }
}
