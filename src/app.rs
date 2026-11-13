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
use crate::player::LocalPlayer;
use crate::theme::Theme;
use crate::models::{Device, Playback, Playlist, Track};

/// Redraw cadence while something on screen changes with time.
const ANIM_TICK: Duration = Duration::from_millis(250);
/// How long the loop blocks for input when nothing is animating.
const IDLE_TICK: Duration = Duration::from_secs(1);
/// How often the remote playback state is fetched while playing.
const PLAYBACK_POLL_PLAYING: Duration = Duration::from_secs(2);
/// ... and while paused or idle.
const PLAYBACK_POLL_IDLE: Duration = Duration::from_secs(10);
/// Cover panel size in terminal cells the image protocol is built for.
pub(crate) const COVER_CELLS: (u16, u16) = (12, 6);
/// How long a transient status message stays on screen.
pub const STATUS_TTL: Duration = Duration::from_secs(4);

/// How many liked tracks are loaded.
const LIBRARY_LIMIT: u32 = 200;
/// How many search results are fetched.
const SEARCH_LIMIT: u32 = 30;

/// The key of the built-in liked-tracks list in the library view.
pub const LIKED_KEY: &str = "liked";

/// How often to look for Spotify devices while none is active.
const DEVICE_REFRESH: Duration = Duration::from_secs(30);
/// Wait this long before retrying a failed View-tab fetch.
const VIEW_RETRY: Duration = Duration::from_secs(10);

/// Where the View tab's lyrics are in their lifecycle.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum LyricsState {
    /// A fetch is in flight (or has not been submitted yet).
    #[default]
    Loading,
    /// The fetch finished and there are no lyrics for this track.
    Missing,
    /// `App::view_lyrics` holds the lyrics.
    Ready,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Library,
    Search,
    /// What is playing: the artist's top tracks and synced lyrics.
    View,
}

/// Where the list panes rendered last frame: click targets for the mouse.
#[derive(Clone, Copy, Default)]
pub struct MouseZones {
    /// The shuffle and repeat toggles in the now-playing panel.
    pub shuffle_button: ratatui::layout::Rect,
    pub repeat_button: ratatui::layout::Rect,
    /// The playlist list items, row 0 being the built-in liked list.
    pub playlists: ratatui::layout::Rect,
    /// The (library or view) track list items minus its heading rows.
    pub tracks: ratatui::layout::Rect,
    /// The search result list items.
    pub results: ratatui::layout::Rect,
    /// The view tab artist-track list items.
    pub view: ratatui::layout::Rect,
    /// Rows the playlist/tracks list skips before the first entry.
    pub tracks_leading_rows: u16,
    /// Scroll offsets of each list as last drawn (first visible item index),
    /// so a click on screen row N maps to item offset + N.
    pub playlists_offset: usize,
    pub tracks_offset: usize,
    pub results_offset: usize,
    pub view_offset: usize,
}

/// Where audio comes from: another Spotify device, driven over the Web API,
/// or spotcli itself streaming through the built-in player.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    Remote,
    Local,
}

/// A message from a background task.
pub enum AppEvent {
    Playback(Result<Option<Playback>>),
    Playlists(Result<Vec<Playlist>>),
    Tracks { key: String, tracks: Vec<Track> },
    SearchResults { seq: u64, tracks: Vec<Track> },
    Devices(Vec<Device>),
    Done { action: &'static str, outcome: Result<()> },
    /// The built-in player finished starting up.
    PlayerReady(Result<LocalPlayer>),
    /// A state change in the built-in player.
    LocalSignal(crate::player::LocalSignal),
    /// Album art for `uri` finished rendering.
    Cover {
        uri: String,
        protocol: Option<ratatui_image::protocol::Protocol>,
        accent: Option<(u8, u8, u8)>,
    },
    /// Radio continuation tracks for a seed.
    RadioTracks { tracks: Vec<Track> },
    /// Artist top tracks (View tab) fetched for a track.
    ViewCatalog {
        for_track: String,
        artist: String,
        tracks: Vec<Track>,
    },
    /// Synced lyrics (View tab) fetched for a track.
    ViewLyrics {
        for_track: String,
        lyrics: Option<crate::library::Lyrics>,
    },
    /// A View-tab fetch failed; `catalog` tells which one.
    ViewFailed { for_track: String, catalog: bool },
}

type EventTx = mpsc::Sender<AppEvent>;

/// Submits background jobs against the API client, or against the streaming
/// session for reads the Web API refuses to development-mode apps.
#[derive(Clone)]
struct Jobs {
    client: Client,
    handle: Handle,
    tx: EventTx,
    local: Option<LocalPlayer>,
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
    pub theme: Theme,

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

    pub(crate) source: Source,
    /// The built-in streaming player once it has connected.
    pub(crate) local: Option<LocalPlayer>,
    /// Playback to begin the moment the built-in player is ready: a
    /// selected track with the list it came from.
    pub(crate) pending_local_start: Option<(Vec<Track>, usize)>,
    /// A playlist was opened locally; play its first track once the list
    /// with this key has loaded.
    pub(crate) local_playlist_play: Option<String>,
    /// The built-in player is being created (guards against duplicates).
    local_starting: bool,
    /// Local shuffle state (the player itself cannot unshuffle).
    local_shuffle: bool,
    /// Repeat mode of the built-in player.
    local_repeat: crate::player::Repeat,
    /// Monotonic id of the latest search; older replies are dropped.
    search_seq: u64,
    /// The query the shown results (or the in-flight search) belong to.
    pub(crate) searched_for: String,
    /// When devices were last requested.
    last_devices_at: Instant,

    /// Terminal capability picker (kitty/sixel/halfblocks) from startup.
    pub(crate) picker: ratatui_image::picker::Picker,
    /// The now-playing cover rendered for the terminal: the track uri.
    pub(crate) cover: Option<(String, ratatui_image::protocol::Protocol)>,
    /// The track uri whose cover fetch is in flight.
    pub(crate) cover_request: Option<String>,
    /// Accent color sampled from the current cover art, when there is one.
    pub(crate) cover_accent: Option<ratatui::style::Color>,

    pub(crate) zones: MouseZones,
    /// The View tab: track uri the catalog was fetched for.
    pub(crate) view_for: Option<String>,
    pub(crate) view_heading: String,
    pub(crate) view_tracks: Vec<Track>,
    pub(crate) view_cursor: usize,
    pub(crate) view_lyrics: Option<crate::library::Lyrics>,
    /// Whether lyrics are loading, absent, or in `view_lyrics`.
    pub(crate) lyrics_state: LyricsState,
    /// A failed View fetch: track uri and when, to back off before retrying.
    view_failed: Option<(String, Instant)>,
    /// The seed of the last radio fetch; repeating the same seed means the
    /// radio service had nothing new, so do not loop.
    pub(crate) radio_seed: Option<String>,

    pub(crate) status: Option<(String, Instant)>,
    pub(crate) pending: usize,
    /// A /me/player poll is awaiting its reply.
    playback_in_flight: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_theme(Theme::dark(), ratatui_image::picker::Picker::halfblocks())
    }

    /// Build the app with an explicit palette and image protocol picker.
    pub fn with_theme(theme: Theme, picker: ratatui_image::picker::Picker) -> Self {
        Self {
            tab: Tab::Library,
            should_quit: false,
            theme,
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
            source: Source::Remote,
            local: None,
            pending_local_start: None,
            local_playlist_play: None,
            local_starting: false,
            local_shuffle: false,
            local_repeat: crate::player::Repeat::Off,
            search_seq: 0,
            searched_for: String::new(),
            last_devices_at: Instant::now(),
            picker,
            cover: None,
            cover_request: None,
            cover_accent: None,
            zones: MouseZones::default(),
            view_for: None,
            view_heading: String::new(),
            view_tracks: Vec::new(),
            view_cursor: 0,
            view_lyrics: None,
            lyrics_state: LyricsState::Loading,
            view_failed: None,
            radio_seed: None,
            status: None,
            pending: 0,
            playback_in_flight: false,
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
        let mut jobs = Jobs { client, handle, tx, local: None };
        self.bootstrap(&jobs);

        let mut last_poll = Instant::now();
        // Redraw only when something changed; the first frame always draws.
        let mut dirty = true;
        let mut last_draw = Instant::now();
        while !self.should_quit {
            if dirty {
                terminal.draw(|frame| crate::ui::draw(frame, self))?;
                dirty = false;
                last_draw = Instant::now();
            }

            // Time-varying UI (progress gauge, expiring status, spinners)
            // needs periodic redraws; otherwise block for a long time.
            let animated = self.is_animated();
            let mut timeout = if animated { ANIM_TICK } else { IDLE_TICK };
            if self.source == Source::Remote {
                let due = self.poll_interval().saturating_sub(last_poll.elapsed());
                timeout = timeout.min(due.max(Duration::from_millis(1)));
            }
            // Wake exactly when the status message expires instead of ticking.
            if let Some((_, expires)) = self.status.as_ref() {
                let left = expires.saturating_duration_since(Instant::now());
                timeout = timeout.min(left.max(Duration::from_millis(1)));
            }
            if event::poll(timeout)? {
                // Drain everything pending so bursts cost one redraw.
                loop {
                    match event::read()? {
                        TermEvent::Key(key) => {
                            if key.kind == KeyEventKind::Press {
                                self.on_key(key, &jobs);
                            }
                        }
                        TermEvent::Mouse(mouse) => self.on_mouse(mouse, &jobs),
                        _ => {}
                    }
                    dirty = true;
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }

            while let Ok(event) = rx.try_recv() {
                self.on_event(event, &jobs);
                if jobs.local.is_none() {
                    jobs.local = self.local.clone();
                }
                dirty = true;
            }

            self.ensure_cover(&jobs);
            if self.tab == Tab::View {
                self.refresh_view(&jobs);
            }

            if self.source == Source::Remote
                && !self.playback_in_flight
                && last_poll.elapsed() >= self.poll_interval()
            {
                self.refresh_playback(&jobs);
                last_poll = Instant::now();
            }

            if self.source == Source::Remote
                && !self.has_active_device()
                && self.last_devices_at.elapsed() >= DEVICE_REFRESH
            {
                self.refresh_devices(&jobs);
            }

            if self.status.as_ref().is_some_and(|(_, expires)| Instant::now() >= *expires) {
                self.status = None;
                dirty = true;
            }
            if animated && last_draw.elapsed() >= ANIM_TICK {
                dirty = true;
            }
        }
        if let Some(local) = self.local.as_ref() {
            local.shutdown();
        }
        Ok(())
    }

    fn bootstrap(&mut self, jobs: &Jobs) {
        self.refresh_playlists(jobs);
        self.refresh_devices(jobs);
        // Songs visible without a second click: liked tracks load on start.
        self.load_tracks(jobs, LIKED_KEY);
        self.tracks_focused = true;
        // When the streaming grant exists, this terminal is the default
        // speaker; the Web API device list stays under `t`.
        if let Ok(dir) = crate::config_dir() {
            if dir.join("streaming").join("credentials.json").exists() {
                self.source = Source::Local;
                self.ensure_local_player(jobs);
            }
        }
    }

    /// Any Spotify device that could take playback right now.
    fn has_active_device(&self) -> bool {
        self.devices.iter().any(|device| device.is_active)
    }

    fn say(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now() + STATUS_TTL));
    }

    /// Like `say`, but the message stays on screen until the next action.
    /// Errors are easy to miss in four seconds.
    fn say_error(&mut self, message: impl Into<String>) {
        self.status =
            Some((message.into(), Instant::now() + Duration::from_secs(10 * 60)));
    }

    // --- Background refreshes -------------------------------------------------

    fn refresh_playlists(&mut self, jobs: &Jobs) {
        self.pending += 1;
        let client = jobs.client.clone();
        jobs.submit(async move { client.playlists().await.map_err(anyhow::Error::from) }, AppEvent::Playlists);
    }

    /// Whether the screen changes by itself with time.
    fn is_animated(&self) -> bool {
        self.playback.as_ref().is_some_and(|p| p.is_playing) || self.pending > 0
    }

    fn poll_interval(&self) -> Duration {
        if self.playback.as_ref().is_some_and(|p| p.is_playing) {
            PLAYBACK_POLL_PLAYING
        } else {
            PLAYBACK_POLL_IDLE
        }
    }

    fn refresh_playback(&mut self, jobs: &Jobs) {
        self.playback_in_flight = true;
        self.pending += 1;
        let client = jobs.client.clone();
        jobs.submit(async move { client.playback().await.map_err(anyhow::Error::from) }, AppEvent::Playback);
    }

    fn refresh_devices(&mut self, jobs: &Jobs) {
        self.pending += 1;
        self.last_devices_at = Instant::now();
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
        // Never show the previous list under the new title.
        self.tracks.clear();
        self.tracks_cursor = 0;
        let client = jobs.client.clone();
        let local = jobs.local.clone();
        let key = key.to_string();
        jobs.submit(
            async move {
                let tracks = match (key.as_str(), local.as_ref()) {
                    (LIKED_KEY, _) => client
                        .saved_tracks(LIBRARY_LIMIT)
                        .await
                        .map_err(anyhow::Error::from),
                    (_, Some(local)) => {
                        crate::library::playlist_tracks(&local.session(), &key, LIBRARY_LIMIT as usize).await
                    }
                    // Without the streaming session the Web API serves
                    // playlist contents only when the app has extended
                    // quota; otherwise Spotify answers 403 Forbidden.
                    (_, None) => client
                        .playlist_tracks(&key, LIBRARY_LIMIT)
                        .await
                        .map_err(anyhow::Error::from),
                };
                tracks.map(|tracks| (key, tracks))
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
        self.searched_for = query.clone();
        self.pending += 1;
        self.search_seq += 1;
        let seq = self.search_seq;
        let client = jobs.client.clone();
        let local = jobs.local.clone();
        jobs.submit(
            async move {
                // The session search has no per-app quota; the Web API's
                // shared search endpoint answers 503 under load.
                match local.as_ref() {
                    Some(local) => {
                        crate::library::search(&local.session(), &query, SEARCH_LIMIT as usize).await
                    }
                    None => client
                        .search_tracks(&query, SEARCH_LIMIT)
                        .await
                        .map_err(anyhow::Error::from),
                }
            },
            move |result| match result {
                Ok(tracks) => AppEvent::SearchResults { seq, tracks },
                Err(e) => AppEvent::Done {
                    action: "search",
                    outcome: Err(e),
                },
            },
        );
    }

    // --- Keys ------------------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent, jobs: &Jobs) {
        // AltGr arrives as CONTROL|ALT on Windows and types a character.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            if key.code == KeyCode::Char('c') {
                self.should_quit = true;
            }
            return;
        }
        // Media keys the terminal forwards (Windows media keys pass through
        // in recent terminals; elsewhere they simply do nothing).
        if let KeyCode::Media(media) = key.code {
            use ratatui::crossterm::event::MediaKeyCode;
            match media {
                MediaKeyCode::Play | MediaKeyCode::PlayPause => self.toggle_playback(jobs),
                MediaKeyCode::TrackNext | MediaKeyCode::FastForward => self.skip(jobs, true),
                MediaKeyCode::TrackPrevious | MediaKeyCode::Rewind => self.skip(jobs, false),
                _ => {}
            }
            return;
        }
        match self.tab {
            Tab::Library => self.on_key_library(key, jobs),
            Tab::Search => self.on_key_search(key, jobs),
            Tab::View => self.on_key_view(key, jobs),
        }
    }

    fn on_key_view(&mut self, key: KeyEvent, jobs: &Jobs) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => self.tab = Tab::Library,
            KeyCode::Char('v') => self.tab = Tab::Library,
            KeyCode::Esc => self.tab = Tab::Library,
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Enter => self.play_view_selection(),
            KeyCode::Char(' ') => self.toggle_playback(jobs),
            KeyCode::Char('n') => self.skip(jobs, true),
            KeyCode::Char('p') => self.skip(jobs, false),
            KeyCode::Char(',') => self.seek(jobs, -5000),
            KeyCode::Char('.') => self.seek(jobs, 5000),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_volume(jobs, 5),
            KeyCode::Char('-') => self.change_volume(jobs, -5),
            _ => {}
        }
    }

    /// Play the View-tab selection through the built-in player.
    fn play_view_selection(&mut self) {
        let Some(track) = self.view_tracks.get(self.view_cursor).cloned() else {
            return;
        };
        if matches!(self.source, Source::Local) {
            if let Some(local) = self.local.clone() {
                self.set_playing_track(&track);
                let mut queue = self.view_tracks.clone();
                // The selection becomes the queue start: everything before
                // the cursor rotates to the end, standard players do this.
                let head: Vec<Track> = queue.drain(..self.view_cursor).collect();
                queue.extend(head);
                self.start_local(&local, queue, 0);
                return;
            }
        }
        self.say("Local playback unavailable; press t to switch outputs");
    }

    /// Switch audio between Spotify devices and spotcli's own speakers.
    fn toggle_source(&mut self, jobs: &Jobs) {
        match self.source {
            Source::Local => {
                if let Some(local) = self.local.as_ref() {
                    local.pause();
                }
                self.source = Source::Remote;
                // Drop the local snapshot; the remote poll refills it.
                self.playback = None;
                self.playback_at = None;
                self.say("Output: Spotify devices");
                self.refresh_playback(jobs);
                self.refresh_devices(jobs);
            }
            Source::Remote => {
                self.source = Source::Local;
                self.apply_local_snapshot();
                if let Some(local) = self.local.clone() {
                    self.say("Output: this terminal");
                    if let Some((tracks, index)) = self.selection_list() {
                        self.start_local(&local, tracks, index);
                    }
                } else {
                    // Who is going to stream what, the moment the player is up.
                    self.pending_local_start = self.selection_list();
                    self.say("Starting the local player...");
                    self.ensure_local_player(jobs);
                }
            }
        }
    }

    /// The currently focused track list with the cursor into it.
    fn selection_list(&self) -> Option<(Vec<Track>, usize)> {
        match self.tab {
            Tab::Library if self.tracks_focused => {
                Some((self.tracks.clone(), self.tracks_cursor))
            }
            Tab::Search => Some((self.search_results.clone(), self.search_cursor)),
            _ => None,
        }
    }

    /// Begin playing `tracks[index]` through the built-in player.
    fn start_local(&mut self, local: &LocalPlayer, tracks: Vec<Track>, index: usize) {
        match local.play_queue(tracks, index) {
            Ok(()) => {
                self.apply_local_snapshot();
                self.say("Streaming locally");
            }
            Err(e) => self.say(format!("Local player: {e}")),
        }
    }

    /// Copy the built-in player's state into the shared playback view.
    fn apply_local_snapshot(&mut self) {
        let Some(local) = self.local.as_ref() else {
            return;
        };
        let Some(snapshot) = local.snapshot() else {
            return;
        };
        let track = snapshot.track.clone();
        let duration_ms = track.duration_ms;
        let playing = snapshot.is_playing;
        let shuffle = self.local_shuffle;
        let playback = self.playback.get_or_insert_with(Playback::default);
        playback.track = Some(track);
        playback.is_playing = playing;
        playback.progress_ms = snapshot.position_ms;
        playback.duration_ms = duration_ms;
        playback.shuffle = shuffle;
        playback.repeat = match self.local_repeat {
            crate::player::Repeat::Off => "off",
            crate::player::Repeat::All => "context",
            crate::player::Repeat::One => "track",
        }
        .to_string();
        playback.volume_percent = Some(snapshot.volume_percent);
        playback.device_name = "This terminal (local)".to_string();
        self.playback_at = snapshot.progress_at;
    }

    /// Fetch the cover of the playing track, unless it is already shown or
    /// in flight. The fetch runs whenever the track changes.
    fn ensure_cover(&mut self, jobs: &Jobs) {
        let Some(track) = self.playback.as_ref().and_then(|p| p.track.as_ref()) else {
            if self.cover.is_some() || self.cover_accent.is_some() {
                self.cover = None;
                self.cover_request = None;
                self.cover_accent = None;
            }
            return;
        };
        let uri = track.uri.clone();
        if self
            .cover
            .as_ref()
            .is_some_and(|(shown, _)| shown == &uri)
            || self.cover_request.as_deref() == Some(uri.as_str())
        {
            return;
        }
        self.cover = None;
        self.cover_accent = None;
        let Some(url) = track.cover_url.clone() else {
            return;
        };
        self.cover_request = Some(uri.clone());
        self.pending += 1;
        let picker = self.picker.clone();
        jobs.submit(
            async move {
                let art = crate::cover::load(url).await?;
                // Protocol encoding (resize, sixel/kitty/halfblocks) is heavy.
                let (protocol, accent) = tokio::task::spawn_blocking(move || {
                    let accent = crate::cover::accent_color(&art.image);
                    let protocol = picker
                        .new_protocol(
                            art.image.clone(),
                            ratatui::layout::Size::new(COVER_CELLS.0, COVER_CELLS.1),
                            ratatui_image::Resize::Fit(None),
                        )
                        .ok();
                    (protocol, accent)
                })
                .await?;
                Ok(AppEvent::Cover { uri, protocol, accent })
            },
            |result| match result {
                Ok(event) => event,
                Err(_) => AppEvent::Done {
                    action: "cover art",
                    outcome: Ok(()),
                },
            },
        );
    }

    /// Ask Spotify's radio for what follows the last played track and keep
    /// going; a no-op when the same seed produced nothing before.
    fn request_radio(&mut self, jobs: &Jobs) {
        let Some(local) = self.local.as_ref() else {
            return;
        };
        let Some(track) = local.current() else {
            return;
        };
        let seed = track.uri.clone();
        if self.radio_seed.as_deref() == Some(seed.as_str()) {
            self.say("Queue finished");
            return;
        }
        self.radio_seed = Some(seed.clone());
        let recent = local.played_uris();
        let session = local.session();
        jobs.submit(
            async move {
                crate::library::radio_tracks(&session, &seed, recent)
                    .await
                    .map(|tracks| AppEvent::RadioTracks { tracks })
            },
            |result| match result {
                Ok(event) => event,
                Err(_) => AppEvent::Done {
                    action: "radio",
                    outcome: Ok(()),
                },
            },
        );
    }

    /// Refresh the View tab for the playing track: artist top tracks and
    /// synced lyrics, fetched once per track.
    fn refresh_view(&mut self, jobs: &Jobs) {
        let Some(track) = self.playback.as_ref().and_then(|p| p.track.as_ref()) else {
            return;
        };
        let uri = track.uri.clone();
        if self.view_for.as_deref() == Some(uri.as_str()) {
            return;
        }
        if let Some((failed, at)) = self.view_failed.as_ref() {
            if failed == &uri && at.elapsed() < VIEW_RETRY {
                return;
            }
        }
        // Only the built-in session can read the catalog; without it, stay
        // retryable (view_for is not set) and say why nothing loads.
        let Some(local) = self.local.as_ref() else {
            self.view_for = None;
            self.view_tracks.clear();
            self.view_lyrics = None;
            self.lyrics_state = LyricsState::Missing;
            self.view_heading = "Needs the built-in player (press t)".to_string();
            return;
        };
        self.view_for = Some(uri.clone());
        self.view_failed = None;
        self.view_tracks.clear();
        self.view_lyrics = None;
        self.lyrics_state = LyricsState::Loading;
        self.view_cursor = 0;
        self.view_heading = "Loading".to_string();
        let session = local.session();
        let seed = uri.clone();
        let failed_uri = uri.clone();
        let failed_uri2 = uri.clone();
        self.pending += 2;
        jobs.submit(
            async move {
                crate::library::view_catalog(&session, &seed)
                    .await
                    .map(|(artist, tracks)| AppEvent::ViewCatalog {
                        for_track: seed,
                        artist,
                        tracks,
                    })
            },
            move |result| match result {
                Ok(event) => event,
                Err(_) => AppEvent::ViewFailed { for_track: failed_uri, catalog: true },
            },
        );
        let seed = uri;
        let session = local.session();
        jobs.submit(
            async move {
                crate::library::lyrics(&session, &seed)
                    .await
                    .map(|lyrics| AppEvent::ViewLyrics {
                        for_track: seed,
                        lyrics,
                    })
            },
            move |result| match result {
                Ok(event) => event,
                Err(_) => AppEvent::ViewFailed { for_track: failed_uri2, catalog: false },
            },
        );
    }

    /// Spin up the built-in player in the background, once.
    fn ensure_local_player(&mut self, jobs: &Jobs) {
        if self.local.is_some() || self.local_starting || self.source != Source::Local {
            return;
        }
        self.local_starting = true;
        self.pending += 1;
        jobs.submit(
            async move {
                let dir = crate::config_dir()?;
                LocalPlayer::create(&dir).await
            },
            AppEvent::PlayerReady,
        );
    }

    fn on_key_library(&mut self, key: KeyEvent, jobs: &Jobs) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => self.tab = Tab::Search,
            KeyCode::Char('v') => self.tab = Tab::View,
            KeyCode::Left | KeyCode::Char('h') => self.tracks_focused = false,
            KeyCode::Right | KeyCode::Char('l') => self.tracks_focused = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Enter => self.play_selection(jobs),
            KeyCode::Char('t') => self.toggle_source(jobs),
            KeyCode::Char(' ') => self.toggle_playback(jobs),
            KeyCode::Char('n') => self.skip(jobs, true),
            KeyCode::Char('p') => self.skip(jobs, false),
            KeyCode::Char('s') => self.toggle_shuffle(jobs),
            KeyCode::Char('r') => self.cycle_repeat(jobs),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_volume(jobs, 5),
            KeyCode::Char('-') => self.change_volume(jobs, -5),
            KeyCode::Char(',') => self.seek(jobs, -5000),
            KeyCode::Char('.') => self.seek(jobs, 5000),
            KeyCode::Char('a') => self.queue_selection(jobs),
            KeyCode::Char('d') => self.next_device(jobs),
            _ => {}
        }
    }

    fn on_key_search(&mut self, key: KeyEvent, jobs: &Jobs) {
        match key.code {
            KeyCode::Char('q') if self.search_input.is_empty() => self.should_quit = true,
            KeyCode::Tab => self.tab = Tab::View,
            KeyCode::Esc => {
                self.search_input.clear();
                self.search_cursor = 0;
            }
            // A new query searches; on the results of the current one, Enter
            // plays the selected track.
            KeyCode::Enter => {
                if !self.search_results.is_empty()
                    && self.search_input.trim() == self.searched_for
                {
                    self.play_selection(jobs);
                } else {
                    self.submit_search(jobs);
                }
            }
            KeyCode::Backspace => {
                self.search_input.pop();
                self.search_cursor = 0;
            }
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            // Every printable key goes into the input here; playback keys
            // live in the Library tab.
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
            Tab::View => (&mut self.view_cursor, self.view_tracks.len()),
            Tab::Library => {
                if self.tracks_focused {
                    (&mut self.tracks_cursor, self.tracks.len())
                } else {
                    // Row 0 is Liked Songs, rows 1.. are the playlists.
                    (&mut self.playlists_cursor, self.playlists.len() + 1)
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
                // Row 0 is Liked Songs, rows 1.. are playlists[row - 1].
                let (id, name, uri) = if self.playlists_cursor == 0 {
                    (LIKED_KEY.to_string(), "Liked Songs".to_string(), None)
                } else {
                    let Some(playlist) = self.playlists.get(self.playlists_cursor - 1) else {
                        return;
                    };
                    (playlist.id.clone(), playlist.name.clone(), Some(playlist.uri.clone()))
                };
                let jobs = jobs.clone();
                self.tracks_focused = true;
                self.tracks_cursor = 0;
                self.local_playlist_play = None;
                self.load_tracks(&jobs, &id);
                match self.source {
                    Source::Local => {
                        // The track list arrives as an event; opening plays
                        // its first track through the built-in player then.
                        self.local_playlist_play = Some(id);
                        if self.local.is_none() {
                            self.say(format!("Opening {name}; starting the local player..."));
                            self.ensure_local_player(&jobs);
                        } else {
                            self.say(format!("Opening {name}..."));
                        }
                    }
                    Source::Remote if !self.has_active_device() => {
                        self.refresh_devices(&jobs);
                        self.say("No Spotify device found; press t to stream locally, or open the Spotify app");
                    }
                    Source::Remote => match uri {
                        Some(uri) => {
                            self.say(format!("Opening {name}..."));
                            let client = jobs.client.clone();
                            jobs.submit(
                                async move {
                                    client.play_context(None, &uri, None).await.map_err(anyhow::Error::from)
                                },
                                done("play playlist"),
                            );
                        }
                        // Liked Songs has no context uri: just show the list;
                        // Enter on a track plays it.
                        None => self.say("Opening Liked Songs..."),
                    },
                }
            }
            _ => {
                let Some(track) = self.selected_track() else {
                    return;
                };
                let uri = track.uri.clone();
                if self.source == Source::Remote && !self.has_active_device() {
                    self.refresh_devices(jobs);
                    self.say("No Spotify device found; press t to stream locally, or open the Spotify app");
                    return;
                }
                self.set_playing_track(&track);
                match self.source {
                    Source::Local => {
                        let (tracks, index) = self
                            .selection_list()
                            .unwrap_or_else(|| (vec![track.clone()], 0));
                        match self.local.clone() {
                            Some(local) => self.start_local(&local, tracks, index),
                            None => {
                                self.pending_local_start = Some((tracks, index));
                                self.say("Starting the local player...");
                                self.ensure_local_player(jobs);
                            }
                        }
                    }
                    _ => {
                        let client = jobs.client.clone();
                        jobs.submit(
                            async move { client.play(None, &[uri]).await.map_err(anyhow::Error::from) },
                            done("play track"),
                        );
                    }
                }
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
        if self.source == Source::Local {
            match self.local.as_ref() {
                Some(local) => {
                    local.toggle();
                    self.apply_local_snapshot();
                }
                None => self.say("The local player is still starting"),
            }
            return;
        }
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
        if self.source == Source::Local {
            match self.local.as_ref() {
                Some(local) => {
                    local.skip(forward);
                    self.apply_local_snapshot();
                }
                None => self.say("The local player is still starting"),
            }
            return;
        }
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

    /// Nudge the local playhead by `delta_ms`; seeks are a local-player
    /// affordance, Spotify devices skip instead.
    fn seek(&mut self, jobs: &Jobs, delta_ms: i64) {
        if self.source != Source::Local {
            return self.skip(jobs, delta_ms > 0);
        }
        match self.local.as_ref() {
            Some(local) => {
                local.seek_by(delta_ms);
                self.apply_local_snapshot();
            }
            None => self.say("The local player is still starting"),
        }
    }

    fn toggle_shuffle(&mut self, jobs: &Jobs) {
        if self.source == Source::Local {
            if let Some(local) = self.local.as_ref() {
                // Turning it on shuffles what is still queued; turning it
                // off only clears the flag (the queue is not restored).
                self.local_shuffle = !self.local_shuffle;
                if self.local_shuffle {
                    local.shuffle_queue();
                }
                self.apply_local_snapshot();
                let on = self.local_shuffle;
                self.say(if on { "Shuffle on" } else { "Shuffle off" });
            }
            return;
        }
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
        if self.source == Source::Local {
            use crate::player::Repeat;
            self.local_repeat = match self.local_repeat {
                Repeat::Off => Repeat::All,
                Repeat::All => Repeat::One,
                Repeat::One => Repeat::Off,
            };
            if let Some(local) = self.local.as_ref() {
                local.set_repeat(self.local_repeat);
            }
            self.apply_local_snapshot();
            self.say(match self.local_repeat {
                Repeat::Off => "Repeat off",
                Repeat::All => "Repeat all",
                Repeat::One => "Repeat one",
            });
            return;
        }
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
        if self.source == Source::Local {
            match self.local.as_ref() {
                Some(local) => {
                    let current = match self.playback.as_ref().and_then(|p| p.volume_percent) {
                        Some(percent) => percent as i32,
                        None => 50,
                    };
                    let next = (current + delta).clamp(0, 100) as u8;
                    local.set_volume_percent(next);
                    self.apply_local_snapshot();
                }
                None => self.say("The local player is still starting"),
            }
            return;
        }
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
        if self.source == Source::Local {
            self.say("Queueing works on Spotify devices; local playback follows the list");
            return;
        }
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
        if self.source == Source::Local {
            self.say("This terminal is the device; press t to switch to Spotify devices");
            return;
        }
        if self.devices.len() < 2 {
            self.refresh_devices(jobs);
            self.say("No other Spotify Connect device found; rescanning");
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

    // --- Mouse -------------------------------------------------------------------

    fn on_mouse(&mut self, mouse: ratatui::crossterm::event::MouseEvent, jobs: &Jobs) {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_cursor(-1),
            MouseEventKind::ScrollDown => self.move_cursor(1),
            MouseEventKind::Down(MouseButton::Left) => {
                use ratatui::layout::Position;
                let at = Position { x: mouse.column, y: mouse.row };
                if self.zones.shuffle_button.contains(at) {
                    self.toggle_shuffle(jobs);
                    return;
                }
                if self.zones.repeat_button.contains(at) {
                    self.cycle_repeat(jobs);
                    return;
                }
                let row_in = |zone: ratatui::layout::Rect, offset: usize| -> usize {
                    (mouse.row - zone.y) as usize + offset
                };
                match self.tab {
                    Tab::Library => {
                        if self.zones.playlists.contains(at) {
                            // Row 0 is Liked Songs, rows 1.. the playlists.
                            let index = row_in(self.zones.playlists, self.zones.playlists_offset);
                            if index > self.playlists.len() {
                                return;
                            }
                            self.tracks_focused = false;
                            self.playlists_cursor = index;
                            self.play_selection(jobs);
                        } else if self.zones.tracks.contains(at) {
                            let leading = self.zones.tracks_leading_rows as usize;
                            let rel = (mouse.row - self.zones.tracks.y) as usize;
                            if rel < leading {
                                return;
                            }
                            let index = rel - leading + self.zones.tracks_offset;
                            if index >= self.tracks.len() {
                                return;
                            }
                            self.tracks_focused = true;
                            self.tracks_cursor = index;
                            self.play_selection(jobs);
                        }
                    }
                    Tab::Search => {
                        if self.zones.results.contains(at) {
                            let index = row_in(self.zones.results, self.zones.results_offset);
                            if index >= self.search_results.len() {
                                return;
                            }
                            self.search_cursor = index;
                            self.play_selection(jobs);
                        }
                    }
                    Tab::View => {
                        if self.zones.view.contains(at) {
                            let index = row_in(self.zones.view, self.zones.view_offset);
                            if index >= self.view_tracks.len() {
                                return;
                            }
                            self.view_cursor = index;
                            self.play_view_selection();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // --- Events -----------------------------------------------------------------

    fn on_event(&mut self, event: AppEvent, jobs: &Jobs) {
        match event {
            AppEvent::Playback(result) => {
                self.playback_in_flight = false;
                self.pending = self.pending.saturating_sub(1);
                match result {
                    // A late reply must not overwrite the local snapshot.
                    Ok(_) if self.source == Source::Local => {}
                    Ok(Some(playback)) => {
                        self.playback_at = Some(Instant::now());
                        self.playback = Some(playback);
                    }
                    // Nothing is playing anywhere: stop extrapolating.
                    Ok(None) => {
                        if let Some(playback) = self.playback.as_mut() {
                            playback.is_playing = false;
                        }
                    }
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
                    if self.local_playlist_play.as_deref() == Some(key.as_str())
                        && self.source == Source::Local
                        && !self.tracks.is_empty()
                    {
                        self.local_playlist_play = None;
                        let tracks = self.tracks.clone();
                        match self.local.clone() {
                            Some(local) => self.start_local(&local, tracks, 0),
                            None => self.pending_local_start = Some((tracks, 0)),
                        }
                    }
                }
            }
            AppEvent::SearchResults { seq, tracks } => {
                self.pending = self.pending.saturating_sub(1);
                if seq != self.search_seq {
                    return;
                }
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
                    if action == "load tracks" {
                        self.local_playlist_play = None;
                    }
                    let text = format!("{e}");
                    let lower = text.to_ascii_lowercase();
                    let no_device = lower.contains("no active device")
                        || lower.contains("no_active_device")
                        || lower.contains("404");
                    if no_device && self.source == Source::Remote {
                        self.refresh_devices(jobs);
                    }
                    // Device rescans run in the background; stay quiet.
                    if action != "devices" {
                        self.say_error(format!("{action}: {text}"));
                    }
                }
            }
            AppEvent::PlayerReady(result) => {
                self.pending = self.pending.saturating_sub(1);
                self.local_starting = false;
                match result {
                    // A duplicate session: keep the first, drop this one.
                    Ok(extra) if self.local.is_some() => extra.shutdown(),
                    Ok(local) => {
                        // Forward player state changes into the event queue.
                        let (signal_tx, mut signal_rx) = tokio::sync::mpsc::unbounded_channel();
                        local.watch(signal_tx);
                        local.set_repeat(self.local_repeat);
                        let event_tx = jobs.tx.clone();
                        jobs.handle.spawn(async move {
                            while let Some(signal) = signal_rx.recv().await {
                                let _ = event_tx.send(AppEvent::LocalSignal(signal));
                            }
                        });
                        self.local = Some(local);
                        self.apply_local_snapshot();
                        if let Some((tracks, index)) = self.pending_local_start.take() {
                            let local = self.local.clone().expect("stored just above");
                            self.start_local(&local, tracks, index);
                        } else {
                            self.say("Output: this terminal");
                        }
                    }
                    Err(e) => {
                        self.source = Source::Remote;
                        self.say_error(format!(
                            "Local player failed: {e} — press t to retry, it keeps this message on screen"
                        ));
                        self.refresh_playback(jobs);
                    }
                }
            }
            AppEvent::Cover { uri, protocol, accent } => {
                self.pending = self.pending.saturating_sub(1);
                self.cover_request = None;
                if self
                    .playback
                    .as_ref()
                    .and_then(|p| p.track.as_ref())
                    .is_some_and(|track| track.uri == uri)
                {
                    if let Some(protocol) = protocol {
                        self.cover = Some((uri, protocol));
                    }
                    self.cover_accent = accent.map(|(r, g, b)| ratatui::style::Color::Rgb(r, g, b));
                }
            }
            AppEvent::LocalSignal(signal) => match signal {
                crate::player::LocalSignal::Status => self.apply_local_snapshot(),
                crate::player::LocalSignal::Ended => {
                    self.apply_local_snapshot();
                    self.request_radio(jobs);
                }
                crate::player::LocalSignal::Error(message) => {
                    self.apply_local_snapshot();
                    self.say_error(message);
                }
            },
            AppEvent::RadioTracks { tracks, .. } => {
                self.radio_seed = None;
                if let Some(local) = self.local.as_ref() {
                    let added = local.extend_queue(tracks);
                    self.apply_local_snapshot();
                    match added {
                        Some(_) => self.say("Radio: similar tracks queued"),
                        None => self.say("Queue finished"),
                    }
                }
            }
            AppEvent::ViewCatalog { for_track, artist, tracks } => {
                self.pending = self.pending.saturating_sub(1);
                if self.view_for.as_deref() == Some(for_track.as_str()) {
                    self.view_heading = format!("{artist} — top tracks");
                    self.view_tracks = tracks;
                    self.view_cursor = 0;
                }
            }
            AppEvent::ViewLyrics { for_track, lyrics } => {
                self.pending = self.pending.saturating_sub(1);
                if self.view_for.as_deref() == Some(for_track.as_str()) {
                    self.lyrics_state =
                        if lyrics.is_some() { LyricsState::Ready } else { LyricsState::Missing };
                    self.view_lyrics = lyrics;
                }
            }
            AppEvent::ViewFailed { for_track, catalog } => {
                self.pending = self.pending.saturating_sub(1);
                if self.view_for.as_deref() == Some(for_track.as_str()) {
                    if catalog {
                        // Allow a retry after the backoff.
                        self.view_for = None;
                        self.view_failed = Some((for_track, Instant::now()));
                        self.view_heading = "Couldn't load; retrying...".to_string();
                    } else {
                        self.lyrics_state = LyricsState::Missing;
                    }
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
