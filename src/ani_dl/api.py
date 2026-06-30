"""AllAnime GraphQL client: search, episode listing, and source-URL retrieval."""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass

import requests

from . import AGENT, API_BASE, EPISODE_QUERY_HASH, REFERER
from .providers import parse_source_urls

SEARCH_GQL = (
    "query( $search: SearchInput $limit: Int $page: Int "
    "$translationType: VaildTranslationTypeEnumType "
    "$countryOrigin: VaildCountryOriginEnumType ) { "
    "shows( search: $search limit: $limit page: $page "
    "translationType: $translationType countryOrigin: $countryOrigin ) { "
    "edges { _id name availableEpisodes __typename } } }"
)

EPISODES_LIST_GQL = (
    "query ($showId: String!) { show( _id: $showId ) "
    "{ _id availableEpisodesDetail } }"
)

EPISODE_EMBED_GQL = (
    "query ($showId: String!, $translationType: VaildTranslationTypeEnumType!, "
    "$episodeString: String!) { episode( showId: $showId "
    "translationType: $translationType episodeString: $episodeString ) "
    "{ episodeString sourceUrls } }"
)


@dataclass
class Show:
    id: str
    name: str
    episodes: int


class AllAnimeClient:
    def __init__(self) -> None:
        self.session = requests.Session()
        self.session.headers.update(
            {
                "User-Agent": AGENT,
                "Referer": REFERER,
                "Origin": REFERER,
            }
        )

    # -- search ----------------------------------------------------------------
    def search(self, query: str, mode: str = "sub") -> list[Show]:
        variables = {
            "search": {
                "allowAdult": False,
                "allowUnknown": False,
                "query": query,
            },
            "limit": 40,
            "page": 1,
            "translationType": mode,
            "countryOrigin": "ALL",
        }
        r = self.session.post(
            f"{API_BASE}/api",
            json={"variables": variables, "query": SEARCH_GQL},
            headers={"Content-Type": "application/json"},
            timeout=20,
        )
        r.raise_for_status()
        edges = (
            r.json().get("data", {}).get("shows", {}).get("edges", [])
        )
        shows: list[Show] = []
        for e in edges:
            avail = e.get("availableEpisodes") or {}
            count = avail.get(mode, 0) if isinstance(avail, dict) else 0
            if not count:
                continue
            shows.append(Show(e["_id"], e.get("name", "?"), count))
        return shows

    # -- episode list ----------------------------------------------------------
    def episodes_list(self, show_id: str, mode: str = "sub") -> list[str]:
        r = self.session.post(
            f"{API_BASE}/api",
            json={"variables": {"showId": show_id}, "query": EPISODES_LIST_GQL},
            headers={"Content-Type": "application/json"},
            timeout=20,
        )
        r.raise_for_status()
        detail = (
            r.json()
            .get("data", {})
            .get("show", {})
            .get("availableEpisodesDetail", {})
        )
        eps = detail.get(mode, []) if isinstance(detail, dict) else []
        # Episodes come back as strings; sort numerically.
        return sorted(eps, key=lambda x: float(x) if _isnum(x) else 0.0)

    # -- source urls for one episode ------------------------------------------
    def source_urls(
        self, show_id: str, mode: str, episode: str
    ) -> list[tuple[str, str]]:
        variables = {
            "showId": show_id,
            "translationType": mode,
            "episodeString": episode,
        }
        ext = {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": EPISODE_QUERY_HASH,
            }
        }
        text = ""
        try:
            r = self.session.get(
                f"{API_BASE}/api",
                params={
                    "variables": json.dumps(variables),
                    "extensions": json.dumps(ext),
                },
                timeout=20,
            )
            text = r.text
        except requests.RequestException as e:
            print(f"  ! persisted-query GET failed: {e}", file=sys.stderr)

        if "tobeparsed" not in text and "sourceUrl" not in text:
            # Fall back to a plain POST of the full query.
            r = self.session.post(
                f"{API_BASE}/api",
                json={"variables": variables, "query": EPISODE_EMBED_GQL},
                headers={"Content-Type": "application/json"},
                timeout=20,
            )
            text = r.text

        return parse_source_urls(text)


def _isnum(x: str) -> bool:
    try:
        float(x)
        return True
    except (TypeError, ValueError):
        return False
