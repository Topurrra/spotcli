//! Ratatui rendering for the whole interface.
//!
//! Every widget reads [`Theme`] (ported from OpenTUI), uses rounded borders,
//! and shows a focus ring on the pane the keyboard or mouse is driving.

use std::time::Instant;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Tabs,
};
use ratatui::Frame;

use crate::app::{App, Tab, LIKED_KEY};
use crate::models::{format_duration, Playback};
use crate::theme::Theme;

pub fn draw(frame: &mut Frame, app: &App) {
    let theme = app.theme;
    let area = frame.area();

    frame.render_widget(Block::default().style(Style::default().bg(theme.background)), area);

    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(frame, app, &theme, chunks[0]);
    match app.tab {
        Tab::Library => draw_library(frame, app, &theme, chunks[1]),
        Tab::Search => draw_search(frame, app, &theme, chunks[1]),
    }
    draw_player(frame, app, &theme, chunks[2]);
    draw_footer(frame, app, &theme, chunks[3]);
}

fn draw_header(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let tabs = Tabs::new(vec!["Library", "Search"])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(theme.border(false))
                .title(Span::styled(
                    " spotcli ",
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                )),
        )
        .style(theme.muted_text())
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
                .underline_color(theme.accent),
        )
        .select(match app.tab {
            Tab::Library => 0,
            Tab::Search => 1,
        });
    frame.render_widget(tabs, area);
}

fn draw_library(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let columns = Layout::horizontal([Constraint::Percentage(30), Constraint::Fill(1)]).split(area);

    let playlists_focused = app.tab == Tab::Library && !app.tracks_focused;
    let tracks_focused = app.tab == Tab::Library && app.tracks_focused;

    let mut playlist_items: Vec<ListItem> = Vec::with_capacity(app.playlists.len() + 1);
    playlist_items.push(ListItem::new(Line::from(Span::styled(
        "Liked Songs",
        Style::default().fg(theme.accent),
    ))));
    for playlist in &app.playlists {
        playlist_items.push(ListItem::new(Line::from(vec![
            Span::styled(playlist.name.clone(), theme.text()),
            Span::styled(
                format!("  {} ({})", playlist.owner, playlist.track_count),
                theme.muted_text(),
            ),
        ])));
    }
    let playlist_list = List::new(playlist_items)
        .block(block(theme, " Playlists ", playlists_focused))
        .highlight_style(theme.highlight());
    frame.render_stateful_widget(
        playlist_list,
        columns[0],
        &mut list_state(app.playlists_cursor + 1),
    );

    let title = match app.tracks_key.as_deref() {
        Some(LIKED_KEY) => " Liked Songs ".to_string(),
        Some(key) => match app.playlists.iter().find(|p| p.id == key) {
            Some(playlist) => format!(" {} ", playlist.name),
            None => " Tracks ".to_string(),
        },
        None => " Tracks ".to_string(),
    };

    let track_items: Vec<ListItem> = app
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| track_row(theme, index, track))
        .collect();
    let track_list = List::new(track_items)
        .block(block(theme, &title, tracks_focused))
        .highlight_style(theme.highlight());
    frame.render_stateful_widget(track_list, columns[1], &mut list_state(app.tracks_cursor));
}

fn draw_search(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).split(area);

    let input = Paragraph::new(Line::from(vec![
        Span::styled(app.search_input.clone(), theme.text()),
        Span::styled("_", Style::default().fg(theme.cursor)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(
                " Search: type, Enter to run ",
                Style::default().fg(theme.focus),
            ))
            .style(Style::default().bg(theme.input_focused_bg)),
    );
    frame.render_widget(input, rows[0]);

    let items: Vec<ListItem> = app
        .search_results
        .iter()
        .enumerate()
        .map(|(index, track)| track_row(theme, index, track))
        .collect();
    let results = List::new(items)
        .block(block(theme, " Results ", false))
        .highlight_style(theme.highlight());
    frame.render_stateful_widget(results, rows[1], &mut list_state(app.search_cursor));
}

fn draw_player(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let block = block(theme, " Now Playing ", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);

    let Some(playback) = app.playback.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Nothing playing. Press Enter on a track to start.",
                theme.muted_text(),
            )),
            rows[0],
        );
        return;
    };

    let track = playback.track.as_ref();
    let title = track.map(|t| t.name.as_str()).unwrap_or("(unknown track)");
    let artists = track.map(|t| t.artists.as_str()).unwrap_or_default();

    let state_symbol = if playback.is_playing { "▶" } else { "⏸" };
    let repeat_symbol = match playback.repeat.as_str() {
        "track" => "🔁1",
        "context" => "🔁",
        _ => "↻",
    };
    let shuffle_symbol = if playback.shuffle { "🔀" } else { "  " };
    let volume = playback
        .volume_percent
        .map(|v| format!(" {v}%"))
        .unwrap_or_default();
    let device = if playback.device_name.is_empty() {
        String::new()
    } else {
        format!(" · {}", playback.device_name)
    };

    let left = Line::from(vec![
        Span::styled(
            format!("{state_symbol} "),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(title.to_string(), theme.text().add_modifier(Modifier::BOLD)),
        Span::styled(format!(" — {artists}"), theme.muted_text()),
    ]);
    let right = Line::from(vec![Span::styled(
        format!("{shuffle_symbol} {repeat_symbol}{volume}{device}"),
        theme.muted_text(),
    )]);
    frame.render_widget(Paragraph::new(left), rows[0]);
    frame.render_widget(
        Paragraph::new(right).alignment(ratatui::layout::Alignment::Right),
        rows[0],
    );

    let elapsed = interpolated_progress(playback, app.playback_at);
    let ratio = if playback.duration_ms > 0 {
        (elapsed as f64 / playback.duration_ms as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(theme.accent).bg(theme.track))
        .ratio(ratio)
        .label(Span::styled(
            format!(
                " {} / {} ",
                format_duration(elapsed),
                format_duration(playback.duration_ms)
            ),
            Style::default().fg(theme.foreground),
        ));
    frame.render_widget(gauge, rows[1]);
}

/// The shown progress advances between polls while Spotify reports playing.
fn interpolated_progress(playback: &Playback, at: Option<Instant>) -> u64 {
    if !playback.is_playing {
        return playback.progress_ms;
    }
    let elapsed = at.map(|at| at.elapsed().as_millis() as u64).unwrap_or(0);
    (playback.progress_ms + elapsed).min(playback.duration_ms)
}

fn draw_footer(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    if let Some((message, at)) = app.status.as_ref() {
        if at.elapsed() < crate::app::STATUS_TTL {
            frame.render_widget(
                Paragraph::new(Span::styled(message.clone(), Style::default().fg(theme.warning))),
                area,
            );
            return;
        }
    }
    let hints: Vec<Span> = [
        ("q", "quit"),
        ("tab", "view"),
        ("enter", "play"),
        ("space", "pause"),
        ("n/p", "skip"),
        ("s", "shuffle"),
        ("r", "repeat"),
        ("+/-", "volume"),
        ("a", "queue"),
        ("d", "device"),
    ]
    .iter()
    .flat_map(|(key, action)| {
        [
            Span::styled(format!(" {key} "), Style::default().fg(theme.accent)),
            Span::styled(action.to_string(), theme.muted_text()),
        ]
    })
    .collect();
    frame.render_widget(Paragraph::new(Line::from(hints)), area);
}

fn track_row(theme: &Theme, index: usize, track: &crate::models::Track) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::styled(format!("{:>3}. ", index + 1), theme.muted_text()),
        Span::styled(track.name.clone(), theme.text().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {}", track.artists), theme.muted_text()),
        Span::styled(
            format!("  {}", track.duration_label()),
            Style::default().fg(theme.thumb),
        ),
    ]))
}

/// A rounded panel with a focus ring: the OpenTUI `View` look.
fn block<'a>(theme: &Theme, title: &str, focused: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(theme.border(focused))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(if focused { theme.focus } else { theme.foreground })
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(theme.surface))
}

fn list_state(selected: usize) -> ListState {
    ListState::default().with_selected(Some(selected))
}
