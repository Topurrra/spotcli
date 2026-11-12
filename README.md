# spotcli

Spotify in your terminal. A keyboard-driven TUI client for the Spotify Web
API, built with [ratatui](https://ratatui.rs).

## Features

- **Library view** - browse your playlists and liked songs, open a playlist,
  play any track.
- **Search** - search Spotify for tracks and play or queue them instantly.
- **Now playing bar** - live progress, play/pause, skip, shuffle, repeat,
  volume, and the device it is playing on.
- **Spotify Connect** - list your devices and move playback between them.
- **One-shot commands** - `status`, `play <query>`, `pause`, `next`, `prev`
  for scripts and keybindings outside the TUI.

Playback happens through Spotify itself: spotcli controls your active Spotify
device (desktop app, phone, browser player, a Connect speaker, ...). It does
not stream audio itself, so it works with Free and Premium accounts alike.

## Setup

spotcli talks to the Spotify Web API with your own Spotify app credentials:

1. Create an app at the [Spotify developer dashboard](https://developer.spotify.com/dashboard).
2. In the app settings, add this redirect URI exactly:
   `http://127.0.0.1:8989/callback`
3. Copy the **Client ID** (no client secret needed; spotcli uses PKCE).

Then:

```
spotcli login --client-id YOUR_CLIENT_ID
```

Your browser opens, you approve the access, and credentials are stored in
`~/.config/spotcli/credentials.json` with owner-only permissions. Tokens are
refreshed automatically; you only sign in again if Spotify revokes the
session. `SPOTIFY_CLIENT_ID` works too if you prefer the environment.

## Build

```
cargo build --release
```

The binary lands in `target/release/spotcli`.

## Usage

Run the TUI:

```
spotcli
```

Keys:

| Key | Action |
| --- | --- |
| `q` / `Ctrl-C` | quit |
| `Tab` | switch Library / Search |
| `j` / `k` or arrows | move the selection |
| `h` / `l` or left / right | focus playlist list / track list |
| `Enter` | play the selection (opens a playlist, plays a track) |
| `space` | play / pause |
| `n` / `p` | next / previous track |
| `s` | toggle shuffle |
| `r` | cycle repeat (off, context, track) |
| `+` / `-` | volume up / down |
| `a` | add the selected track to the queue |
| `d` | move playback to the next device |
| `Esc` | clear the search input |

In the Search tab, typing goes into the search box; `Enter` runs the search.

For scripts and window-manager keybindings:

```
spotcli status
spotcli play "daft punk around the world"
spotcli pause
spotcli next
spotcli prev
spotcli devices
spotcli me
spotcli logout
```

## Architecture

A short tour for contributors:

- `src/auth.rs` - Authorization Code + PKCE flow: browser login, a loopback
  listener on `127.0.0.1:8989`, and a token store that refreshes before
  expiry and after a 401.
- `src/api.rs` - the Web API client: one transport that carries the bearer
  token, retries once after a refresh on 401, and reports rate limits with
  their `Retry-After`.
- `src/models.rs` - flat view models mapped from Spotify's sparse JSON.
- `src/app.rs` - the event loop and state. Every network call runs on a
  tokio worker; the UI thread only drains an event channel and never
  blocks. Controls update optimistically, then the backend confirms.
- `src/ui.rs` - ratatui rendering.
- `src/main.rs` - clap entry point: the TUI plus one-shot commands.

## License

MIT. See [LICENSE](LICENSE).
