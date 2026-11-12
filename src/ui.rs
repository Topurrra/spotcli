//! Ratatui rendering for the whole interface.

use std::time::Instant;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Tabs};
use ratatui::Frame;

use crate::app::{App, Tab, LIKED_KEY};
use crate::models::{format_duration, Playback};

const ACCENT: Color = Color::Green;
const DIM: Color = Color::DarkGray;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(frame, app, chunks[0]);
    match app.tab {
        Tab::Library => draw_library(frame, app, chunks[1]),
        Tab::Search => draw_search(frame, app, chunks[1]),
    }
    draw_player(frame, app, chunks[2]);
    draw_footer(frame, app, chunks[3]);
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let tabs = Tabs::new(vec!["Library", "Search"])
        .block(Block::default().borders(Borders::ALL).title(" spotcli "))
        .style(Style::default().fg(Color::Gray))
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
        .select(match app.tab {
            Tab::Library => 0,
            Tab::Search => 1,
        });
    frame.render_widget(tabs, area);
}

fn draw_library(frame: &mut Frame, app: &App, area: Rect) {
    let columns = Layout::horizontal([Constraint::Percentage(30), Constraint::Fill(1)]).split(area);

    let mut playlist_items: Vec<ListItem> = Vec::with_capacity(app.playlists.len() + 1);
    playlist_items.push(ListItem::new(Line::from(Span::styled(
        "Liked Songs",
        Style::default().fg(ACCENT),
    ))));
    for playlist in &app.playlists {
        playlist_items.push(ListItem::new(Line::from(vec![
            Span::raw(playlist.name.clone()),
            Span::styled(
                format!("  {} ({})", playlist.owner, playlist.track_count),
                Style::default().fg(DIM),
            ),
        ])));
    }
    let playlist_list = List::new(playlist_items)
        .block(list_block(" Playlists ", app.tab == Tab::Library && !app.tracks_focused))
        .highlight_style(highlight());
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
        .map(|(index, track)| track_row(index, track))
        .collect();
    let track_list = List::new(track_items)
        .block(list_block(&title, app.tab == Tab::Library && app.tracks_focused))
        .highlight_style(highlight());
    frame.render_stateful_widget(track_list, columns[1], &mut list_state(app.tracks_cursor));
}

fn draw_search(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).split(area);

    let input = Paragraph::new(format!("{}_", app.search_input))
        .block(list_block(" Search: type, Enter to search ", true));
    frame.render_widget(input, rows[0]);

    let items: Vec<ListItem> = app
        .search_results
        .iter()
        .enumerate()
        .map(|(index, track)| track_row(index, track))
        .collect();
    let results = List::new(items)
        .block(list_block(" Results ", false))
        .highlight_style(highlight());
    frame.render_stateful_widget(results, rows[1], &mut list_state(app.search_cursor));
}

fn draw_player(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Now Playing ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);

    let Some(playback) = app.playback.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Nothing playing. Press Enter on a track to start.",
                Style::default().fg(DIM),
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
        Span::styled(format!("{state_symbol} "), Style::default().fg(ACCENT)),
        Span::styled(title.to_string(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!(" — {artists}"), Style::default().fg(Color::Gray)),
    ]);
    let right = Line::from(vec![Span::styled(
        format!("{shuffle_symbol} {repeat_symbol}{volume}{device}"),
        Style::default().fg(DIM),
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
        .gauge_style(Style::default().fg(ACCENT))
        .ratio(ratio)
        .label(Span::styled(
            format!(
                " {} / {} ",
                format_duration(elapsed),
                format_duration(playback.duration_ms)
            ),
            Style::default().fg(Color::White),
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

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    if let Some((message, at)) = app.status.as_ref() {
        if at.elapsed() < crate::app::STATUS_TTL {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    message.clone(),
                    Style::default().fg(Color::Yellow),
                )),
                area,
            );
            return;
        }
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "q quit · tab view · enter play · space pause · n/p skip · s shuffle · r repeat · +/- volume · a queue · d device",
            Style::default().fg(DIM),
        )),
        area,
    );
}

fn track_row(index: usize, track: &crate::models::Track) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::styled(format!("{:>3}. ", index + 1), Style::default().fg(DIM)),
        Span::styled(
            track.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", track.artists), Style::default().fg(Color::Gray)),
        Span::styled(
            format!("  {}", track.duration_label()),
            Style::default().fg(DIM),
        ),
    ]))
}

fn list_block(title: &str, focused: bool) -> Block<'_> {
    let border = if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(if focused { ACCENT } else { Color::Gray }),
        ))
}

fn highlight() -> Style {
    Style::default()
        .bg(Color::Rgb(30, 30, 30))
        .fg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

fn list_state(selected: usize) -> ListState {
    ListState::default().with_selected(Some(selected))
}
