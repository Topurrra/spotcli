//! spotcli: Spotify in your terminal.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use spotcli::{config_dir, App, Client, TokenStore};

#[derive(Parser)]
#[command(
    name = "spotcli",
    version,
    about = "Spotify in your terminal: a keyboard-driven TUI client"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Which palette to draw with: auto (ask the terminal), dark, light,
    /// or terminal (inherit the terminal's own colors).
    #[arg(long, default_value = "auto")]
    theme: String,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in through the browser and store credentials.
    Login {
        /// Spotify app client ID (falls back to SPOTIFY_CLIENT_ID or the
        /// stored one). Register an app at the Spotify developer dashboard
        /// with redirect URI http://127.0.0.1:8989/callback
        #[arg(long)]
        client_id: Option<String>,
    },
    /// Forget stored credentials.
    Logout,
    /// Show the signed-in account.
    Me,
    /// Show the current playback state.
    Status,
    /// Play a search query, or resume playback with no argument.
    Play {
        /// What to search for and play; omit to resume.
        query: Option<String>,
    },
    /// Pause playback.
    Pause,
    /// Skip to the next track.
    Next,
    /// Go back to the previous track.
    Prev,
    /// List Spotify Connect devices.
    Devices,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(dispatch(cli, &runtime));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(cli: Cli, runtime: &tokio::runtime::Runtime) -> Result<()> {
    let theme = pick_theme(&cli.theme);
    match cli.command {
        Some(Command::Login { client_id }) => login(client_id).await,
        Some(Command::Logout) => {
            TokenStore::clear(&config_dir()?);
            println!("Credentials removed.");
            Ok(())
        }
        Some(command) => one_shot(command).await,
        None => run_tui(runtime, theme).await,
    }
}

/// Resolve `--theme`: `auto` asks the terminal for its background color and
/// picks dark or light from its brightness; anything unrecognized falls
/// back to dark.
fn pick_theme(name: &str) -> spotcli::Theme {
    match name.trim().to_ascii_lowercase().as_str() {
        "light" => spotcli::Theme::light(),
        "terminal" => spotcli::Theme::terminal(),
        "dark" => spotcli::Theme::dark(),
        _ => match spotcli::theme_detect::detect_background() {
            Some(background) => spotcli::Theme::for_background(background),
            None => spotcli::Theme::dark(),
        },
    }
}

async fn login(client_id: Option<String>) -> Result<()> {
    let dir = config_dir()?;
    let client_id = match client_id
        .or_else(|| std::env::var("SPOTIFY_CLIENT_ID").ok())
        .or_else(|| {
            spotcli::TokenStore::read(&dir)
                .ok()
                .flatten()
                .map(|credentials| credentials.client_id)
        }) {
        Some(id) if !id.trim().is_empty() => id.trim().to_string(),
        _ => anyhow::bail!(
            "a Spotify client ID is required.\n\
             1. Create an app at https://developer.spotify.com/dashboard\n\
             2. Add the redirect URI http://127.0.0.1:8989/callback\n\
             3. Run: spotcli login --client-id <your client id>"
        ),
    };
    let http = reqwest::Client::new();
    spotcli::login(&http, &client_id, &dir).await?;
    println!(
        "Signed in. Credentials saved to {}",
        dir.join("credentials.json").display()
    );
    Ok(())
}

async fn one_shot(command: Command) -> Result<()> {
    let client = Client::from_dir(&config_dir()?)?;
    match command {
        Command::Me => {
            let me = client.me().await?;
            println!("{} (@{}) — plan: {}", me.display_name, me.id, me.product);
        }
        Command::Status => match client.playback().await? {
            Some(playback) => match playback.track.as_ref() {
                Some(track) => println!(
                    "{}{} — {} [{}] on {}{}",
                    if playback.is_playing { "▶ " } else { "⏸ " },
                    track.name,
                    track.artists,
                    playback.progress_label(),
                    playback.device_name,
                    playback
                        .volume_percent
                        .map(|v| format!(" ({v}%)"))
                        .unwrap_or_default()
                ),
                None => println!("Nothing playing."),
            },
            None => println!("Nothing playing."),
        },
        Command::Play { query } => match query {
            Some(query) => {
                let tracks = client.search_tracks(&query, 1).await?;
                let Some(track) = tracks.first() else {
                    anyhow::bail!("no track matches {query:?}");
                };
                client.play(None, std::slice::from_ref(&track.uri)).await?;
                println!("Playing {} — {}", track.name, track.artists);
            }
            None => client.play(None, &[]).await?,
        },
        Command::Pause => client.pause(None).await?,
        Command::Next => client.next(None).await?,
        Command::Prev => client.previous(None).await?,
        Command::Devices => {
            for device in client.devices().await? {
                println!(
                    "{}{} ({})",
                    if device.is_active { "* " } else { "  " },
                    device.name,
                    device.kind
                );
            }
        }
        _ => unreachable!("login and logout are handled in dispatch"),
    }
    Ok(())
}

async fn run_tui(runtime: &tokio::runtime::Runtime, theme: spotcli::Theme) -> Result<()> {
    let client = Client::from_dir(&config_dir()?)?;
    let mut terminal = ratatui::init();
    let result = App::with_theme(theme).run(&mut terminal, client, runtime.handle().clone());
    ratatui::restore();
    result
}
