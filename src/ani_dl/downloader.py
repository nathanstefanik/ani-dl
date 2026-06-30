"""Download logic: yt-dlp primary, ffmpeg fallback. No playback, ever."""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path


def _have(tool: str) -> bool:
    return shutil.which(tool) is not None


def _ytdlp_format(quality: str) -> list[str]:
    """Build yt-dlp selection args for a requested quality."""
    if quality in ("best", "worst"):
        return ["-f", quality]
    if quality.isdigit():
        # Prefer the variant at or just below the requested height.
        return ["-S", f"res:{quality}"]
    return []


def download(
    url: str,
    referer: str,
    out_dir: Path,
    filename: str,
    quality: str = "best",
) -> bool:
    """Download ``url`` to ``out_dir/filename.mp4``. Returns True on success."""
    out_dir.mkdir(parents=True, exist_ok=True)
    final_mp4 = out_dir / f"{filename}.mp4"

    if _have("yt-dlp"):
        template = str(out_dir / f"{filename}.%(ext)s")
        cmd = [
            "yt-dlp",
            "--referer", referer,
            "--no-skip-unavailable-fragments",
            "--fragment-retries", "infinite",
            "-N", "16",
            "--remux-video", "mp4",
            "-o", template,
            *_ytdlp_format(quality),
            url,
        ]
        print(f"  -> yt-dlp {filename}", file=sys.stderr)
        if subprocess.run(cmd).returncode == 0 and _produced(out_dir, filename):
            return True
        print("  ! yt-dlp failed, falling back to ffmpeg", file=sys.stderr)
    else:
        print("  ! yt-dlp not found, trying ffmpeg", file=sys.stderr)

    if _have("ffmpeg"):
        return _ffmpeg(url, referer, final_mp4)

    print(
        "  ! Neither yt-dlp nor ffmpeg is available; cannot download.",
        file=sys.stderr,
    )
    return False


def _ffmpeg(url: str, referer: str, final_mp4: Path) -> bool:
    base = [
        "ffmpeg", "-y",
        "-referer", referer,
        "-loglevel", "error", "-stats",
        "-i", url,
        "-c", "copy",
        str(final_mp4),
    ]
    # -extension_picky is only on newer ffmpeg builds; try with it, then without.
    print(f"  -> ffmpeg {final_mp4.name}", file=sys.stderr)
    with_picky = ["ffmpeg", "-extension_picky", "0"] + base[1:]
    if subprocess.run(with_picky).returncode == 0 and final_mp4.exists():
        return True
    if subprocess.run(base).returncode == 0 and final_mp4.exists():
        return True
    return False


def _produced(out_dir: Path, filename: str) -> bool:
    return any(out_dir.glob(f"{filename}.*"))
