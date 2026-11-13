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
use crate::models::{Album, Artist, Device, Playback, Playlist, Track, User};

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
/// How many search results are fetched (per page).
const SEARCH_LIMIT: u32 = 30;
/// How many artists, albums and playlists each are requested.
const SEARCH_EXTRA_LIMIT: u32 = 20;
/// Quiet time after the last keystroke before searching as you type.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(350);
/// Shortest trimmed query that searches by itself.
pub const SEARCH_MIN_CHARS: usize = 2;

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

/// Where typing goes on the Search tab.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum SearchFocus {
    /// Keys edit the query.
    #[default]
    Input,
    /// Keys are shortcuts on the selected result.
    Results,
}

/// The result groups shown as tabs inside the results panel.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum SearchCategory {
    #[default]
    Tracks,
    Artists,
    Albums,
    Playlists,
}

impl SearchCategory {
    pub const ALL: [SearchCategory; 4] = [
        SearchCategory::Tracks,
        SearchCategory::Artists,
        SearchCategory::Albums,
        SearchCategory::Playlists,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            SearchCategory::Tracks => "Tracks",
            SearchCategory::Artists => "Artists",
            SearchCategory::Albums => "Albums",
            SearchCategory::Playlists => "Playlists",
        }
    }

    fn shifted(self, delta: i32) -> Self {
        Self::ALL[wrap(self.index() as i64 + delta as i64, 4) as usize]
    }
}

/// Where the lists of artists, albums and playlists stand.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum ExtraState {
    /// No search has asked yet.
    #[default]
    Idle,
    Loading,
    Ready,
    /// The Web API refused or was unreachable (503, 429, ...).
    Failed,
}

/// Where the next page of track results comes from.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub enum MoreTracks {
    /// Everything was loaded.
    #[default]
    None,
    /// Session search: the track uris not resolved yet, in match order.
    Session(Vec<String>),
    /// Web API search: the offset of the next page.
    Web(u32),
}

/// One page of track results.
pub struct SearchPage {
    pub tracks: Vec<Track>,
    pub more: MoreTracks,
}

/// Artists, albums and playlists matching a query.
pub struct SearchExtras {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub playlists: Vec<Playlist>,
}

/// The "add to playlist" overlay.
pub struct PlaylistPicker {
    pub track: Track,
    pub cursor: usize,
    pub filter: String,
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
    /// The search input field.
    pub search_input: ratatui::layout::Rect,
    /// The category tabs of the search results, in `SearchCategory::ALL` order.
    pub search_tabs: [ratatui::layout::Rect; 4],
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
    SearchResults { seq: u64, outcome: Result<SearchPage> },
    /// The next page of track results.
    SearchMore { seq: u64, outcome: Result<SearchPage> },
    /// Artists, albums and playlists for the search.
    SearchExtra { seq: u64, outcome: Result<SearchExtras> },
    /// The signed-in user's profile (for playlist ownership).
    Me(Result<User>),
    /// A like (save to Liked Songs) finished.
    LikeResult { uri: String, outcome: Result<()> },
    /// A track was (or was not) added to a playlist.
    PlaylistAdded { playlist_id: String, name: String, outcome: Result<()> },
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
    /// Heading of the tracks pane when it shows something other than a
    /// library playlist (an album or artist opened from search).
    pub(crate) tracks_title: Option<String>,

    pub(crate) search_input: String,
    pub(crate) search_results: Vec<Track>,
    pub(crate) search_cursor: usize,
    pub(crate) search_focus: SearchFocus,
    pub(crate) search_category: SearchCategory,
    pub(crate) search_artists: Vec<Artist>,
    pub(crate) search_albums: Vec<Album>,
    pub(crate) search_playlists: Vec<Playlist>,
    /// Cursors of the artists, albums and playlists lists.
    pub(crate) search_extra_cursors: [usize; 3],
    pub(crate) search_extra: ExtraState,
    /// The track search is in flight.
    pub(crate) search_loading: bool,
    /// More track results exist beyond `search_results`.
    pub(crate) search_more: MoreTracks,
    pub(crate) search_more_loading: bool,
    /// When the search-as-you-type debounce fires.
    pub(crate) search_debounce: Option<Instant>,
    /// The "add to playlist" overlay, while open.
    pub(crate) add_to: Option<PlaylistPicker>,
    /// The signed-in user's id, to tell owned playlists from followed ones.
    pub(crate) me_id: Option<String>,
    me_in_flight: bool,

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
            tracks_title: None,
            search_input: String::new(),
            search_results: Vec::new(),
            search_cursor: 0,
            search_focus: SearchFocus::Input,
            search_category: SearchCategory::Tracks,
            search_artists: Vec::new(),
            search_albums: Vec::new(),
            search_playlists: Vec::new(),
            search_extra_cursors: [0; 3],
            search_extra: ExtraState::Idle,
            search_loading: false,
            search_more: MoreTracks::None,
            search_more_loading: false,
            search_debounce: None,
            add_to: None,
            me_id: None,
            me_in_flight: false,
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
            // Wake when the search-as-you-type debounce is due, no keypress needed.
            if let Some(left) = self.search_debounce_left(Instant::now()) {
                timeout = timeout.min(left.max(Duration::from_millis(1)));
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

            if self.take_due_search(Instant::now()) {
                self.submit_search(&jobs);
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
        self.fetch_me(jobs);
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
        self.tracks_key = Some(key.to_string());
        self.tracks_title = None;
        // Never show the previous list under the new title.
        self.tracks.clear();
        self.tracks_cursor = 0;
        self.fetch_tracks(jobs, key);
    }

    /// Fetch the list for `key` into the tracks pane without clearing it first.
    fn fetch_tracks(&mut self, jobs: &Jobs, key: &str) {
        self.pending += 1;
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
                    (k, _) if k.starts_with("album:") || k.starts_with("artist:") => {
                        catalog_tracks(&client, local.as_ref(), k).await
                    }
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

    /// Show `key` (a playlist id, `album:<id>` or `artist:<id>`) in the
    /// Library tab's tracks pane under `title`.
    fn open_catalog(&mut self, jobs: &Jobs, key: String, title: String) {
        if key.ends_with(':') || key.is_empty() {
            self.say("That result has no id to open");
            return;
        }
        self.tab = Tab::Library;
        self.tracks_focused = true;
        self.local_playlist_play = None;
        self.load_tracks(jobs, &key);
        self.tracks_title = Some(title.clone());
        self.say(format!("Opening {title}..."));
    }

    // --- Search -------------------------------------------------------------------

    /// Time left before the debounced search is due, if one is armed.
    pub(crate) fn search_debounce_left(&self, now: Instant) -> Option<Duration> {
        self.search_debounce.map(|at| at.saturating_duration_since(now))
    }

    /// Re-arm (or disarm) the search-as-you-type debounce after an edit.
    fn arm_search_debounce(&mut self, now: Instant) {
        self.search_debounce =
            wants_search(&self.search_input, &self.searched_for).then(|| now + SEARCH_DEBOUNCE);
    }

    /// Whether the debounce elapsed and a search should start now; disarms it.
    pub(crate) fn take_due_search(&mut self, now: Instant) -> bool {
        match self.search_debounce {
            Some(at) if now >= at => {
                self.search_debounce = None;
                wants_search(&self.search_input, &self.searched_for)
            }
            _ => false,
        }
    }

    fn submit_search(&mut self, jobs: &Jobs) {
        let query = self.search_input.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.search_debounce = None;
        self.searched_for = query.clone();
        self.pending += 2;
        self.search_seq += 1;
        self.search_loading = true;
        self.search_more_loading = false;
        self.search_extra = ExtraState::Loading;
        let seq = self.search_seq;

        let client = jobs.client.clone();
        let local = jobs.local.clone();
        let tracks_query = query.clone();
        jobs.submit(
            async move { search_tracks_job(&client, local.as_ref(), &tracks_query).await },
            move |outcome| AppEvent::SearchResults { seq, outcome },
        );

        // Artists, albums and playlists exist only in the Web API's search;
        // the session's search context carries tracks alone.
        let client = jobs.client.clone();
        jobs.submit(
            async move {
                let (artists, albums, playlists) = client
                    .search_extras(&query, SEARCH_EXTRA_LIMIT)
                    .await
                    .map_err(anyhow::Error::from)?;
                Ok(SearchExtras { artists, albums, playlists })
            },
            move |outcome| AppEvent::SearchExtra { seq, outcome },
        );
    }

    /// Fetch the next page of track results (no-op when none or in flight).
    fn load_more_tracks(&mut self, jobs: &Jobs) {
        if self.search_more_loading {
            return;
        }
        let more = self.search_more.clone();
        if more == MoreTracks::None {
            return;
        }
        self.search_more_loading = true;
        self.pending += 1;
        let seq = self.search_seq;
        let client = jobs.client.clone();
        let local = jobs.local.clone();
        let query = self.searched_for.clone();
        jobs.submit(
            async move {
                match more {
                    MoreTracks::Session(mut rest) => {
                        let remainder = rest.split_off(SEARCH_LIMIT.min(rest.len() as u32) as usize);
                        let local = local.ok_or_else(|| anyhow::anyhow!("the local player is gone"))?;
                        let tracks = crate::library::resolve_page(&local.session(), rest).await;
                        let more = if remainder.is_empty() {
                            MoreTracks::None
                        } else {
                            MoreTracks::Session(remainder)
                        };
                        Ok(SearchPage { tracks, more })
                    }
                    MoreTracks::Web(offset) => {
                        let (tracks, next) = client
                            .search_tracks_page(&query, SEARCH_LIMIT, offset)
                            .await
                            .map_err(anyhow::Error::from)?;
                        Ok(SearchPage { tracks, more: next.map_or(MoreTracks::None, MoreTracks::Web) })
                    }
                    MoreTracks::None => Ok(SearchPage { tracks: Vec::new(), more: MoreTracks::None }),
                }
            },
            move |outcome| AppEvent::SearchMore { seq, outcome },
        );
    }

    fn fetch_me(&mut self, jobs: &Jobs) {
        if self.me_in_flight || self.me_id.is_some() {
            return;
        }
        self.me_in_flight = true;
        self.pending += 1;
        let client = jobs.client.clone();
        jobs.submit(async move { client.me().await.map_err(anyhow::Error::from) }, AppEvent::Me);
    }

    pub(crate) fn search_len(&self, category: SearchCategory) -> usize {
        match category {
            SearchCategory::Tracks => self.search_results.len(),
            SearchCategory::Artists => self.search_artists.len(),
            SearchCategory::Albums => self.search_albums.len(),
            SearchCategory::Playlists => self.search_playlists.len(),
        }
    }

    pub(crate) fn search_cursor_of(&self, category: SearchCategory) -> usize {
        match category {
            SearchCategory::Tracks => self.search_cursor,
            other => self.search_extra_cursors[other.index() - 1],
        }
    }

    fn set_search_cursor(&mut self, category: SearchCategory, value: usize) {
        match category {
            SearchCategory::Tracks => self.search_cursor = value,
            other => self.search_extra_cursors[other.index() - 1] = value,
        }
    }

    fn focus_search_results_or_keep(&mut self) {
        if self.has_any_results() {
            self.search_focus = SearchFocus::Results;
        }
    }

    fn has_any_results(&self) -> bool {
        SearchCategory::ALL.iter().any(|c| self.search_len(*c) > 0)
    }

    fn focus_search_results(&mut self) {
        if !self.has_any_results() {
            return;
        }
        self.search_focus = SearchFocus::Results;
        // Land on a category that has something when the current one is empty.
        if self.search_len(self.search_category) == 0 {
            if let Some(c) = SearchCategory::ALL.iter().find(|c| self.search_len(**c) > 0) {
                self.search_category = *c;
            }
        }
    }

    /// Move the selection in the active category. Single steps wrap; page
    /// jumps clamp. Landing on the last track row loads the next page.
    fn search_move(&mut self, delta: i32, jobs: &Jobs) {
        let category = self.search_category;
        let len = self.search_len(category);
        if len == 0 {
            return;
        }
        let cursor = self.search_cursor_of(category).min(len - 1);
        let more = category == SearchCategory::Tracks && self.search_more != MoreTracks::None;
        let next = if delta.abs() > 1 {
            (cursor as i64 + delta as i64).clamp(0, len as i64 - 1) as usize
        } else if delta > 0 && cursor + 1 >= len && more {
            cursor
        } else {
            wrap(cursor as i64 + delta as i64, len as i64) as usize
        };
        self.set_search_cursor(category, next);
        if more && next + 1 >= len {
            self.load_more_tracks(jobs);
        }
    }

    fn on_key_search(&mut self, key: KeyEvent, jobs: &Jobs) {
        match self.search_focus {
            SearchFocus::Input => self.on_key_search_input(key, jobs),
            SearchFocus::Results => self.on_key_search_results(key, jobs),
        }
    }

    fn on_key_search_input(&mut self, key: KeyEvent, jobs: &Jobs) {
        match key.code {
            KeyCode::Char('q') if self.search_input.is_empty() => self.should_quit = true,
            KeyCode::Tab => self.tab = Tab::View,
            KeyCode::Esc => {
                self.search_input.clear();
                self.search_debounce = None;
            }
            // A new query searches at once; on the results of the current
            // one, Enter hands the keyboard to them.
            KeyCode::Enter => {
                if self.has_any_results() && self.search_input.trim() == self.searched_for {
                    self.focus_search_results();
                } else {
                    self.submit_search(jobs);
                }
            }
            KeyCode::Down => self.focus_search_results(),
            KeyCode::Backspace => {
                self.search_input.pop();
                self.arm_search_debounce(Instant::now());
            }
            // Every printable key goes into the input here.
            KeyCode::Char(c) => {
                self.search_input.push(c);
                self.arm_search_debounce(Instant::now());
            }
            _ => {}
        }
    }

    fn on_key_search_results(&mut self, key: KeyEvent, jobs: &Jobs) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => self.tab = Tab::View,
            KeyCode::Esc | KeyCode::Char('/') | KeyCode::Char('i') => {
                self.search_focus = SearchFocus::Input;
            }
            KeyCode::Up | KeyCode::Char('k') => self.search_move(-1, jobs),
            KeyCode::Down | KeyCode::Char('j') => self.search_move(1, jobs),
            KeyCode::PageUp => self.search_move(-10, jobs),
            KeyCode::PageDown => self.search_move(10, jobs),
            KeyCode::Left => self.search_category = self.search_category.shifted(-1),
            KeyCode::Right => self.search_category = self.search_category.shifted(1),
            KeyCode::Char(c @ '1'..='4') => {
                self.search_category = SearchCategory::ALL[(c as u8 - b'1') as usize];
            }
            KeyCode::Enter => self.activate_search_selection(jobs),
            KeyCode::Char('a') => self.queue_selection(jobs),
            KeyCode::Char('l') => self.like_selection(jobs),
            KeyCode::Char('p') => self.open_add_to_playlist(jobs),
            KeyCode::Char(' ') => self.toggle_playback(jobs),
            KeyCode::Char('n') => self.skip(jobs, true),
            KeyCode::Char(',') => self.seek(jobs, -5000),
            KeyCode::Char('.') => self.seek(jobs, 5000),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_volume(jobs, 5),
            KeyCode::Char('-') => self.change_volume(jobs, -5),
            KeyCode::Char('s') => self.toggle_shuffle(jobs),
            KeyCode::Char('r') => self.cycle_repeat(jobs),
            KeyCode::Char('t') => self.toggle_source(jobs),
            _ => {}
        }
    }

    /// Enter on the selected search result: play a track, open anything else
    /// in the Library tab's tracks pane.
    fn activate_search_selection(&mut self, jobs: &Jobs) {
        let category = self.search_category;
        let index = self.search_cursor_of(category);
        match category {
            SearchCategory::Tracks => self.play_selection(jobs),
            SearchCategory::Artists => {
                if let Some(artist) = self.search_artists.get(index).cloned() {
                    self.open_catalog(jobs, format!("artist:{}", artist.id), artist.name);
                }
            }
            SearchCategory::Albums => {
                if let Some(album) = self.search_albums.get(index).cloned() {
                    self.open_catalog(jobs, format!("album:{}", album.id), album.name);
                }
            }
            SearchCategory::Playlists => {
                if let Some(playlist) = self.search_playlists.get(index).cloned() {
                    self.open_catalog(jobs, playlist.id, playlist.name);
                }
            }
        }
    }

    /// Save the selected track to Liked Songs, optimistically.
    fn like_selection(&mut self, jobs: &Jobs) {
        let Some(track) = self.selected_track() else {
            self.say("Select a track to like it");
            return;
        };
        let id = track_id(&track);
        if id.is_empty() {
            return;
        }
        self.say(format!("Liked {}", track.name));
        if self.tracks_key.as_deref() == Some(LIKED_KEY)
            && !self.tracks.is_empty()
            && !self.tracks.iter().any(|t| t.uri == track.uri)
        {
            self.tracks.insert(0, track.clone());
            self.tracks_cursor += 1;
        }
        self.pending += 1;
        let client = jobs.client.clone();
        let uri = track.uri.clone();
        jobs.submit(
            async move { client.save_tracks(&[id]).await.map_err(|e| e.into_write_error()) },
            move |outcome| AppEvent::LikeResult { uri, outcome },
        );
    }

    /// Open the playlist picker for the selected track.
    fn open_add_to_playlist(&mut self, jobs: &Jobs) {
        let Some(track) = self.selected_track() else {
            self.say("Select a track to add it to a playlist");
            return;
        };
        self.fetch_me(jobs);
        self.add_to = Some(PlaylistPicker { track, cursor: 0, filter: String::new() });
    }

    /// Indices into `playlists` the user may add to (owned or collaborative)
    /// whose name contains `filter`.
    pub(crate) fn writable_playlists(&self, filter: &str) -> Vec<usize> {
        let needle = filter.trim().to_lowercase();
        self.playlists
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.collaborative
                    // Until the profile arrives, offer everything.
                    || self.me_id.as_deref().is_none_or(|me| p.owner_id == me)
            })
            .filter(|(_, p)| needle.is_empty() || p.name.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    fn on_key_add_to(&mut self, key: KeyEvent, jobs: &Jobs) {
        let Some(picker) = self.add_to.as_ref() else {
            return;
        };
        let candidates = self.writable_playlists(&picker.filter);
        match key.code {
            KeyCode::Esc => self.add_to = None,
            KeyCode::Up | KeyCode::Down => {
                if candidates.is_empty() {
                    return;
                }
                let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                if let Some(picker) = self.add_to.as_mut() {
                    picker.cursor =
                        wrap(picker.cursor as i64 + delta, candidates.len() as i64) as usize;
                }
            }
            KeyCode::Enter => {
                let Some(picker) = self.add_to.take() else {
                    return;
                };
                let Some(&index) = candidates.get(picker.cursor.min(candidates.len().saturating_sub(1)))
                else {
                    self.add_to = Some(picker);
                    return;
                };
                let playlist = self.playlists[index].clone();
                self.say(format!("Adding to {}...", playlist.name));
                self.pending += 1;
                let client = jobs.client.clone();
                let uri = picker.track.uri;
                let (playlist_id, name) = (playlist.id.clone(), playlist.name.clone());
                jobs.submit(
                    async move {
                        client.playlist_add(&playlist.id, &[uri]).await.map_err(|e| e.into_write_error())
                    },
                    move |outcome| AppEvent::PlaylistAdded { playlist_id, name, outcome },
                );
            }
            KeyCode::Backspace => {
                if let Some(picker) = self.add_to.as_mut() {
                    picker.filter.pop();
                    picker.cursor = 0;
                }
            }
            KeyCode::Char(c) => {
                if let Some(picker) = self.add_to.as_mut() {
                    picker.filter.push(c);
                    picker.cursor = 0;
                }
            }
            _ => {}
        }
    }

    // --- Keys ------------------------------------------------------------------

    /// Switch to the next named theme and remember it for the next start.
    fn cycle_theme(&mut self) {
        self.theme = self.theme.next();
        let name = self.theme.name;
        let saved = crate::config_dir().and_then(|dir| {
            let mut settings = crate::settings::load(&dir);
            settings.theme = Some(name.to_string());
            crate::settings::save(&dir, &settings)
        });
        match saved {
            Ok(()) => self.say(format!("Theme: {name}")),
            Err(e) => self.say(format!("Theme: {name} (not saved: {e})")),
        }
    }

    fn on_key(&mut self, key: KeyEvent, jobs: &Jobs) {
        // AltGr arrives as CONTROL|ALT on Windows and types a character.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            match key.code {
                KeyCode::Char('c') => self.should_quit = true,
                KeyCode::Char('t') => self.cycle_theme(),
                _ => {}
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
        if self.add_to.is_some() {
            self.on_key_add_to(key, jobs);
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
            Tab::Search if self.search_category == SearchCategory::Tracks => {
                Some((self.search_results.clone(), self.search_cursor))
            }
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
            // The search lists move through `search_move`.
            Tab::Search => return,
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
            Tab::Search if self.search_category == SearchCategory::Tracks => {
                self.search_results.get(self.search_cursor).cloned()
            }
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
        let Some(track) = self.selected_track() else {
            return;
        };
        if self.source == Source::Local {
            match self.local.clone() {
                Some(local) => {
                    // Joins the end of the local queue.
                    local.extend_queue(vec![track.clone()]);
                    self.apply_local_snapshot();
                    self.say(format!("Queued {}", track.name));
                }
                None => self.say("The local player is still starting"),
            }
            return;
        }
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
            MouseEventKind::ScrollUp if self.tab == Tab::Search => self.search_move(-1, jobs),
            MouseEventKind::ScrollDown if self.tab == Tab::Search => self.search_move(1, jobs),
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
                        if self.zones.search_input.contains(at) {
                            self.search_focus = SearchFocus::Input;
                        } else if let Some(i) =
                            self.zones.search_tabs.iter().position(|r| r.contains(at))
                        {
                            self.search_category = SearchCategory::ALL[i];
                            self.focus_search_results_or_keep();
                        } else if self.zones.results.contains(at) {
                            let index = row_in(self.zones.results, self.zones.results_offset);
                            let category = self.search_category;
                            if index >= self.search_len(category) {
                                return;
                            }
                            self.search_focus = SearchFocus::Results;
                            self.set_search_cursor(category, index);
                            self.activate_search_selection(jobs);
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
            AppEvent::SearchResults { seq, outcome } => {
                self.pending = self.pending.saturating_sub(1);
                if seq != self.search_seq {
                    return;
                }
                self.search_loading = false;
                match outcome {
                    Ok(page) => {
                        let count = page.tracks.len();
                        self.search_results = page.tracks;
                        self.search_more = page.more;
                        self.search_cursor = 0;
                        self.say(format!("{count} result{}", if count == 1 { "" } else { "s" }));
                    }
                    Err(e) => self.say_error(format!("search: {e}")),
                }
            }
            AppEvent::SearchMore { seq, outcome } => {
                self.pending = self.pending.saturating_sub(1);
                if seq != self.search_seq {
                    return;
                }
                self.search_more_loading = false;
                match outcome {
                    Ok(page) => {
                        for track in page.tracks {
                            if !self.search_results.iter().any(|t| t.uri == track.uri) {
                                self.search_results.push(track);
                            }
                        }
                        self.search_more = page.more;
                    }
                    Err(e) => self.say(format!("Couldn't load more results: {e}")),
                }
            }
            AppEvent::SearchExtra { seq, outcome } => {
                self.pending = self.pending.saturating_sub(1);
                if seq != self.search_seq {
                    return;
                }
                match outcome {
                    Ok(extras) => {
                        self.search_artists = extras.artists;
                        self.search_albums = extras.albums;
                        self.search_playlists = extras.playlists;
                        self.search_extra = ExtraState::Ready;
                    }
                    Err(_) => {
                        self.search_artists.clear();
                        self.search_albums.clear();
                        self.search_playlists.clear();
                        self.search_extra = ExtraState::Failed;
                    }
                }
                self.search_extra_cursors = [0; 3];
            }
            AppEvent::Me(result) => {
                self.pending = self.pending.saturating_sub(1);
                self.me_in_flight = false;
                if let Ok(user) = result {
                    if !user.id.is_empty() {
                        self.me_id = Some(user.id);
                    }
                }
            }
            AppEvent::LikeResult { uri, outcome } => {
                self.pending = self.pending.saturating_sub(1);
                if let Err(e) = outcome {
                    // Take back the optimistic row.
                    if self.tracks_key.as_deref() == Some(LIKED_KEY)
                        && self.tracks.first().is_some_and(|t| t.uri == uri)
                    {
                        self.tracks.remove(0);
                        self.tracks_cursor = self.tracks_cursor.saturating_sub(1);
                    }
                    self.say_error(format!("Couldn't like: {e}"));
                }
            }
            AppEvent::PlaylistAdded { playlist_id, name, outcome } => {
                self.pending = self.pending.saturating_sub(1);
                match outcome {
                    Ok(()) => {
                        if let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == playlist_id) {
                            playlist.track_count += 1;
                        }
                        // Opened in the Library tab right now: refetch it so
                        // the new song shows (loaded lists are not cached, so
                        // opening it later reads fresh data anyway).
                        if self.tracks_key.as_deref() == Some(playlist_id.as_str()) {
                            self.fetch_tracks(jobs, &playlist_id);
                        }
                        self.say(format!("Added to {name}"));
                    }
                    Err(e) => self.say_error(format!("Couldn't add to {name}: {e}")),
                }
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

/// Whether a query as typed should search by itself: long enough once
/// trimmed, and different from what was last searched.
fn wants_search(input: &str, searched_for: &str) -> bool {
    let query = input.trim();
    query.chars().count() >= SEARCH_MIN_CHARS && query != searched_for
}

/// The Web API id of a track (the last segment of its uri).
fn track_id(track: &Track) -> String {
    track.uri.rsplit(':').next().unwrap_or_default().to_string()
}

/// Track search: the session first (no quota), the Web API as fallback.
async fn search_tracks_job(
    client: &Client,
    local: Option<&LocalPlayer>,
    query: &str,
) -> Result<SearchPage> {
    let mut session_error = None;
    let mut session_empty = false;
    if let Some(local) = local {
        match crate::library::search(&local.session(), query, SEARCH_LIMIT as usize).await {
            Ok((tracks, rest)) if !tracks.is_empty() => {
                let more = if rest.is_empty() { MoreTracks::None } else { MoreTracks::Session(rest) };
                return Ok(SearchPage { tracks, more });
            }
            Ok(_) => session_empty = true,
            Err(e) => session_error = Some(e),
        }
    }
    match client.search_tracks_page(query, SEARCH_LIMIT, 0).await {
        Ok((tracks, next)) => Ok(SearchPage { tracks, more: next.map_or(MoreTracks::None, MoreTracks::Web) }),
        Err(_) if session_empty => Ok(SearchPage { tracks: Vec::new(), more: MoreTracks::None }),
        Err(e) => Err(session_error.unwrap_or_else(|| e.into())),
    }
}

/// Tracks of an `album:<id>` or `artist:<id>` key: the session first, the
/// Web API as fallback.
async fn catalog_tracks(client: &Client, local: Option<&LocalPlayer>, key: &str) -> Result<Vec<Track>> {
    let (kind, id) = key.split_once(':').unwrap_or_default();
    let mut session_error = None;
    if let Some(local) = local {
        let session = local.session();
        let result = match kind {
            "album" => crate::library::album_tracks(&session, id, LIBRARY_LIMIT as usize).await,
            _ => crate::library::artist_top_tracks(&session, &format!("spotify:artist:{id}"))
                .await
                .map(|(_, tracks)| tracks),
        };
        match result {
            Ok(tracks) if !tracks.is_empty() => return Ok(tracks),
            Ok(_) => {}
            Err(e) => session_error = Some(e),
        }
    }
    let web = match kind {
        "album" => client.album_tracks(id).await,
        _ => client.artist_top_tracks(id).await,
    };
    match web {
        Ok(tracks) => Ok(tracks),
        Err(e) => Err(session_error.unwrap_or_else(|| e.into())),
    }
}

/// Wrap an index around `len`, in either direction.
fn wrap(index: i64, len: i64) -> i64 {
    index.rem_euclid(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runtime plus jobs whose client never gets used: tests only press
    /// keys that do not touch the network.
    fn test_jobs() -> (tokio::runtime::Runtime, Jobs, mpsc::Receiver<AppEvent>) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("spotcli-test-{}-{n}", std::process::id()));
        crate::auth::TokenStore::save(
            &dir,
            &crate::auth::StoredCredentials {
                client_id: "x".into(),
                access_token: "t".into(),
                refresh_token: "r".into(),
                expires_at: i64::MAX,
            },
        )
        .unwrap();
        let client = Client::from_dir(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (tx, rx) = mpsc::channel();
        let jobs = Jobs { client, handle: runtime.handle().clone(), tx, local: None };
        (runtime, jobs, rx)
    }

    fn press(app: &mut App, jobs: &Jobs, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE), jobs);
    }

    fn track(n: usize) -> Track {
        Track { uri: format!("spotify:track:{n}"), name: format!("t{n}"), ..Default::default() }
    }

    fn with_results() -> App {
        let mut app = App::new();
        app.tab = Tab::Search;
        app.search_results = (0..3).map(track).collect();
        app.searched_for = "abc".into();
        app.search_input = "abc".into();
        app
    }

    #[test]
    fn debounce_waits_for_quiet_and_min_length() {
        let mut app = App::new();
        let t0 = Instant::now();
        app.search_input = "a".into();
        app.arm_search_debounce(t0);
        assert!(app.search_debounce.is_none(), "one character is too short");

        app.search_input = "ab".into();
        app.arm_search_debounce(t0);
        assert_eq!(app.search_debounce_left(t0), Some(SEARCH_DEBOUNCE));
        assert!(!app.take_due_search(t0 + SEARCH_DEBOUNCE - Duration::from_millis(1)));
        assert!(app.search_debounce.is_some());
        assert!(app.take_due_search(t0 + SEARCH_DEBOUNCE));
        assert!(app.search_debounce.is_none(), "fires once");
        assert!(!app.take_due_search(t0 + SEARCH_DEBOUNCE * 2));

        // A later keystroke pushes the deadline out.
        app.search_input = "abc".into();
        app.arm_search_debounce(t0 + Duration::from_millis(100));
        assert_eq!(
            app.search_debounce_left(t0 + Duration::from_millis(100)),
            Some(SEARCH_DEBOUNCE)
        );
    }

    #[test]
    fn debounce_skips_an_unchanged_query() {
        assert!(wants_search("  hello ", "world"));
        assert!(!wants_search("  hello ", "hello"));
        assert!(!wants_search(" h ", ""));
        let mut app = App::new();
        app.searched_for = "ab".into();
        app.search_input = "ab ".into();
        app.arm_search_debounce(Instant::now());
        assert!(app.search_debounce.is_none());
    }

    #[test]
    fn typing_arms_the_debounce_and_esc_disarms_it() {
        let (_rt, jobs, _rx) = test_jobs();
        let mut app = App::new();
        app.tab = Tab::Search;
        press(&mut app, &jobs, KeyCode::Char('h'));
        assert!(app.search_debounce.is_none());
        press(&mut app, &jobs, KeyCode::Char('i'));
        assert!(app.search_debounce.is_some());
        press(&mut app, &jobs, KeyCode::Esc);
        assert!(app.search_input.is_empty() && app.search_debounce.is_none());
    }

    #[test]
    fn focus_moves_between_input_and_results() {
        let (_rt, jobs, _rx) = test_jobs();
        let mut app = with_results();
        assert_eq!(app.search_focus, SearchFocus::Input);
        // Enter on the already searched query hands over the keyboard.
        press(&mut app, &jobs, KeyCode::Enter);
        assert_eq!(app.search_focus, SearchFocus::Results);
        // Letters are shortcuts now, not text.
        press(&mut app, &jobs, KeyCode::Char('x'));
        assert_eq!(app.search_input, "abc");
        press(&mut app, &jobs, KeyCode::Down);
        assert_eq!(app.search_cursor, 1);
        // `i` returns to the input with the text kept.
        press(&mut app, &jobs, KeyCode::Char('i'));
        assert_eq!(app.search_focus, SearchFocus::Input);
        assert_eq!(app.search_input, "abc");
        // Down goes back; `/` and Esc return again.
        press(&mut app, &jobs, KeyCode::Down);
        assert_eq!(app.search_focus, SearchFocus::Results);
        press(&mut app, &jobs, KeyCode::Char('/'));
        assert_eq!(app.search_focus, SearchFocus::Input);
        press(&mut app, &jobs, KeyCode::Down);
        press(&mut app, &jobs, KeyCode::Esc);
        assert_eq!(app.search_focus, SearchFocus::Input);
        assert_eq!(app.search_input, "abc", "Esc in Results keeps the text");
    }

    #[test]
    fn down_with_nothing_to_select_stays_in_the_input() {
        let (_rt, jobs, _rx) = test_jobs();
        let mut app = App::new();
        app.tab = Tab::Search;
        press(&mut app, &jobs, KeyCode::Down);
        assert_eq!(app.search_focus, SearchFocus::Input);
    }

    #[test]
    fn categories_switch_with_arrows_and_digits() {
        let (_rt, jobs, _rx) = test_jobs();
        let mut app = with_results();
        app.search_artists = vec![Artist::default()];
        press(&mut app, &jobs, KeyCode::Down);
        press(&mut app, &jobs, KeyCode::Right);
        assert_eq!(app.search_category, SearchCategory::Artists);
        press(&mut app, &jobs, KeyCode::Char('4'));
        assert_eq!(app.search_category, SearchCategory::Playlists);
        press(&mut app, &jobs, KeyCode::Right);
        assert_eq!(app.search_category, SearchCategory::Tracks);
        press(&mut app, &jobs, KeyCode::Left);
        assert_eq!(app.search_category, SearchCategory::Playlists);
    }

    #[test]
    fn playlist_picker_lists_only_editable_playlists() {
        let mut app = App::new();
        app.me_id = Some("me".into());
        let mk = |name: &str, owner: &str, collab: bool| Playlist {
            id: name.into(),
            name: name.into(),
            owner_id: owner.into(),
            collaborative: collab,
            ..Default::default()
        };
        app.playlists = vec![
            mk("Mine", "me", false),
            mk("Followed", "someone", false),
            mk("Shared", "someone", true),
        ];
        assert_eq!(app.writable_playlists(""), vec![0, 2]);
        assert_eq!(app.writable_playlists("sha"), vec![2]);
        app.me_id = None;
        assert_eq!(app.writable_playlists("").len(), 3);
    }

    #[test]
    fn picker_keys_navigate_and_cancel() {
        let (_rt, jobs, _rx) = test_jobs();
        let mut app = with_results();
        app.me_id = Some("me".into());
        app.playlists = (0..3)
            .map(|i| Playlist { id: format!("p{i}"), name: format!("P{i}"), owner_id: "me".into(), ..Default::default() })
            .collect();
        press(&mut app, &jobs, KeyCode::Down);
        press(&mut app, &jobs, KeyCode::Char('p'));
        assert!(app.add_to.is_some());
        press(&mut app, &jobs, KeyCode::Down);
        press(&mut app, &jobs, KeyCode::Down);
        assert_eq!(app.add_to.as_ref().unwrap().cursor, 2);
        press(&mut app, &jobs, KeyCode::Down);
        assert_eq!(app.add_to.as_ref().unwrap().cursor, 0, "wraps");
        press(&mut app, &jobs, KeyCode::Char('2'));
        assert_eq!(app.add_to.as_ref().unwrap().filter, "2");
        press(&mut app, &jobs, KeyCode::Esc);
        assert!(app.add_to.is_none());
        assert_eq!(app.search_focus, SearchFocus::Results);
    }

    #[test]
    fn late_search_replies_are_dropped() {
        let (_rt, jobs, _rx) = test_jobs();
        let mut app = App::new();
        app.search_seq = 2;
        app.search_loading = true;
        app.on_event(
            AppEvent::SearchResults {
                seq: 1,
                outcome: Ok(SearchPage { tracks: vec![track(1)], more: MoreTracks::None }),
            },
            &jobs,
        );
        assert!(app.search_results.is_empty() && app.search_loading);
        app.on_event(
            AppEvent::SearchResults {
                seq: 2,
                outcome: Ok(SearchPage { tracks: vec![track(1)], more: MoreTracks::Web(30) }),
            },
            &jobs,
        );
        assert_eq!(app.search_results.len(), 1);
        assert!(!app.search_loading);
        assert_eq!(app.search_more, MoreTracks::Web(30));
    }

    #[test]
    fn wrap_moves_in_both_directions() {
        assert_eq!(wrap(0, 3), 0);
        assert_eq!(wrap(-1, 3), 2);
        assert_eq!(wrap(3, 3), 0);
        assert_eq!(wrap(4, 3), 1);
    }
}
