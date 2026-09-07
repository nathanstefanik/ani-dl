# ani-dl

`ani-dl` is a single, self-contained **Rust** binary for downloading anime. It
keeps the ani-cli search experience — now on the same **anidb.app** backend as
ani-cli v5 — but has **zero runtime dependencies**: no `fzf`, no `yt-dlp`, no
`ffmpeg`, no `curl-impersonate`. Search and episode selection run in a built-in
[ratatui](https://ratatui.rs) TUI, Cloudflare is cleared with compiled-in Chrome
TLS fingerprinting ([wreq](https://crates.io/crates/wreq)), and downloads use an
async parallel HLS segment fetcher (with AES-128 support) — no external process
is ever spawned to play or mux video.

## Zero runtime dependencies

Everything is compiled in. Once you have the binary you need **nothing else
installed** — no media player, no downloader, no shell tools. The only build-time
requirements are a Rust toolchain and `cmake` (for BoringSSL, used by wreq).

## Install

```sh
# from a clone of this repo
cargo install --path .

# or build a release binary directly
cargo build --release      # -> target/release/ani-dl
```

Pre-built binaries: download the release for your platform from the Releases page,
`chmod +x ani-dl`, and drop it on your `PATH` (e.g. `~/.local/bin`).

## Usage

```
ani-dl [OPTIONS] [QUERY]

Commands:
  sync                 Run provider health check (add --daemon to detach)
  config               Print the config path and contents

Options:
  -d, --download-dir <PATH>   Save directory (default: current directory)
  -q, --quality <QUALITY>     best | worst | 1080 | 720 | 480 [default: best]
  -e, --episodes <RANGE>      "1", "1-12", "1 2 5"; 0 = first, -1 = last
  -D, --dubbed                Use the dubbed version
  -s, --season <N>            Season number for the SxxExx filename tag [default: 1]
  -n, --number <N>            Auto-select the nth search result (skips the TUI)
  -c, --concurrency <N>       Parallel HLS segment downloads [default: 16]
      --list-providers        Print resolved stream URLs and exit (debug)
      --no-tui                Plain-text output for scripting (needs -n and -e)
  -V, --version               Print version (long form shows build info)
```

### Examples

```sh
# Interactive: TUI search, then multi-select episodes
ani-dl

# Episodes 1-10 into ~/anime, non-interactive
ani-dl "attack on titan" -e 1-10 -d ~/anime -n 1

# Dubbed
ani-dl -D "one piece" -n 1 -e 1

# Whole show (0 = first available episode, -1 = last); "-1" alone is the latest
ani-dl "frieren" -n 1 -e 0--1
ani-dl "frieren" -n 1 -e -1

# Season/episode naming: writes Tongari.Boushi.no.Atelier.S01E13.mp4
ani-dl "Tongari Boushi no Atelier" -n 1 -s 1 -e 13

# Inspect what resolves, without downloading
ani-dl "cyberpunk edgerunners" -n 1 -e 1 --list-providers
```

Files are saved as `{Dotted.Title}.S{NN}E{NN}.mp4` in the download directory.

TUI keys: type to filter search results, ↑/↓ to move, Enter to select, Esc to
quit. In episode selection: `j`/`k` or arrows to move, `Space` to toggle, `a` to
select/deselect all, `Enter` to confirm.

## Versioning

ani-dl's own semver is independent of the ani-cli release whose scraping it
mirrors. `-V` prints the number and commit; `--version` also shows the target
triple, **ani-cli parity**, and provider. `ani-dl sync` stamps both version
numbers into `provider_health.json`.

```
ani-dl 1.0.0
commit:         a1b2c3d45
target:         aarch64-apple-darwin
ani-cli parity: 5.0.4
provider:       https://anidb.app
```

## How it works

```
query → browse scrape → pick show → episode JSON → pick episodes
  └─ for each episode:
       ├─ languages API → embed page → master.m3u8
       ├─ expand quality variants
       ├─ select_quality — pick best/worst/1080/720/480
       └─ HlsDownloader — parallel HLS segments (AES-128 in-process)
          → {Title}.S{NN}E{NN}.mp4
```

Provider: [anidb.app](https://anidb.app), matching ani-cli v5 — the scraping
paths track **ani-cli 5.0.4**. Sub uses the `jpn` language embed; `--dubbed`
uses `eng`.

## `ani-dl sync` — health check

`sync` searches a configured test title, resolves one episode, and writes
`~/.config/ani-dl/provider_health.json`. Run it manually anytime, or detach:

```sh
ani-dl sync --daemon    # forks a detached process, writes ~/.config/ani-dl/sync.pid
```

## Performance notes

- **Parallel HLS** — segments download concurrently via
  `buffer_unordered(concurrency)` (default 16). Encrypted (`#EXT-X-KEY`,
  AES-128-CBC) streams are decrypted in-process.
- **Startup** — a compiled binary with no interpreter or subprocess launch; first
  search is network-bound, not startup-bound.

This is an unofficial tool for personal use; respect the rights of content owners.
