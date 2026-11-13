# spotcli

Spotify in your terminal: a fast, keyboard-driven TUI built with
[ratatui](https://ratatui.rs). Browse your library, search, see album art and
lyrics, and either stream right in the terminal or control any Spotify device.

## Features

- **Library**: your playlists and Liked Songs, play anything with `Enter`
- **Search as you type**: tracks, artists, albums and playlists; queue, like,
  or add a track to one of your playlists
- **Built-in streaming**: spotcli can be the speaker itself (Premium,
  via [librespot](https://github.com/librespot-org/librespot)), or drive your
  phone, desktop app or any Connect device
- **Now playing**: album art, progress, shuffle and repeat, and an accent color
  taken from the cover
- **View tab**: synced lyrics and the artist's top tracks
- **Themes**: `dark`, `light`, `catppuccin`, `gruvbox`, `terminal`
- **Mouse support** and one-shot commands for scripts

## Install

Download the archive for your system from
[Releases](https://github.com/Topurrra/spotcli/releases/latest):

- **Windows**: unzip and run `install.ps1` (right-click → Run with PowerShell),
  then open a new terminal
- **macOS / Linux**: extract and move `spotcli` somewhere on your `PATH`
  (Linux needs ALSA: `libasound2`)

Or build from source:

```sh
cargo install --git https://github.com/Topurrra/spotcli
```

## Sign in

1. Create an app in the [Spotify developer dashboard](https://developer.spotify.com/dashboard).
2. Add the redirect URI `http://127.0.0.1:8989/callback` and copy the **Client ID**.
3. Run:

```sh
spotcli login --client-id YOUR_CLIENT_ID
```

Your browser opens twice: once for your account, once to enable built-in
streaming. Credentials live in `~/.config/spotcli/`. After upgrading spotcli,
run `spotcli login` again if a feature reports a missing permission.

## Keys

| Key | Action |
| --- | --- |
| `Tab` | switch Library, Search, View |
| `↑↓` / `j k` | move |
| `←→` / `h l` | playlists / tracks (Library) |
| `Enter` | play or open |
| `space` | play / pause |
| `n` / `p` | next / previous track (Library, View) |
| `,` / `.` | seek 5 s |
| `+` / `-` | volume |
| `s` / `r` | shuffle / repeat |
| `a` | add to queue |
| `t` | stream here or control another device |
| `d` | next Spotify device |
| `Ctrl+T` | next theme (remembered) |
| `q` / `Ctrl+C` | quit |

**Search**: type to search. `↓` or `Enter` moves into the results, where
`Enter` plays, `a` queues, `l` likes, `p` adds to a playlist, and `←→`
switches between tracks, artists, albums and playlists. `/` or `Esc` goes
back to typing.

## Themes

```sh
spotcli --theme gruvbox
```

The default, `auto`, picks dark or light from your terminal's background.
`Ctrl+T` cycles themes inside the app and remembers your choice.

## Commands

```sh
spotcli status | play "query" | pause | next | prev | devices | me | logout
```

## Development

```sh
git config core.hooksPath .githooks   # run checks before every push
scripts/check.sh                      # tests + clippy
scripts/release.sh 1.1.0              # bump, build, tag, publish
```

## License

MIT
