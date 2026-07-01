# ani-dl

`ani-dl` is a minimal Python CLI for **downloading** anime. It borrows the
search-and-select experience of [ani-cli](https://github.com/pystardust/ani-cli)
— the same AllAnime GraphQL backend, the same fzf-driven picker — but strips out
all playback. No video player is ever launched. It resolves a direct stream URL
for the episodes you pick and saves them to disk with `yt-dlp` (falling back to
`ffmpeg` for stubborn HLS streams).

## Dependencies

- **Python 3.10+**
- **fzf** — for interactive search/episode selection (system package)
- **ffmpeg** — fallback downloader for bare `m3u8`/HLS streams (system package)
- **yt-dlp** — primary downloader (installed automatically as a Python dependency)

`requests` and `cryptography` are also installed automatically. Install the
system tools with your package manager, e.g.:

```sh
brew install fzf ffmpeg        # macOS
sudo apt install fzf ffmpeg    # Debian/Ubuntu
```

## Installation

```sh
# from a clone of this repo
pip install .

# or, isolated, with pipx (recommended)
pipx install .

# editable/dev install
pip install -e .
```

## Usage

```
ani-dl [OPTIONS] [QUERY]

  -d, --download-dir PATH   Directory to save files (default: current directory)
  -q, --quality QUALITY     best, worst, or e.g. 1080 / 720 / 480 (default: best)
  -e, --episodes RANGE      "1", "1-12", "1 2 5" (skips fzf selection)
  -D, --dubbed              Use the dubbed version (default: subbed)
  -n, --number INT          Auto-select the nth search result (skips fzf)
      --list-providers      Print resolved providers/streams for an episode (debug)
  -v, --version             Show version
```

### Examples

```sh
# Fully interactive: search, pick a show in fzf, multi-select episodes
ani-dl

# Download episodes 1-10 of a show into ~/anime
ani-dl "attack on titan" -e 1-10 -d ~/anime

# Dubbed
ani-dl -D "one piece"

# Inspect what streams resolve for episode 1 without downloading
ani-dl "attack on titan" -n 1 -e 1 --list-providers
```

Episodes are saved as `{Dotted.Title}.S{NN}E{NN}.mp4` in the download
directory (the current directory unless `-d` is given).

## Notes on AllAnime key rotation

AllAnime periodically rotates the API behaviour ani-dl depends on. Two things
can break a download:

1. **The AES decryption key.** The encrypted source list (`tobeparsed`) is
   decrypted with a key derived from a seed string (`Xot36i3lK3:v1`) in
   [`src/ani_dl/__init__.py`](src/ani_dl/__init__.py). Upstream ani-cli derives
   the same value; if streams stop resolving, compare against the live source
   and update `_KEY_SEED` / endpoints:
   <https://raw.githubusercontent.com/pystardust/ani-cli/master/ani-cli>
   (grep for `allanime_key`, `allanime_refr`, and the persisted-query hash).
2. **Provider availability.** Providers come and go. The mp4upload and
   fast4speed/Filemoon paths are the most volatile; if a particular show won't
   download, try `--list-providers` to see which sources resolved, and another
   quality/source usually works.

This is an unofficial tool for personal use; respect the rights of content
owners.
