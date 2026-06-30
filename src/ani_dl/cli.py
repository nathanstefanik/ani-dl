"""Command-line interface and main download loop for ani-dl."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

from . import __version__
from .api import AllAnimeClient, Show
from .downloader import download
from .providers import Stream, resolve
from .ui import fzf_select


# --- helpers ------------------------------------------------------------------

def dotted_title(name: str) -> str:
    """Turn a show name into a dot-separated, filename-safe title.

    "Tongari Boushi no Atelier" -> "Tongari.Boushi.no.Atelier"
    """
    cleaned = re.sub(r"[^\w\s-]", "", name).strip()   # drop unsafe punctuation
    cleaned = re.sub(r"[\s_]+", ".", cleaned).strip(".")
    return cleaned or "anime"


def season_episode_tag(season: int, episode: str) -> str:
    """Build the SxxExx tag, zero-padded to at least two digits each.

    Episode strings are usually integers ("13" -> "E13") but AllAnime
    occasionally uses fractional specials ("10.5" -> "E10.5").
    """
    s = f"S{int(season):02d}"
    try:
        ep_f = float(episode)
        e = f"E{int(ep_f):02d}" if ep_f.is_integer() else f"E{episode}"
    except (TypeError, ValueError):
        e = f"E{episode}"
    return s + e


def build_filename(name: str, season: int, episode: str) -> str:
    """e.g. "Tongari.Boushi.no.Atelier.S01E13" (extension added by downloader)."""
    return f"{dotted_title(name)}.{season_episode_tag(season, episode)}"


def parse_episode_arg(arg: str, available: list[str]) -> list[str]:
    """Expand "1", "1-12", "1 2 5", "1,3,5" against the available episode list."""
    avail = set(available)
    picked: list[str] = []
    for token in re.split(r"[,\s]+", arg.strip()):
        if not token:
            continue
        m = re.fullmatch(r"(\d+)-(\d+)", token)
        if m:
            lo, hi = int(m.group(1)), int(m.group(2))
            for n in range(lo, hi + 1):
                picked.append(str(n))
        else:
            picked.append(token)
    # Keep only episodes that actually exist, preserving requested order.
    result = []
    for ep in picked:
        if ep in avail and ep not in result:
            result.append(ep)
        elif ep not in avail:
            print(f"  ! episode {ep} not available, skipping", file=sys.stderr)
    return result


def select_stream(streams: list[Stream], quality: str) -> Stream | None:
    if not streams:
        return None
    ordered = sorted(streams, key=lambda s: s.height, reverse=True)
    if quality == "best":
        return ordered[0]
    if quality == "worst":
        with_h = [s for s in ordered if s.height > 0]
        return with_h[-1] if with_h else ordered[-1]
    if quality.isdigit():
        want = int(quality)
        exact = [s for s in ordered if s.height == want]
        if exact:
            return exact[0]
        at_or_below = [s for s in ordered if s.height and s.height <= want]
        if at_or_below:
            return at_or_below[0]
        print(
            f"  ! quality {quality} not found, using best", file=sys.stderr
        )
    return ordered[0]


def gather_streams(client: AllAnimeClient, show_id, mode, ep) -> list[Stream]:
    pairs = client.source_urls(show_id, mode, ep)
    streams: list[Stream] = []
    for source_name, source_url in pairs:
        try:
            streams.extend(resolve(client.session, source_name, source_url))
        except Exception as e:  # noqa: BLE001
            print(f"  ! provider {source_name} failed: {e}", file=sys.stderr)
    return streams


# --- interactive pickers ------------------------------------------------------

def pick_show(shows: list[Show], number: int | None) -> Show | None:
    if not shows:
        print("No results found.", file=sys.stderr)
        return None
    if number is not None:
        if 1 <= number <= len(shows):
            return shows[number - 1]
        print(f"--number {number} out of range (1-{len(shows)})", file=sys.stderr)
        return None
    lines = [f"{i+1}\t{s.name} ({s.episodes} episodes)" for i, s in enumerate(shows)]
    chosen = fzf_select(lines, "Anime> ", multi=False)
    if not chosen:
        return None
    idx = int(chosen[0].split("\t", 1)[0]) - 1
    return shows[idx]


def pick_episodes(
    available: list[str], episodes_arg: str | None
) -> list[str]:
    if episodes_arg:
        return parse_episode_arg(episodes_arg, available)
    chosen = fzf_select(available, "Episode> ", multi=True)
    # fzf returns in display order; sort numerically for sane download order.
    return sorted(chosen, key=lambda x: float(x) if _isnum(x) else 0.0)


def _isnum(x: str) -> bool:
    try:
        float(x)
        return True
    except (TypeError, ValueError):
        return False


# --- main ---------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="ani-dl",
        description="Download anime via the AllAnime API (no playback).",
    )
    p.add_argument("query", nargs="?", help="search term")
    p.add_argument("-d", "--download-dir", default="./downloads",
                   help="directory to save files (default: ./downloads)")
    p.add_argument("-q", "--quality", default="best",
                   help="best, worst, or e.g. 1080/720/480 (default: best)")
    p.add_argument("-e", "--episodes",
                   help='episode range: "1", "1-12", "1 2 5" (skips fzf)')
    p.add_argument("-D", "--dubbed", action="store_true",
                   help="use dubbed version (default: subbed)")
    p.add_argument("-s", "--season", type=int, default=1,
                   help="season number for the SxxExx filename tag (default: 1)")
    p.add_argument("-n", "--number", type=int,
                   help="auto-select the nth search result (skips fzf)")
    p.add_argument("--list-providers", action="store_true",
                   help="show resolved providers/streams for the episode(s)")
    p.add_argument("-v", "--version", action="version",
                   version=f"ani-dl {__version__}")
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    mode = "dub" if args.dubbed else "sub"

    query = args.query
    if not query:
        try:
            query = input("Search anime: ").strip()
        except (EOFError, KeyboardInterrupt):
            return 1
    if not query:
        print("No query given.", file=sys.stderr)
        return 1

    client = AllAnimeClient()
    print(f"Searching '{query}' ({mode})...", file=sys.stderr)
    shows = client.search(query, mode)
    show = pick_show(shows, args.number)
    if show is None:
        return 1
    print(f"Selected: {show.name} ({show.episodes} eps)", file=sys.stderr)

    available = client.episodes_list(show.id, mode)
    if not available:
        print("No episodes available for this translation type.", file=sys.stderr)
        return 1

    episodes = pick_episodes(available, args.episodes)
    if not episodes:
        print("No episodes selected.", file=sys.stderr)
        return 1

    out_dir = Path(args.download_dir).expanduser()
    failures = 0

    for ep in episodes:
        print(f"\n=== Episode {ep} ===", file=sys.stderr)
        streams = gather_streams(client, show.id, mode, ep)

        if args.list_providers:
            if not streams:
                print("  (no streams resolved)", file=sys.stderr)
            for s in sorted(streams, key=lambda s: s.height, reverse=True):
                h = f"{s.height}p" if s.height else "????"
                print(f"  [{s.provider:>10}] {h:>5}  {s.url}")
            continue

        chosen = select_stream(streams, args.quality)
        if chosen is None:
            print(f"  ! No stream found for episode {ep}", file=sys.stderr)
            failures += 1
            continue

        h = f"{chosen.height}p" if chosen.height else "unknown"
        print(f"  source: {chosen.provider} ({h})", file=sys.stderr)
        filename = build_filename(show.name, args.season, ep)
        if download(chosen.url, chosen.referer, out_dir, filename, args.quality):
            print(f"Downloaded: {out_dir / (filename + '.mp4')}")
        else:
            print(f"  ! Download failed for episode {ep}", file=sys.stderr)
            failures += 1

    return 1 if failures else 0
