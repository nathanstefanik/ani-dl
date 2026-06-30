"""Thin fzf wrapper for interactive selection."""

from __future__ import annotations

import shutil
import subprocess
import sys


def has_fzf() -> bool:
    return shutil.which("fzf") is not None


def require_fzf() -> None:
    if not has_fzf():
        print(
            "Error: 'fzf' is required for interactive selection but was not "
            "found in PATH.\n"
            "Install it (e.g. 'brew install fzf' / 'apt install fzf') or use "
            "non-interactive flags (-n / -e).",
            file=sys.stderr,
        )
        raise SystemExit(1)


def fzf_select(lines: list[str], prompt: str, multi: bool = False) -> list[str]:
    """Pipe ``lines`` to fzf and return the selected line(s).

    Returns an empty list if the user aborted (Esc / Ctrl-C).
    """
    require_fzf()
    cmd = ["fzf", "--prompt", prompt, "--height", "40%", "--reverse"]
    if multi:
        cmd.append("--multi")
    proc = subprocess.run(
        cmd,
        input="\n".join(lines),
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        return []
    return [l for l in proc.stdout.splitlines() if l.strip()]
