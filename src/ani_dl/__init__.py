"""ani-dl: a minimal anime downloader with the ani-cli search UX, no playback.

All shared constants live here so that both ``api`` and ``providers`` can import
them without creating an import cycle.
"""

import hashlib

__version__ = "0.1.0"

# --- AllAnime endpoints / headers (mirrors the live ani-cli source) -----------
# NOTE: these track upstream ani-cli. If streams break, re-check the values in
# https://raw.githubusercontent.com/pystardust/ani-cli/master/ani-cli
ALLANIME_BASE = "allanime.day"
API_BASE = f"https://api.{ALLANIME_BASE}"
REFERER = "https://youtu-chan.com"
AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:150.0) "
    "Gecko/20100101 Firefox/150.0"
)

# AES-256 key used to decrypt the encrypted source list ("tobeparsed").
# Upstream derives it as sha256 of a rotating seed string rather than hardcoding
# the hex. If decryption starts failing, the seed below is the thing to update.
_KEY_SEED = "Xot36i3lK3:v1"
ALLANIME_KEY = hashlib.sha256(_KEY_SEED.encode()).hexdigest()

# Persisted-query hash for the episode-embed GraphQL request.
EPISODE_QUERY_HASH = (
    "d405d0edd690624b66baba3068e0edc3ac90f1597d898a1ec8db4e5c43c00fec"
)
