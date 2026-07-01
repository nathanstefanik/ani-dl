# ani-dl

`ani-dl` is a single, self-contained **Rust** binary for downloading anime. It
keeps the ani-cli search experience — the same AllAnime GraphQL backend — but
has **zero runtime dependencies**: no `fzf`, no `yt-dlp`, no `ffmpeg`, no Python.
Search and episode selection run in a built-in [ratatui](https://ratatui.rs) TUI,
streams are resolved with pure-Rust AES decryption, and downloads use an async
parallel HLS segment fetcher (with AES-128 support) or a direct progressive
downloader — no external process is ever spawned to play or mux video.

## Zero runtime dependencies

Everything is compiled in. Once you have the binary you need **nothing else
installed** — no media player, no downloader, no shell tools. The only build-time
requirement is a Rust toolchain.

## Install

```sh
# from a clone of this repo
cargo install --path .

# or build a release binary directly
cargo build --release      # -> target/release/ani-dl
```

Pre-built binaries: download the release for your platform from the Releases page,
`chmod +x ani-dl`, and drop it on your `PATH` (e.g. `~/.local/bin`). The stripped
release binary is ~4 MB.

## Usage

```
ani-dl [OPTIONS] [QUERY]

Commands:
  sync                 Run provider sync + health check (add --daemon to detach)
  config               Print the config path and contents

Options:
  -d, --download-dir <PATH>   Save directory (default: current directory)
  -q, --quality <QUALITY>     best | worst | 1080 | 720 | 480 [default: best]
  -e, --episodes <RANGE>      "1", "1-12", "1 2 5" (bypasses the TUI)
  -D, --dubbed                Use the dubbed version
  -s, --season <N>            Season number for the SxxExx filename tag [default: 1]
  -n, --number <N>            Auto-select the nth search result (skips the TUI)
  -c, --concurrency <N>       Parallel HLS segment downloads [default: 16]
      --list-providers        Print resolved provider URLs and exit (debug)
      --no-tui                Plain-text output for scripting (needs -n and -e)
  -v, --version               Print version
```

### Examples

```sh
# Interactive: TUI search, then multi-select episodes
ani-dl

# Episodes 1-10 into ~/anime, non-interactive
ani-dl "attack on titan" -e 1-10 -d ~/anime -n 4

# Dubbed
ani-dl -D "one piece" -n 1 -e 1

# Season/episode naming: writes Tongari.Boushi.no.Atelier.S01E13.mp4
ani-dl "Tongari Boushi no Atelier" -n 1 -s 1 -e 13

# Inspect what resolves, without downloading
ani-dl "attack on titan" -n 4 -e 1 --list-providers
```

Files are saved as `{Dotted.Title}.S{NN}E{NN}.mp4` in the download directory.

TUI keys: type to filter search results, ↑/↓ to move, Enter to select, Esc to
quit. In episode selection: `j`/`k` or arrows to move, `Space` to toggle, `a` to
select/deselect all, `Enter` to confirm.

## `ani-dl sync` — the daily maintenance daemon

AllAnime periodically rotates the API. `sync` keeps ani-dl healthy:

- **Source diff** — fetches the live ani-cli script, SHA-256s it against the last
  seen hash, and if it changed, extracts the current AES key seed. If the key
  rotated it updates `~/.config/ani-dl/config.toml` and logs a warning to
  `~/.config/ani-dl/sync.log`.
- **Provider health** — resolves stream URLs for a known test episode across every
  provider, recording success, latency, and stream format to
  `~/.config/ani-dl/provider_health.json`.

Run it manually anytime (`ani-dl sync`), or run it detached to check every 24h:

```sh
ani-dl sync --daemon    # forks a detached process, writes ~/.config/ani-dl/sync.pid
```

The daemon shuts down cleanly on SIGINT/SIGTERM. On macOS, wrapping it in a
`launchd` agent is the recommended long-term setup.

## Performance notes

- **Parallel HLS** — segments download concurrently via
  `buffer_unordered(concurrency)` (default 16), written to a temp dir and
  concatenated in order. Encrypted (`#EXT-X-KEY`, AES-128-CBC) streams are
  detected and decrypted in-process.
- **Concurrent provider probing** — all providers are resolved in parallel
  (`join_all`) so quality selection sees every candidate.
- **Startup** — a compiled binary with no interpreter or subprocess launch; first
  search is network-bound, not startup-bound.

## AllAnime key rotation

The `sync` daemon handles key rotation automatically and updates your config. If
streams break before the next sync, run `ani-dl sync` manually. If the seed itself
changed, `sync` logs the new seed — update `KEY_SEED` in
[`src/constants.rs`](src/constants.rs) and rebuild. Reference:
<https://raw.githubusercontent.com/pystardust/ani-cli/master/ani-cli>

This is an unofficial tool for personal use; respect the rights of content owners.
