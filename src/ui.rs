//! Ratatui rendering for the whole interface.
//!
//! No border boxes: panels are distinguished by three background layers
//! (`background` < `surface` < `surface_raised`) plus padding, with a small
//! caps title as the first line of each panel. A single accent color (the
//! album cover's, when known) marks focus, selection and playback.

use std::borrow::Cow;
use std::sync::OnceLock;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Clear, List, ListItem, Padding, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, StatefulWidget, Table, Widget,
};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    App, ExtraState, LyricsState, MoreTracks, MouseZones, SearchCategory, SearchFocus, Tab,
    COVER_CELLS, LIKED_KEY,
};
use crate::models::{format_duration, Playback, Track};
use crate::theme::Theme;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let mut theme = app.theme;
    theme.accent = accent(app);
    let theme = theme;
    let area = frame.area();

    // Offsets from last frame keep scrolling stable; zones are rebuilt below.
    let prev = app.zones;
    app.zones = MouseZones::default();

    frame.render_widget(Block::default().style(Style::default().bg(theme.background)), area);

    let gap = u16::from(area.height >= 24);
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(gap),
        Constraint::Min(3),
        Constraint::Length(COVER_CELLS.1 + 2),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(frame, app, &theme, chunks[0]);
    match app.tab {
        Tab::Library => draw_library(frame, app, &theme, chunks[2], &prev),
        Tab::Search => draw_search(frame, app, &theme, chunks[2], &prev),
        Tab::View => draw_view(frame, app, &theme, chunks[2], &prev),
    }
    let [shuffle_button, repeat_button] = draw_player(frame, app, &theme, chunks[3]);
    app.zones.shuffle_button = shuffle_button;
    app.zones.repeat_button = repeat_button;
    draw_footer(frame, app, &theme, chunks[4]);
    if app.add_to.is_some() {
        draw_add_to(frame, app, &theme, area);
    }
}

/// The one accent: the cover's color when sampled, else the theme's. Nudged
/// to stay readable against the theme's background.
fn accent(app: &App) -> Color {
    let theme = &app.theme;
    let Some(Color::Rgb(r, g, b)) = app.cover_accent else {
        return app.cover_accent.unwrap_or(theme.accent);
    };
    let dark_bg = match theme.background {
        Color::Rgb(r, g, b) => luma(r, g, b) < 128,
        _ => true,
    };
    let l = luma(r, g, b);
    let blend = |c: u8, target: u8, t: f32| (c as f32 + (target as f32 - c as f32) * t) as u8;
    let (target, t) = if dark_bg && l < 120 {
        (255, (120 - l) as f32 / (255 - l).max(1) as f32)
    } else if !dark_bg && l > 140 {
        (0, (l - 140) as f32 / l.max(1) as f32)
    } else {
        return Color::Rgb(r, g, b);
    };
    Color::Rgb(blend(r, target, t), blend(g, target, t), blend(b, target, t))
}

fn luma(r: u8, g: u8, b: u8) -> u32 {
    (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000
}

// --- Small helpers ------------------------------------------------------------

/// `s` cut to at most `max` terminal columns, ending in "…" when shortened.
fn truncate(s: &str, max: usize) -> Cow<'_, str> {
    if UnicodeWidthStr::width(s) <= max {
        return Cow::Borrowed(s);
    }
    if max == 0 {
        return Cow::Borrowed("");
    }
    let mut out = String::with_capacity(max + 3);
    let mut used = 0;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > max - 1 {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    Cow::Owned(out)
}

fn spinner_frame() -> &'static str {
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    SPINNER[(start.elapsed().as_millis() / 80) as usize % SPINNER.len()]
}

/// First visible item so that `cursor` stays on screen, keeping last frame's
/// offset when possible.
fn window(prev: usize, cursor: usize, len: usize, height: usize) -> usize {
    if height == 0 || len == 0 {
        return 0;
    }
    let max = len.saturating_sub(height);
    let cursor = cursor.min(len - 1);
    let mut offset = prev.min(max);
    if cursor < offset {
        offset = cursor;
    } else if cursor >= offset + height {
        offset = cursor + 1 - height;
    }
    offset.min(max)
}

/// A borderless panel: a background layer with padding and a title line.
/// Returns the content rect below the title.
fn panel(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    title: &str,
    focused: bool,
    loading: bool,
    right: Option<&str>,
) -> Rect {
    let block = Block::default()
        .style(Style::default().bg(theme.panel_bg(focused)))
        .padding(Padding::new(2, 2, 1, 0));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return inner;
    }
    let title_style = if focused {
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        theme.muted_text().add_modifier(Modifier::BOLD)
    };
    let right_w = right.map(|r| r.width() as u16 + 1).unwrap_or(0);
    let spin_w = if loading { 2 } else { 0 };
    let title_max = (inner.width.saturating_sub(right_w + spin_w)) as usize;
    let mut spans = vec![Span::styled(truncate(title, title_max).into_owned(), title_style)];
    if loading {
        spans.push(Span::styled(
            format!(" {}", spinner_frame()),
            Style::default().fg(theme.accent),
        ));
    }
    let row = Rect { height: 1, ..inner };
    frame.render_widget(Paragraph::new(Line::from(spans)), row);
    if let Some(right) = right {
        if right_w < inner.width {
            frame.render_widget(
                Paragraph::new(Span::styled(right.to_string(), theme.dim_text()))
                    .alignment(Alignment::Right),
                row,
            );
        }
    }
    Rect {
        y: inner.y + 1,
        height: inner.height - 1,
        ..inner
    }
}

/// A centered glyph, message and dim hint.
fn empty_state(frame: &mut Frame, theme: &Theme, area: Rect, glyph: &str, msg: &str, hint: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mut lines = vec![
        Line::from(Span::styled(glyph.to_string(), Style::default().fg(theme.accent))),
        Line::from(Span::styled(msg.to_string(), theme.muted_text())),
    ];
    if !hint.is_empty() {
        lines.push(Line::from(Span::styled(hint.to_string(), theme.dim_text())));
    }
    let h = (lines.len() as u16).min(area.height);
    let y = area.y + (area.height - h) / 2;
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        Rect { y, height: h, ..area },
    );
}

/// Builds the three text columns of a result row from its index.
type RowFn<'a> = Box<dyn Fn(usize) -> (String, String, String) + 'a>;

fn draw_scrollbar(frame: &mut Frame, theme: &Theme, items: Rect, len: usize, offset: usize) {
    let height = items.height as usize;
    if len <= height || items.height == 0 {
        return;
    }
    let max = len - height;
    let position = offset * (len - 1) / max;
    let mut state = ScrollbarState::new(len)
        .viewport_content_length(height)
        .position(position);
    let rect = Rect {
        x: items.right(),
        width: 1,
        ..items
    };
    Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(None)
        .thumb_symbol("┃")
        .thumb_style(theme.muted_text())
        .render(rect, frame.buffer_mut(), &mut state);
}

// --- Header and footer ----------------------------------------------------------

fn draw_header(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let area = Rect {
        x: area.x + 2.min(area.width),
        width: area.width.saturating_sub(4),
        ..area
    };
    let mut spans = vec![
        Span::styled(
            "spotcli",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
    ];
    for (tab, name) in [(Tab::Library, "Library"), (Tab::Search, "Search"), (Tab::View, "View")] {
        if app.tab == tab {
            spans.push(Span::styled(
                name,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ));
        } else {
            spans.push(Span::styled(name, theme.muted_text()));
        }
        spans.push(Span::raw("  "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    if let Some(playback) = app.playback.as_ref() {
        let mut text = String::new();
        if !playback.device_name.is_empty() {
            text.push_str(&playback.device_name);
        }
        if let Some(v) = playback.volume_percent {
            if !text.is_empty() {
                text.push_str("  ");
            }
            text.push_str(&format!("{v}%"));
        }
        if !text.is_empty() && (text.width() as u16) + 36 < area.width {
            frame.render_widget(
                Paragraph::new(Span::styled(text, theme.muted_text())).alignment(Alignment::Right),
                area,
            );
        }
    }
}

fn hints_for(app: &App) -> &'static [(&'static str, &'static str)] {
    match app.tab {
        Tab::Library if app.tracks_focused => &[
            ("enter", "play"),
            ("a", "queue"),
            ("space", "pause"),
            ("h", "playlists"),
            ("tab", "search"),
            ("q", "quit"),
        ],
        Tab::Library => &[
            ("enter", "open"),
            ("l", "tracks"),
            ("space", "pause"),
            ("n/p", "skip"),
            ("tab", "search"),
            ("q", "quit"),
        ],
        Tab::Search if app.search_focus == SearchFocus::Input => &[
            ("enter", "search"),
            ("↓", "results"),
            ("esc", "clear"),
            ("tab", "view"),
        ],
        Tab::Search if app.search_category == SearchCategory::Tracks => &[
            ("enter", "play"),
            ("a", "queue"),
            ("l", "like"),
            ("p", "playlist"),
            ("←→", "type"),
            ("/", "edit"),
        ],
        Tab::Search => &[
            ("enter", "open"),
            ("←→", "type"),
            ("1-4", "jump"),
            ("/", "edit"),
            ("esc", "back"),
            ("tab", "view"),
        ],
        Tab::View => &[
            ("enter", "play"),
            ("space", "pause"),
            ("n/p", "skip"),
            (",/.", "seek"),
            ("tab", "library"),
            ("q", "quit"),
        ],
    }
}

fn draw_footer(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let area = Rect {
        x: area.x + 2.min(area.width),
        width: area.width.saturating_sub(4),
        ..area
    };
    let now = Instant::now();
    if let Some((message, expires)) = app.status.as_ref() {
        if now < *expires {
            // Errors hold the bar for minutes, everything else a few seconds.
            let lower = message.to_lowercase();
            let (glyph, color) = if expires.saturating_duration_since(now).as_secs() > 60 {
                ("✗", theme.error)
            } else if ["fail", "error", "could not", "couldn't", "unable", "no active", "not "]
                .iter()
                .any(|w| lower.contains(w))
            {
                ("!", theme.warning)
            } else {
                ("•", theme.muted)
            };
            let text = truncate(message, area.width.saturating_sub(2) as usize).into_owned();
            let body = if color == theme.muted {
                theme.text()
            } else {
                Style::default().fg(color)
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!("{glyph} "), Style::default().fg(color)),
                    Span::styled(text, body),
                ])),
                area,
            );
            return;
        }
    }
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0usize;
    for (key, action) in hints_for(app).iter().take(6) {
        let w = key.width() + 1 + action.width() + if spans.is_empty() { 0 } else { 3 };
        if used + w > area.width as usize {
            break;
        }
        if !spans.is_empty() {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            *key,
            Style::default().fg(theme.foreground).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {action}"), theme.muted_text()));
        used += w;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// --- Library, search, view --------------------------------------------------------

fn playing_uri(app: &App) -> Option<(String, bool)> {
    let playback = app.playback.as_ref()?;
    let track = playback.track.as_ref()?;
    Some((track.uri.clone(), playback.is_playing))
}

fn draw_library(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect, prev: &MouseZones) {
    let columns = Layout::horizontal([Constraint::Percentage(30), Constraint::Fill(1)])
        .spacing(1)
        .split(area);

    let playlists_focused = !app.tracks_focused;
    let tracks_focused = app.tracks_focused;
    let loading = app.pending > 0;

    // Playlists: row 0 is Liked Songs.
    let content = panel(
        frame,
        columns[0],
        theme,
        "PLAYLISTS",
        playlists_focused,
        loading && playlists_focused,
        None,
    );
    let len = app.playlists.len() + 1;
    let height = content.height as usize;
    let offset = window(prev.playlists_offset, app.playlists_cursor, len, height);
    let cursor = app.playlists_cursor.min(len - 1);
    let width = content.width as usize;
    let bar_style = if playlists_focused {
        Style::default().fg(theme.accent)
    } else {
        theme.dim_text()
    };
    let items: Vec<ListItem> = (offset..len.min(offset + height))
        .map(|row| {
            let selected = row == cursor;
            let bar = if selected { "▌" } else { " " };
            let (name, count, name_style) = if row == 0 {
                (
                    Cow::Borrowed("Liked Songs"),
                    String::new(),
                    Style::default().fg(theme.accent),
                )
            } else {
                let playlist = &app.playlists[row - 1];
                let count = if playlist.track_count > 0 {
                    playlist.track_count.to_string()
                } else {
                    String::new()
                };
                (Cow::Borrowed(playlist.name.as_str()), count, theme.text())
            };
            let prefix = if row == 0 { "♥ " } else { "" };
            let avail = width.saturating_sub(2 + prefix.width());
            let count_w = count.width();
            let name_max = if count_w > 0 {
                avail.saturating_sub(count_w + 1)
            } else {
                avail
            };
            let name = truncate(&name, name_max).into_owned();
            let pad = avail.saturating_sub(name.width() + count_w);
            let mut item = ListItem::new(Line::from(vec![
                Span::styled(bar, bar_style),
                Span::raw(" "),
                Span::styled(format!("{prefix}{name}"), name_style),
                Span::raw(" ".repeat(pad)),
                Span::styled(count, theme.dim_text()),
            ]));
            if selected {
                item = item.style(theme.selected(playlists_focused));
            }
            item
        })
        .collect();
    frame.render_widget(List::new(items), content);
    draw_scrollbar(frame, theme, content, len, offset);
    app.zones.playlists = content;
    app.zones.playlists_offset = offset;
    app.zones.tracks_leading_rows = 0;

    // Tracks of the selected playlist.
    let title = match app.tracks_key.as_deref() {
        Some(LIKED_KEY) => "LIKED SONGS".to_string(),
        Some(key) => match (app.tracks_title.as_deref(), app.playlists.iter().find(|p| p.id == key)) {
            (Some(title), _) => title.to_uppercase(),
            (None, Some(playlist)) => playlist.name.to_uppercase(),
            (None, None) => "TRACKS".to_string(),
        },
        None => "TRACKS".to_string(),
    };
    let count = (!app.tracks.is_empty()).then(|| format!("{} tracks", app.tracks.len()));
    let content = panel(
        frame,
        columns[1],
        theme,
        &title,
        tracks_focused,
        loading && tracks_focused,
        count.as_deref(),
    );
    let (hint_glyph, hint_msg, hint_sub) = if loading {
        ("", "Loading tracks…", "")
    } else if app.tracks_key.is_none() {
        ("♪", "Pick a playlist", "Select one on the left and press enter")
    } else {
        ("♪", "No tracks here", "")
    };
    let playing = playing_uri(app);
    let (items, offset) = track_table(
        frame,
        theme,
        content,
        &app.tracks,
        app.tracks_cursor,
        prev.tracks_offset,
        tracks_focused,
        playing.as_ref().map(|(u, p)| (u.as_str(), *p)),
        (hint_glyph, hint_msg, hint_sub),
    );
    app.zones.tracks = items;
    app.zones.tracks_offset = offset;
}

fn draw_search(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect, prev: &MouseZones) {
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Fill(1)])
        .spacing(1)
        .split(area);
    let input_focused = app.search_focus == SearchFocus::Input;
    let results_focused = !input_focused;

    // Input: a raised field with a "/" prefix; the block cursor only shows
    // while the keyboard is typing into it.
    let field = Block::default()
        .style(Style::default().bg(if input_focused { theme.input_focused_bg } else { theme.input_bg }))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = field.inner(rows[0]);
    frame.render_widget(field, rows[0]);
    let prefix = if input_focused {
        Style::default().fg(theme.accent)
    } else {
        theme.dim_text()
    };
    let mut spans = vec![Span::styled("/ ", prefix)];
    let cursor = |spans: &mut Vec<Span>| {
        if input_focused {
            spans.push(Span::styled("▍", Style::default().fg(theme.accent)));
        }
    };
    if app.search_input.is_empty() {
        cursor(&mut spans);
        spans.push(Span::styled(
            if input_focused { " Search tracks, artists, albums, playlists" } else { "Search tracks, artists, albums, playlists" },
            theme.dim_text(),
        ));
    } else {
        let max = inner.width.saturating_sub(4) as usize;
        let text = tail(&app.search_input, max);
        spans.push(Span::styled(text, if input_focused { theme.text() } else { theme.muted_text() }));
        cursor(&mut spans);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
    app.zones.search_input = rows[0];

    let loading = app.search_loading || app.search_extra == ExtraState::Loading;
    let content = panel(frame, rows[1], theme, "RESULTS", results_focused, loading, None);
    if content.height == 0 {
        return;
    }
    let [tabs_row, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(content);
    draw_search_tabs(frame, app, theme, tabs_row, results_focused);

    let category = app.search_category;
    let extra_state = app.search_extra;
    let searched = !app.searched_for.is_empty();
    let playing = playing_uri(app);
    let (items, offset) = match category {
        SearchCategory::Tracks => {
            let more_loading = app.search_more_loading;
            let (table_area, spinner_row) = if more_loading && list_area.height > 4 {
                let [table, bottom] =
                    Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(list_area);
                (table, Some(bottom))
            } else {
                (list_area, None)
            };
            let (g, m, h) = if app.search_loading {
                ("", "Searching…", "")
            } else if searched {
                ("/", "No tracks found", "")
            } else {
                ("/", "Search for something", "Results appear as you type; enter searches now")
            };
            let result = track_table(
                frame,
                theme,
                table_area,
                &app.search_results,
                app.search_cursor,
                prev.results_offset,
                results_focused,
                playing.as_ref().map(|(u, p)| (u.as_str(), *p)),
                (g, m, h),
            );
            if let Some(row) = spinner_row {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("  {} Loading more…", spinner_frame()),
                        Style::default().fg(theme.accent),
                    )),
                    row,
                );
            }
            result
        }
        _ => {
            let len = app.search_len(category);
            let noun = category.label().to_lowercase();
            if len == 0 {
                let (g, m) = match extra_state {
                    ExtraState::Loading => ("", "Searching…".to_string()),
                    ExtraState::Failed => ("!", format!("Couldn't load {noun}")),
                    ExtraState::Ready => ("/", format!("No {noun} found")),
                    ExtraState::Idle => ("/", "Search for something".to_string()),
                };
                let hint = if extra_state == ExtraState::Failed {
                    "Spotify refused the request; try again in a moment"
                } else {
                    ""
                };
                empty_state(frame, theme, list_area, g, &m, hint);
                (Rect::default(), 0)
            } else {
                let cursor = app.search_cursor_of(category);
                let (heads, row): ([&str; 3], RowFn<'_>) = match category {
                    SearchCategory::Artists => (
                        ["NAME", "", "FOLLOWERS"],
                        Box::new(|i| {
                            let a = &app.search_artists[i];
                            (
                                a.name.clone(),
                                String::new(),
                                a.followers.map(compact_count).unwrap_or_default(),
                            )
                        }),
                    ),
                    SearchCategory::Albums => (
                        ["ALBUM", "ARTIST", "YEAR · TRACKS"],
                        Box::new(|i| {
                            let a = &app.search_albums[i];
                            let tail = match (a.year.is_empty(), a.track_count) {
                                (true, 0) => String::new(),
                                (true, n) => format!("{n} tracks"),
                                (false, 0) => a.year.clone(),
                                (false, n) => format!("{} · {n}", a.year),
                            };
                            (a.name.clone(), a.artists.clone(), tail)
                        }),
                    ),
                    _ => (
                        ["PLAYLIST", "OWNER", "TRACKS"],
                        Box::new(|i| {
                            let p = &app.search_playlists[i];
                            let count = if p.track_count > 0 { p.track_count.to_string() } else { String::new() };
                            (p.name.clone(), p.owner.clone(), count)
                        }),
                    ),
                };
                list_table(
                    frame,
                    theme,
                    list_area,
                    len,
                    cursor,
                    prev.results_offset,
                    results_focused,
                    heads,
                    &*row,
                )
            }
        }
    };
    app.zones.results = items;
    app.zones.results_offset = offset;
}

/// "Tracks 30 · Artists 20 ..." with the active category marked; fills the
/// click zones of each label.
fn draw_search_tabs(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect, focused: bool) {
    let mut spans: Vec<Span> = Vec::new();
    let mut x = area.x;
    for category in SearchCategory::ALL {
        let count = match category {
            SearchCategory::Tracks => {
                let more = if app.search_more != MoreTracks::None { "+" } else { "" };
                if app.search_loading && app.search_results.is_empty() {
                    String::new()
                } else {
                    format!(" {}{more}", app.search_results.len())
                }
            }
            _ => match app.search_extra {
                ExtraState::Loading | ExtraState::Idle => String::new(),
                ExtraState::Failed => " –".to_string(),
                ExtraState::Ready => format!(" {}", app.search_len(category)),
            },
        };
        let label = format!("{}{count}", category.label());
        let active = category == app.search_category;
        let style = if active && focused {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else if active {
            theme.text().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            theme.muted_text()
        };
        let width = label.width() as u16;
        let rect = Rect { x, y: area.y, width, height: 1 }.intersection(area);
        app.zones.search_tabs[category.index()] = rect;
        spans.push(Span::styled(label, style));
        x = x.saturating_add(width);
        if category != SearchCategory::Playlists {
            spans.push(Span::styled(" · ", theme.dim_text()));
            x = x.saturating_add(3);
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// 1200 -> "1.2K", 3_400_000 -> "3.4M".
fn compact_count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}K", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// A three-column list (name, secondary, right-aligned detail) with a header
/// and divider, like `track_table`. Builds only the visible rows. Returns the
/// item rows rect and the scroll offset.
#[allow(clippy::too_many_arguments)]
fn list_table(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    len: usize,
    cursor: usize,
    prev_offset: usize,
    focused: bool,
    heads: [&str; 3],
    row: &dyn Fn(usize) -> (String, String, String),
) -> (Rect, usize) {
    if area.height < 3 || len == 0 {
        return (Rect::default(), 0);
    }
    let [header, divider, items] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    let widths = [
        Constraint::Length(4),
        Constraint::Fill(1),
        Constraint::Percentage(30),
        Constraint::Length(16),
    ];
    let col_rects = Layout::horizontal(widths).spacing(1).split(items);
    let (name_w, sub_w, right_w) = (
        col_rects[1].width as usize,
        col_rects[2].width as usize,
        col_rects[3].width as usize,
    );
    let head = Row::new([
        Cell::from("   #"),
        Cell::from(heads[0].to_string()),
        Cell::from(heads[1].to_string()),
        Cell::from(Line::from(heads[2].to_string()).right_aligned()),
    ])
    .style(theme.dim_text());
    frame.render_widget(Table::new(Vec::<Row>::new(), widths).header(head).column_spacing(1), header);
    frame.render_widget(
        Paragraph::new(Span::styled("─".repeat(divider.width as usize), theme.dim_text())),
        divider,
    );

    let height = items.height as usize;
    let offset = window(prev_offset, cursor, len, height);
    let cursor = cursor.min(len - 1);
    let bar_style = if focused {
        Style::default().fg(theme.accent)
    } else {
        theme.dim_text()
    };
    let rows: Vec<Row> = (offset..len.min(offset + height))
        .map(|i| {
            let selected = i == cursor;
            let (name, sub, right) = row(i);
            let line = Row::new([
                Cell::from(Line::from(vec![
                    Span::styled(if selected { "▌" } else { " " }, bar_style),
                    Span::styled(format!("{:>3}", i + 1), theme.dim_text()),
                ])),
                Cell::from(Span::styled(truncate(&name, name_w).into_owned(), theme.text())),
                Cell::from(Span::styled(truncate(&sub, sub_w).into_owned(), theme.muted_text())),
                Cell::from(
                    Line::from(Span::styled(truncate(&right, right_w).into_owned(), theme.dim_text()))
                        .right_aligned(),
                ),
            ]);
            if selected {
                line.style(theme.selected(focused))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Table::new(rows, widths).column_spacing(1), items);
    draw_scrollbar(frame, theme, items, len, offset);
    (items, offset)
}

/// The centered "add to playlist" overlay.
fn draw_add_to(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(picker) = app.add_to.as_ref() else {
        return;
    };
    let candidates = app.writable_playlists(&picker.filter);
    let width = area.width.saturating_sub(4).min(60);
    let height = (candidates.len() as u16 + 6).min(area.height.saturating_sub(2)).min(20);
    if width < 24 || height < 7 {
        return;
    }
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .style(Style::default().bg(theme.surface_raised))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let [title, subtitle, filter, list, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let w = inner.width as usize;
    frame.render_widget(
        Paragraph::new(Span::styled(
            "ADD TO PLAYLIST",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )),
        title,
    );
    let what = format!("{} — {}", picker.track.name, picker.track.artists);
    frame.render_widget(
        Paragraph::new(Span::styled(truncate(&what, w).into_owned(), theme.muted_text())),
        subtitle,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/ ", Style::default().fg(theme.accent)),
            Span::styled(tail(&picker.filter, w.saturating_sub(4)).to_string(), theme.text()),
            Span::styled("▍", Style::default().fg(theme.accent)),
        ])),
        filter,
    );
    if candidates.is_empty() {
        let msg = if app.playlists.is_empty() {
            "Playlists are still loading"
        } else {
            "No playlists you can edit match"
        };
        empty_state(frame, theme, list, "", msg, "");
    } else {
        let height = list.height as usize;
        let cursor = picker.cursor.min(candidates.len() - 1);
        let offset = window(0, cursor, candidates.len(), height);
        let items: Vec<ListItem> = (offset..candidates.len().min(offset + height))
            .map(|row| {
                let playlist = &app.playlists[candidates[row]];
                let selected = row == cursor;
                let count = if playlist.track_count > 0 {
                    playlist.track_count.to_string()
                } else {
                    String::new()
                };
                let avail = w.saturating_sub(2);
                let name = truncate(&playlist.name, avail.saturating_sub(count.width() + 1)).into_owned();
                let pad = avail.saturating_sub(name.width() + count.width());
                let mut item = ListItem::new(Line::from(vec![
                    Span::styled(if selected { "▌ " } else { "  " }, Style::default().fg(theme.accent)),
                    Span::styled(name, theme.text()),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(count, theme.dim_text()),
                ]));
                if selected {
                    item = item.style(theme.selected(true));
                }
                item
            })
            .collect();
        frame.render_widget(List::new(items), list);
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("enter", Style::default().fg(theme.foreground).add_modifier(Modifier::BOLD)),
            Span::styled(" add   ", theme.muted_text()),
            Span::styled("↑↓", Style::default().fg(theme.foreground).add_modifier(Modifier::BOLD)),
            Span::styled(" select   ", theme.muted_text()),
            Span::styled("esc", Style::default().fg(theme.foreground).add_modifier(Modifier::BOLD)),
            Span::styled(" cancel", theme.muted_text()),
        ])),
        footer,
    );
}

/// The last `max` columns of `s`, so a long query keeps its cursor in view.
fn tail(s: &str, max: usize) -> &str {
    if s.width() <= max {
        return s;
    }
    let mut used = 0;
    for (i, c) in s.char_indices().rev() {
        used += UnicodeWidthChar::width(c).unwrap_or(0);
        if used > max {
            return &s[i + c.len_utf8()..];
        }
    }
    s
}

/// The View tab: the playing artist's top tracks plus lyrics.
fn draw_view(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect, prev: &MouseZones) {
    let columns = Layout::horizontal([Constraint::Fill(5), Constraint::Fill(3)])
        .spacing(1)
        .split(area);
    let heading = app.view_heading.as_str();
    let is_status = matches!(heading, "Loading")
        || heading.starts_with("Needs the built-in")
        || heading.starts_with("Couldn't load");
    let loading = heading == "Loading" || heading.starts_with("Couldn't load");

    let title = if heading.is_empty() || is_status {
        "ARTIST VIEW".to_string()
    } else {
        heading.to_uppercase()
    };
    let content = panel(frame, columns[0], theme, &title, true, loading, None);
    let (g, m, h) = if heading == "Loading" {
        ("", "Loading…", "")
    } else if is_status {
        ("♪", heading, "")
    } else {
        ("♪", "Nothing to show yet", "Play a track to see its artist")
    };
    let playing = playing_uri(app);
    let (items, offset) = track_table(
        frame,
        theme,
        content,
        &app.view_tracks,
        app.view_cursor,
        prev.view_offset,
        true,
        playing.as_ref().map(|(u, p)| (u.as_str(), *p)),
        (g, m, h),
    );
    app.zones.view = items;
    app.zones.view_offset = offset;

    let lyrics_loading = app.lyrics_state == LyricsState::Loading;
    let content = panel(frame, columns[1], theme, "LYRICS", false, lyrics_loading, None);
    if lyrics_loading {
        empty_state(frame, theme, content, "", "Loading lyrics", "");
        return;
    }
    let Some(lyrics) = app.view_lyrics.as_ref().filter(|_| app.lyrics_state == LyricsState::Ready) else {
        empty_state(frame, theme, content, "♪", "No lyrics for this track", "");
        return;
    };
    if lyrics.lines.is_empty() {
        empty_state(frame, theme, content, "♪", "No lyrics for this track", "");
        return;
    }
    let height = content.height as usize;
    let width = content.width as usize;
    let progress = app
        .playback
        .as_ref()
        .map(|p| interpolated_progress(p, app.playback_at))
        .unwrap_or(0);
    let current = lyrics.current_line(progress);
    // Unsynced lyrics have no position: plain, from the top.
    let origin = match current {
        Some(active) if lyrics.synced => active.saturating_sub(height / 2),
        _ => 0,
    }
    .min(lyrics.lines.len().saturating_sub(height));
    let lines: Vec<Line> = lyrics
        .lines
        .iter()
        .enumerate()
        .skip(origin)
        .take(height)
        .map(|(i, (_, text))| {
            let style = if !lyrics.synced {
                theme.text()
            } else if Some(i) == current {
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
            } else if current.is_some_and(|c| i < c) {
                theme.dim_text()
            } else {
                theme.muted_text()
            };
            Line::from(Span::styled(truncate(text, width).into_owned(), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), content);
}

/// A track table (marker, title, artist, duration) with a header and divider.
/// Only the visible rows are built. Returns the item rows rect and the
/// scroll offset.
#[allow(clippy::too_many_arguments)]
fn track_table(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    tracks: &[Track],
    cursor: usize,
    prev_offset: usize,
    focused: bool,
    playing: Option<(&str, bool)>,
    empty: (&str, &str, &str),
) -> (Rect, usize) {
    if tracks.is_empty() {
        empty_state(frame, theme, area, empty.0, empty.1, empty.2);
        return (Rect::default(), 0);
    }
    if area.height < 3 {
        return (Rect::default(), 0);
    }
    let [header, divider, items] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);

    let widths = [
        Constraint::Length(4),
        Constraint::Fill(1),
        Constraint::Percentage(30),
        Constraint::Length(7),
    ];
    let col_rects = Layout::horizontal(widths).spacing(1).split(items);
    let (title_w, artist_w) = (col_rects[1].width as usize, col_rects[2].width as usize);

    let head = Row::new([
        Cell::from("   #"),
        Cell::from("TITLE"),
        Cell::from("ARTIST"),
        Cell::from(Line::from("TIME").right_aligned()),
    ])
    .style(theme.dim_text());
    frame.render_widget(Table::new(Vec::<Row>::new(), widths).header(head).column_spacing(1), header);
    frame.render_widget(
        Paragraph::new(Span::styled("─".repeat(divider.width as usize), theme.dim_text())),
        divider,
    );

    let height = items.height as usize;
    let len = tracks.len();
    let offset = window(prev_offset, cursor, len, height);
    let cursor = cursor.min(len - 1);
    let bar_style = if focused {
        Style::default().fg(theme.accent)
    } else {
        theme.dim_text()
    };
    let rows: Vec<Row> = (offset..len.min(offset + height))
        .map(|i| {
            let track = &tracks[i];
            let selected = i == cursor;
            let is_current = playing.is_some_and(|(uri, _)| !uri.is_empty() && uri == track.uri);
            let bar = Span::styled(if selected { "▌" } else { " " }, bar_style);
            let (marker, title_style) = if is_current {
                let glyph = if playing.is_some_and(|(_, p)| p) { "▶" } else { "♪" };
                (
                    Span::styled(format!("  {glyph}"), Style::default().fg(theme.accent)),
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                )
            } else {
                (Span::styled(format!("{:>3}", i + 1), theme.dim_text()), theme.text())
            };
            let row = Row::new([
                Cell::from(Line::from(vec![bar, marker])),
                Cell::from(Span::styled(truncate(&track.name, title_w).into_owned(), title_style)),
                Cell::from(Span::styled(
                    truncate(&track.artists, artist_w).into_owned(),
                    theme.muted_text(),
                )),
                Cell::from(
                    Line::from(Span::styled(track.duration_label(), theme.dim_text()))
                        .right_aligned(),
                ),
            ]);
            if selected {
                row.style(theme.selected(focused))
            } else {
                row
            }
        })
        .collect();
    frame.render_widget(Table::new(rows, widths).column_spacing(1), items);
    draw_scrollbar(frame, theme, items, len, offset);
    (items, offset)
}

// --- Now playing --------------------------------------------------------------------

fn draw_player(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) -> [Rect; 2] {
    let block = Block::default()
        .style(Style::default().bg(theme.surface_raised))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return [Rect::default(); 2];
    }

    let (cw, ch) = COVER_CELLS;
    let show_cover = inner.width >= cw + 2 + 14 && inner.height >= ch;
    let info = if show_cover {
        let cover = Rect {
            x: inner.x,
            y: inner.y + (inner.height - ch) / 2,
            width: cw,
            height: ch,
        };
        draw_cover(frame, app, theme, cover);
        Rect {
            x: inner.x + cw + 2,
            width: inner.width - cw - 2,
            ..inner
        }
    } else {
        inner
    };

    // Text block: title, artist, device, spacer, progress (bottom aligned).
    let rows = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(info);

    let Some(playback) = app.playback.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled("Nothing playing", theme.muted_text())),
            rows[1],
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Pick a track and press enter",
                theme.dim_text(),
            )),
            rows[2],
        );
        return [Rect::default(); 2];
    };

    let track = playback.track.as_ref();
    let title = track.map(|t| t.name.as_str()).unwrap_or("(unknown track)");
    let artist = track.map(|t| t.artists.as_str()).unwrap_or_default();
    let album = track.map(|t| t.album.as_str()).unwrap_or_default();

    // Line 1: title on the left, shuffle/repeat on the right.
    let on = Style::default().fg(theme.accent);
    let off = theme.dim_text();
    let (repeat, repeat_style) = match playback.repeat.as_str() {
        "track" => ("⟲¹", on),
        "context" => ("⟲", on),
        _ => ("⟲", off),
    };
    let flags = Line::from(vec![
        Span::styled("⇄", if playback.shuffle { on } else { off }),
        Span::raw("  "),
        Span::styled(repeat, repeat_style),
    ]);
    let flags_w = flags.width() as u16;
    let [title_area, flags_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(flags_w + 1)]).areas(rows[1]);
    let mut title_spans = Vec::new();
    let mut title_max = title_area.width as usize;
    let title_style = if playback.is_playing {
        theme.text().add_modifier(Modifier::BOLD)
    } else {
        theme.muted_text().add_modifier(Modifier::BOLD)
    };
    if !playback.is_playing {
        title_spans.push(Span::styled("⏸ ", theme.muted_text()));
        title_max = title_max.saturating_sub(2);
    }
    title_spans.push(Span::styled(truncate(title, title_max).into_owned(), title_style));
    frame.render_widget(Paragraph::new(Line::from(title_spans)), title_area);
    frame.render_widget(Paragraph::new(flags).alignment(Alignment::Right), flags_area);
    // Click targets: shuffle glyph (plus a cell of slack), then repeat.
    let flags_x = flags_area.right().saturating_sub(flags_w);
    let buttons = [
        Rect::new(flags_x.saturating_sub(1), flags_area.y, 3, 1).intersection(flags_area),
        Rect::new(flags_x + 2, flags_area.y, flags_w.saturating_sub(2) + 1, 1).intersection(flags_area),
    ];

    // Line 2: artist and album.
    let sub = if album.is_empty() {
        artist.to_string()
    } else {
        format!("{artist} · {album}")
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate(&sub, info.width as usize).into_owned(),
            theme.muted_text(),
        )),
        rows[2],
    );

    // Line 3: device and volume.
    let device = if playback.device_name.is_empty() {
        "No device"
    } else {
        playback.device_name.as_str()
    };
    let mut line3 = vec![Span::styled(
        format!("♪ {}", truncate(device, (info.width as usize).saturating_sub(16))),
        theme.dim_text(),
    )];
    if let Some(v) = playback.volume_percent {
        let filled = ((v as usize * 5 + 50) / 100).min(5);
        let bars = ["▁", "▂", "▃", "▄", "▅"];
        line3.push(Span::raw("  "));
        for (i, b) in bars.iter().enumerate() {
            let style = if i < filled { theme.muted_text() } else { theme.dim_text() };
            line3.push(Span::styled(*b, style));
        }
        line3.push(Span::styled(format!(" {v}%"), theme.dim_text()));
    }
    frame.render_widget(Paragraph::new(Line::from(line3)), rows[3]);

    // Last line: progress.
    let elapsed = interpolated_progress(playback, app.playback_at);
    let ratio = if playback.duration_ms > 0 {
        (elapsed as f64 / playback.duration_ms as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    frame.render_widget(
        ProgressLine {
            elapsed: format_duration(elapsed),
            total: format_duration(playback.duration_ms),
            ratio,
            fill: theme.accent,
            head: theme.foreground,
            rest: theme.border,
            text: theme.muted_text(),
        },
        rows[5],
    );
    buttons
}

/// "1:12 ━━━━━━●──────── 3:42": accent fill, foreground head.
struct ProgressLine {
    elapsed: String,
    total: String,
    ratio: f64,
    fill: Color,
    head: Color,
    rest: Color,
    text: Style,
}

impl Widget for ProgressLine {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width < 6 {
            return;
        }
        let time_w = self.elapsed.width().max(self.total.width()) as u16;
        if area.width < time_w * 2 + 4 {
            return;
        }
        buf.set_string(area.x, area.y, &self.elapsed, self.text);
        buf.set_string(
            area.right() - self.total.width() as u16,
            area.y,
            &self.total,
            self.text,
        );
        let x0 = area.x + time_w + 1;
        let x1 = area.right() - time_w - 1;
        let bar_w = x1.saturating_sub(x0);
        if bar_w == 0 {
            return;
        }
        let head = (self.ratio * (bar_w - 1) as f64).round() as u16;
        for i in 0..bar_w {
            let (symbol, color) = match i.cmp(&head) {
                std::cmp::Ordering::Less => ("━", self.fill),
                std::cmp::Ordering::Equal => ("●", self.head),
                std::cmp::Ordering::Greater => ("─", self.rest),
            };
            buf[(x0 + i, area.y)].set_symbol(symbol).set_fg(color);
        }
    }
}

/// The album-art panel: a terminal-native bitmap (kitty/sixel) or cells
/// when the terminal has no image protocol. `area` is exactly COVER_CELLS.
fn draw_cover(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let placeholder = |frame: &mut Frame, area: Rect| {
        frame.render_widget(
            Block::default().style(Style::default().bg(theme.surface)),
            area,
        );
        let mid = Rect {
            y: area.y + area.height / 2,
            height: 1,
            ..area
        };
        frame.render_widget(
            Paragraph::new(Span::styled("♪", theme.dim_text())).alignment(Alignment::Center),
            mid,
        );
    };
    let Some((_, protocol)) = app.cover.as_ref() else {
        placeholder(frame, area);
        return;
    };
    let mut drawn = false;
    if let Some(placeholder_area) = protocol.needs_placeholder(area) {
        placeholder(frame, placeholder_area);
        // Sixel-style protocols draw over the whole area only when it fits.
        drawn = placeholder_area == area;
    }
    if !drawn {
        frame.render_widget(ratatui_image::Image::new(protocol), area);
    }
}

/// The shown progress advances between polls while Spotify reports playing.
fn interpolated_progress(playback: &Playback, at: Option<Instant>) -> u64 {
    if !playback.is_playing {
        return playback.progress_ms;
    }
    let elapsed = at.map(|at| at.elapsed().as_millis() as u64).unwrap_or(0);
    (playback.progress_ms + elapsed).min(playback.duration_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn track(n: usize) -> Track {
        Track {
            id: format!("id{n}"),
            uri: format!("spotify:track:{n}"),
            name: format!("A rather long track name number {n} — 日本語のタイトル"),
            artists: "Some Artist, Another Artist".into(),
            album: "Album".into(),
            duration_ms: 200_000 + n as u64,
            cover_url: None,
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.playlists = (0..40)
            .map(|i| crate::models::Playlist {
                id: format!("p{i}"),
                uri: format!("spotify:playlist:{i}"),
                name: format!("Playlist {i}"),
                owner: "me".into(),
                owner_id: "me".into(),
                collaborative: false,
                track_count: i as u64,
            })
            .collect();
        app.tracks = (0..100).map(track).collect();
        app.tracks_key = Some(LIKED_KEY.to_string());
        app.search_results = (0..5).map(track).collect();
        app.view_tracks = (0..5).map(track).collect();
        app.view_lyrics = Some(crate::library::Lyrics {
            provider: "test".into(),
            lines: vec![(0, "one".into()), (1000, "two".into()), (2000, "three".into())],
            synced: true,
        });
        app.playback = Some(Playback {
            track: Some(track(3)),
            is_playing: true,
            progress_ms: 72_000,
            duration_ms: 222_000,
            shuffle: true,
            repeat: "track".into(),
            volume_percent: Some(70),
            device_name: "This terminal".into(),
        });
        app.playback_at = Some(Instant::now());
        app
    }

    fn render(app: &mut App, w: u16, h: u16) {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
    }

    #[test]
    fn shuffle_and_repeat_buttons_cover_their_glyphs() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let glyphs = |zone: Rect| -> String {
            zone.positions().map(|p| buf[(p.x, p.y)].symbol().to_string()).collect()
        };
        assert!(glyphs(app.zones.shuffle_button).contains('⇄'));
        assert!(glyphs(app.zones.repeat_button).contains('⟲'));
        assert!(!glyphs(app.zones.shuffle_button).contains('⟲'));
    }

    #[test]
    fn draws_every_tab_at_several_sizes() {
        for theme in [Theme::dark(), Theme::light(), Theme::terminal()] {
            for (w, h) in [(100, 30), (60, 20), (20, 5), (1, 1), (200, 60)] {
                for tab in [Tab::Library, Tab::Search, Tab::View] {
                    let mut app = app();
                    app.theme = theme;
                    app.tab = tab;
                    app.tracks_cursor = 99;
                    app.playlists_cursor = 25;
                    app.tracks_focused = true;
                    render(&mut app, w, h);
                    app.playback = None;
                    app.tracks.clear();
                    app.search_results.clear();
                    app.view_tracks.clear();
                    app.view_lyrics = None;
                    render(&mut app, w, h);
                }
            }
        }
    }

    #[test]
    fn search_tab_draws_categories_focus_and_overlay() {
        for focus in [SearchFocus::Input, SearchFocus::Results] {
            for category in SearchCategory::ALL {
                for state in [ExtraState::Idle, ExtraState::Loading, ExtraState::Ready, ExtraState::Failed] {
                    let mut app = app();
                    app.tab = Tab::Search;
                    app.search_focus = focus;
                    app.search_category = category;
                    app.search_extra = state;
                    app.searched_for = "x".into();
                    app.search_more = MoreTracks::Web(5);
                    app.search_more_loading = true;
                    app.search_artists = vec![crate::models::Artist { name: "Band".into(), followers: Some(1_500_000), ..Default::default() }];
                    app.search_albums = vec![crate::models::Album { name: "LP".into(), year: "2020".into(), track_count: 9, ..Default::default() }];
                    app.search_playlists = vec![crate::models::Playlist { name: "Mix".into(), ..Default::default() }];
                    for (w, h) in [(100, 30), (40, 12), (20, 5)] {
                        render(&mut app, w, h);
                    }
                    app.add_to = Some(crate::app::PlaylistPicker {
                        track: track(1),
                        cursor: 3,
                        filter: String::new(),
                    });
                    app.me_id = Some("me".into());
                    for (w, h) in [(100, 30), (40, 12), (20, 5)] {
                        render(&mut app, w, h);
                    }
                }
            }
        }
    }

    #[test]
    fn input_cursor_only_shows_while_typing() {
        let text = |app: &mut App| -> String {
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect()
        };
        let mut app = app();
        app.tab = Tab::Search;
        app.search_focus = SearchFocus::Input;
        assert!(text(&mut app).contains('▍'));
        app.search_focus = SearchFocus::Results;
        assert!(!text(&mut app).contains('▍'));
        assert!(app.zones.search_tabs.iter().all(|r| r.width > 0));
    }

    #[test]
    fn truncates_by_width() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("日本語", 5), "日本…");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn selection_scrolls_into_view_and_fills_zones() {
        let mut app = app();
        app.tracks_cursor = 80;
        render(&mut app, 100, 30);
        assert!(app.zones.tracks_offset > 0);
        assert!(app.zones.tracks.height > 0);
        assert!(app.zones.tracks_offset <= 80);
        assert!(80 < app.zones.tracks_offset + app.zones.tracks.height as usize);
    }
}
