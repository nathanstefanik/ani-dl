//! Follow list: shows tracked for new episodes, plus `ani-dl update`.
//!
//! State lives in `~/.config/ani-dl/follows.toml`. A show's identity is the
//! (hianime slug, dubbed) pair, so sub and dub of the same title can both be
//! followed.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::api::{HianimeClient, TranslationType};
use crate::cli::{FollowAction, FollowAdd};
use crate::config::{Config, config_dir};
use crate::download::{EpisodeOutcome, EpisodeTarget, download_episode};

pub fn follows_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("follows.toml"))
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FollowList {
    #[serde(default, rename = "show")]
    pub shows: Vec<Followed>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Followed {
    /// hianime slug, e.g. `sousou-no-frieren-18542`.
    pub id: String,
    /// Display name from search.
    pub name: String,
    /// `file_title_for(name, known_season)` captured at add time.
    pub file_title: String,
    pub season: u32,
    #[serde(default)]
    pub dubbed: bool,
    /// Per-show quality override; `None` uses the config default at update time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// Absolute path, resolved at add time — the sync daemon runs with a
    /// different cwd.
    pub directory: String,
    /// Episodes downloaded, already on disk, or marked done.
    #[serde(default)]
    pub done: Vec<String>,
    /// rfc3339.
    pub added: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<String>,
}

impl Followed {
    /// Episodes in `available` not yet in `done`, in `available` order.
    pub fn pending(&self, available: &[String]) -> Vec<String> {
        available
            .iter()
            .filter(|ep| !self.done.contains(*ep))
            .cloned()
            .collect()
    }

    pub fn mark_done(&mut self, ep: &str) {
        if !self.done.iter().any(|d| d == ep) {
            self.done.push(ep.to_string());
            self.sort_done();
        }
    }

    /// Done list order: numeric value first, then the raw string — so "9"
    /// sorts before "12" and "12" before "12.5".
    fn sort_done(&mut self) {
        self.done
            .sort_by(|a, b| ep_num(a).total_cmp(&ep_num(b)).then_with(|| a.cmp(b)));
    }
}

fn ep_num(s: &str) -> f64 {
    s.parse().unwrap_or(f64::INFINITY)
}

impl FollowList {
    /// A missing file means an empty list; it is NOT created here.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Atomic save: write a sibling `.tmp` file, then rename over the target.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = PathBuf::from(format!("{}.tmp", path.display()));
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&follows_path()?)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&follows_path()?)
    }

    /// Position of the entry with this (id, dubbed) identity, if any.
    pub fn position(&self, id: &str, dubbed: bool) -> Option<usize> {
        self.shows
            .iter()
            .position(|s| s.id == id && s.dubbed == dubbed)
    }

    /// `sel` is a 1-based list index, an exact slug id, or a case-insensitive
    /// substring of name/file_title that matches exactly one entry.
    pub fn find(&self, sel: &str) -> Result<usize> {
        if let Ok(n) = sel.parse::<usize>() {
            if n >= 1 && n <= self.shows.len() {
                return Ok(n - 1);
            }
            anyhow::bail!("no show #{n} ({} followed)", self.shows.len());
        }
        let needle = sel.to_lowercase();
        // Exact slug id wins; if sub AND dub of the same id are both followed
        // it falls into the same ambiguity error as the substring path.
        let mut matches: Vec<usize> = self
            .shows
            .iter()
            .enumerate()
            .filter(|(_, s)| s.id == sel)
            .map(|(i, _)| i)
            .collect();
        if matches.is_empty() {
            matches = self
                .shows
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    s.name.to_lowercase().contains(&needle)
                        || s.file_title.to_lowercase().contains(&needle)
                })
                .map(|(i, _)| i)
                .collect();
        }
        match matches.as_slice() {
            [] => anyhow::bail!("no followed show matching '{sel}'"),
            [i] => Ok(*i),
            many => {
                let candidates = many
                    .iter()
                    .map(|&i| {
                        let s = &self.shows[i];
                        format!("{}. {} ({})", i + 1, s.name, mode_label(s.dubbed))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!("'{sel}' matches {} shows: {candidates}", many.len());
            }
        }
    }

    /// Reload from disk, apply `f` to the (id, dubbed) entry, save. A no-op
    /// when the entry was removed meanwhile — and other entries changed
    /// meanwhile survive, so `update` calls this after every episode.
    pub fn update_entry(
        path: &Path,
        id: &str,
        dubbed: bool,
        f: impl FnOnce(&mut Followed),
    ) -> Result<()> {
        let mut list = Self::load_from(path)?;
        if let Some(entry) = list
            .shows
            .iter_mut()
            .find(|s| s.id == id && s.dubbed == dubbed)
        {
            f(entry);
            list.save_to(path)?;
        }
        Ok(())
    }
}

/// `done` seed for a freshly followed show. `None` → everything currently
/// available counts as done (only future episodes download). `Some(f)` →
/// `available` minus what `{f}--1` selects: `--from 0` backfills the whole
/// show, `--from 5` leaves 5 onward pending, `--from -1` leaves the latest
/// pending.
pub fn initial_done(available: &[String], from: Option<&str>) -> Vec<String> {
    match from {
        None => available.to_vec(),
        Some(f) => {
            let wanted = crate::parse_episode_arg(&format!("{f}--1"), available);
            available
                .iter()
                .filter(|ep| !wanted.contains(*ep))
                .cloned()
                .collect()
        }
    }
}

fn mode_label(dubbed: bool) -> &'static str {
    if dubbed { "dub" } else { "sub" }
}

/// Expand a leading `~/` — clap won't, and a quoted path stays literal.
fn expand_home(p: &str) -> PathBuf {
    if p == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(p)
}

pub async fn run(cfg: &Config, action: &FollowAction) -> Result<()> {
    match action {
        FollowAction::Add(a) => add(cfg, a).await,
        FollowAction::List => list(),
        FollowAction::Remove { show } => remove(show),
        FollowAction::Mark {
            show,
            range,
            unmark,
        } => mark(show, range, *unmark).await,
    }
}

async fn add(cfg: &Config, a: &FollowAdd) -> Result<()> {
    let mode = if a.dubbed {
        TranslationType::Dub
    } else {
        TranslationType::Sub
    };
    let api = HianimeClient::new()?;

    eprintln!("Searching '{}' ({})...", a.query, mode.as_str());
    let results = api.search(&a.query, mode).await?;
    if results.is_empty() {
        anyhow::bail!("no results found");
    }
    let show = crate::pick_show(&results, a.number, crate::non_tty())?;
    let Some(show) = show else {
        return Ok(());
    };
    let known_season = match a.season {
        Some(n) => Some(n),
        None => crate::infer_season(&show, &results, &api, mode).await,
    };
    let season = known_season.unwrap_or(1);
    let file_title = crate::file_title_for(&show.name, known_season);

    let available = api.episode_list(&show.id, mode).await?;

    let mut list = FollowList::load()?;
    if list.position(&show.id, a.dubbed).is_some() {
        anyhow::bail!(
            "already following '{}' ({}) — use `follow mark` to adjust progress",
            show.name,
            mode_label(a.dubbed)
        );
    }

    let dir_raw = a
        .download_dir
        .clone()
        .unwrap_or_else(|| cfg.download.directory.clone());
    let dir = std::path::absolute(expand_home(&dir_raw))
        .with_context(|| format!("resolving directory '{dir_raw}'"))?;

    let done = initial_done(&available, a.from.as_deref());
    if a.from.is_some() && done.len() == available.len() && !available.is_empty() {
        eprintln!(
            "  ! --from {} matched no available episode; treating all {} as done",
            a.from.as_deref().unwrap_or_default(),
            available.len()
        );
    }
    let pending = available.len() - done.len();

    list.shows.push(Followed {
        id: show.id.clone(),
        name: show.name.clone(),
        file_title,
        season,
        dubbed: a.dubbed,
        quality: a.quality.clone(),
        directory: dir.to_string_lossy().into_owned(),
        done,
        added: Utc::now().to_rfc3339(),
        last_checked: None,
    });
    list.save()?;

    println!(
        "Following {} (S{:02}, {}) → {} — {} available, {} pending. Run `ani-dl update` to fetch.",
        show.name,
        season,
        mode_label(a.dubbed),
        dir.display(),
        available.len(),
        pending
    );
    Ok(())
}

fn list() -> Result<()> {
    let list = FollowList::load()?;
    if list.shows.is_empty() {
        println!("Not following any shows. Add one with `ani-dl follow add <QUERY>`.");
        return Ok(());
    }
    let name_w = list.shows.iter().map(|s| s.name.len()).max().unwrap_or(0);
    for (i, s) in list.shows.iter().enumerate() {
        let checked = s
            .last_checked
            .as_deref()
            .and_then(|t| t.get(..10))
            .unwrap_or("never");
        println!(
            "{:>2}  {:<name_w$}  S{:02}  {:3}  {:>3} done  {:>10}  {}",
            i + 1,
            s.name,
            s.season,
            mode_label(s.dubbed),
            s.done.len(),
            checked,
            s.directory,
        );
    }
    Ok(())
}

fn remove(sel: &str) -> Result<()> {
    let mut list = FollowList::load()?;
    let i = list.find(sel)?;
    let s = list.shows.remove(i);
    list.save()?;
    println!("Unfollowed {} ({}).", s.name, mode_label(s.dubbed));
    Ok(())
}

async fn mark(sel: &str, range: &str, unmark: bool) -> Result<()> {
    let mut list = FollowList::load()?;
    let i = list.find(sel)?;
    let (id, dubbed) = (list.shows[i].id.clone(), list.shows[i].dubbed);
    let mode = if dubbed {
        TranslationType::Dub
    } else {
        TranslationType::Sub
    };
    let available = HianimeClient::new()?.episode_list(&id, mode).await?;
    let eps = crate::parse_episode_arg(range, &available);
    if eps.is_empty() {
        anyhow::bail!("no episodes matched '{range}'");
    }

    let entry = &mut list.shows[i];
    if unmark {
        entry.done.retain(|e| !eps.contains(e));
    } else {
        for ep in &eps {
            entry.mark_done(ep);
        }
    }
    let name = entry.name.clone();
    let done_count = entry.done.len();
    list.save()?;
    println!(
        "{name}: {} episode(s) {} — {done_count} done.",
        eps.len(),
        if unmark { "unmarked" } else { "marked" },
    );
    Ok(())
}

#[derive(Debug, Default)]
pub struct UpdateOpts {
    /// `follow list` indices, slug ids, or unique name substrings.
    /// Empty means every followed show.
    pub selectors: Vec<String>,
    /// Report pending episodes without downloading or writing state.
    pub dry_run: bool,
    pub concurrency: Option<usize>,
    pub force: bool,
}

#[derive(Debug, Default)]
pub struct UpdateSummary {
    pub shows_checked: usize,
    /// Episodes newly accounted for (downloaded, or already on disk).
    pub downloaded: usize,
    pub failed: usize,
    /// Human-readable per-show and per-episode failures, for logging.
    pub errors: Vec<String>,
}

pub async fn run_update(cfg: &Config, opts: &UpdateOpts) -> Result<UpdateSummary> {
    let path = follows_path()?;
    let list = FollowList::load_from(&path)?;
    let mut summary = UpdateSummary::default();
    if list.shows.is_empty() {
        println!("Not following any shows. Add one with `ani-dl follow add <QUERY>`.");
        return Ok(summary);
    }

    // Resolve selectors before any network work.
    let mut indices: Vec<usize> = Vec::new();
    if opts.selectors.is_empty() {
        indices.extend(0..list.shows.len());
    } else {
        for sel in &opts.selectors {
            let i = list.find(sel)?;
            if !indices.contains(&i) {
                indices.push(i);
            }
        }
    }

    let api = HianimeClient::new()?;
    let dl_client = crate::api::download_client()?;

    for i in indices {
        let entry = &list.shows[i];
        let mode = if entry.dubbed {
            TranslationType::Dub
        } else {
            TranslationType::Sub
        };
        summary.shows_checked += 1;

        let available = match api.episode_list(&entry.id, mode).await {
            Ok(a) => a,
            Err(e) => {
                let msg = format!("{}: {e:#}", entry.name);
                eprintln!("  ! {msg}");
                summary.errors.push(msg);
                continue;
            }
        };

        let pending = entry.pending(&available);
        if pending.is_empty() {
            println!("{}: up to date ({} eps)", entry.name, available.len());
        } else if opts.dry_run {
            println!(
                "{}: {} new — {}",
                entry.name,
                pending.len(),
                pending.join(", ")
            );
            continue; // dry-run: no writes, no last_checked bump
        } else {
            let target = EpisodeTarget {
                show_id: &entry.id,
                file_title: &entry.file_title,
                season: entry.season,
                mode,
                quality: entry.quality.as_deref().unwrap_or(&cfg.download.quality),
                out_dir: Path::new(&entry.directory),
                concurrency: opts.concurrency.unwrap_or(cfg.download.concurrency),
                retries: cfg.download.retries,
                force: opts.force,
            };
            let total = pending.len();
            for (n, ep) in pending.iter().enumerate() {
                eprintln!("\n=== {} episode {ep} ({}/{total}) ===", entry.name, n + 1);
                match download_episode(&api, &dl_client, &target, ep).await {
                    Ok(EpisodeOutcome::Downloaded | EpisodeOutcome::Skipped) => {
                        match FollowList::update_entry(&path, &entry.id, entry.dubbed, |s| {
                            s.mark_done(ep)
                        }) {
                            Ok(()) => summary.downloaded += 1,
                            Err(e) => {
                                let msg = format!(
                                    "{} ep {ep}: downloaded but failed to save state: {e:#}",
                                    entry.name
                                );
                                eprintln!("  ! {msg}");
                                summary.errors.push(msg);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  ! {e:#}");
                        summary.failed += 1;
                        summary
                            .errors
                            .push(format!("{} ep {ep}: {e:#}", entry.name));
                    }
                }
            }
        }

        if !opts.dry_run
            && let Err(e) = FollowList::update_entry(&path, &entry.id, entry.dubbed, |s| {
                s.last_checked = Some(Utc::now().to_rfc3339());
            })
        {
            let msg = format!("{}: failed to save last_checked: {e:#}", entry.name);
            eprintln!("  ! {msg}");
            summary.errors.push(msg);
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn followed(id: &str, name: &str, dubbed: bool) -> Followed {
        Followed {
            id: id.to_string(),
            name: name.to_string(),
            file_title: name.to_string(),
            season: 1,
            dubbed,
            quality: None,
            directory: "/tmp".to_string(),
            done: Vec::new(),
            added: "2025-01-01T00:00:00+00:00".to_string(),
            last_checked: None,
        }
    }

    fn eps(ns: &[&str]) -> Vec<String> {
        ns.iter().map(|s| s.to_string()).collect()
    }

    fn temp_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ani-dl-test-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // pending

    #[test]
    fn pending_excludes_done_and_keeps_available_order() {
        let mut s = followed("a", "Show", false);
        s.done = eps(&["2", "4"]);
        assert_eq!(s.pending(&eps(&["4", "1", "2", "3"])), eps(&["1", "3"]));
    }

    #[test]
    fn pending_handles_decimal_episodes() {
        let mut s = followed("a", "Show", false);
        s.done = eps(&["12"]);
        assert_eq!(s.pending(&eps(&["12", "12.5", "13"])), eps(&["12.5", "13"]));
    }

    // initial_done

    #[test]
    fn initial_done_none_marks_everything_done() {
        assert_eq!(
            initial_done(&eps(&["1", "2", "3"]), None),
            eps(&["1", "2", "3"])
        );
    }

    #[test]
    fn initial_done_from_zero_backfills_everything() {
        assert!(initial_done(&eps(&["1", "2", "3"]), Some("0")).is_empty());
    }

    #[test]
    fn initial_done_from_five_leaves_five_onward_pending() {
        let avail = eps(&["1", "2", "3", "4", "5", "6"]);
        assert_eq!(initial_done(&avail, Some("5")), eps(&["1", "2", "3", "4"]));
    }

    #[test]
    fn initial_done_from_minus_one_leaves_latest_pending() {
        let avail = eps(&["1", "2", "3"]);
        // "-1--1" splits into (-1, -1) — the last episode on both sides.
        assert_eq!(initial_done(&avail, Some("-1")), eps(&["1", "2"]));
    }

    #[test]
    fn initial_done_empty_available() {
        assert!(initial_done(&[], None).is_empty());
        assert!(initial_done(&[], Some("5")).is_empty());
    }

    #[test]
    fn initial_done_from_beyond_last_marks_all_done() {
        let avail = eps(&["1", "2"]);
        assert_eq!(initial_done(&avail, Some("99")), avail);
    }

    // find

    fn two_shows() -> FollowList {
        FollowList {
            shows: vec![
                followed("frieren-1", "Sousou no Frieren", false),
                followed("frieren-1", "Sousou no Frieren", true),
                followed("op-2", "One Piece", false),
            ],
        }
    }

    #[test]
    fn find_by_one_based_index() {
        let list = two_shows();
        assert_eq!(list.find("1").unwrap(), 0);
        assert_eq!(list.find("3").unwrap(), 2);
    }

    #[test]
    fn find_index_zero_and_out_of_range_error() {
        let list = two_shows();
        assert!(list.find("0").is_err());
        assert!(list.find("4").is_err());
    }

    #[test]
    fn find_by_exact_id() {
        let list = two_shows();
        assert_eq!(list.find("op-2").unwrap(), 2);
    }

    #[test]
    fn find_by_id_ambiguous_between_sub_and_dub() {
        let list = two_shows();
        let err = list.find("frieren-1").unwrap_err().to_string();
        assert!(err.contains("1. Sousou no Frieren (sub)"));
        assert!(err.contains("2. Sousou no Frieren (dub)"));
        // An id followed only once still resolves directly.
        assert_eq!(list.find("op-2").unwrap(), 2);
    }

    #[test]
    fn find_by_unique_substring_case_insensitive() {
        let list = two_shows();
        assert_eq!(list.find("one PIECE").unwrap(), 2);
    }

    #[test]
    fn find_ambiguous_substring_errors_with_candidates() {
        let list = two_shows();
        let err = list.find("frieren").unwrap_err().to_string();
        assert!(err.contains("1. Sousou no Frieren (sub)"));
        assert!(err.contains("2. Sousou no Frieren (dub)"));
    }

    #[test]
    fn find_no_match_errors() {
        let list = two_shows();
        assert!(list.find("naruto").is_err());
    }

    // toml roundtrip

    #[test]
    fn toml_roundtrip_and_missing_file() {
        let dir = temp_dir();
        let path = dir.join("follows.toml");
        assert!(FollowList::load_from(&path).unwrap().shows.is_empty());
        assert!(!path.exists());

        let mut list = FollowList::default();
        let mut s = followed("a-1", "Show A", false);
        s.done = eps(&["1", "2"]);
        list.shows.push(s);
        list.save_to(&path).unwrap();
        assert!(!dir.join("follows.toml.tmp").exists());

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[show]]"));
        assert!(!text.contains("quality"));
        assert!(!text.contains("last_checked"));

        let back = FollowList::load_from(&path).unwrap();
        assert_eq!(back.shows.len(), 1);
        assert_eq!(back.shows[0].id, "a-1");
        assert_eq!(back.shows[0].done, eps(&["1", "2"]));
        std::fs::remove_dir_all(&dir).ok();
    }

    // update_entry

    #[test]
    fn update_entry_preserves_concurrent_adds() {
        let dir = temp_dir();
        let path = dir.join("follows.toml");
        let mut list = FollowList::default();
        list.shows.push(followed("a-1", "Show A", false));
        list.save_to(&path).unwrap();

        // Another process adds a show after our in-memory load.
        let mut other = FollowList::load_from(&path).unwrap();
        other.shows.push(followed("b-2", "Show B", false));
        other.save_to(&path).unwrap();

        FollowList::update_entry(&path, "a-1", false, |s| s.mark_done("1")).unwrap();
        let reloaded = FollowList::load_from(&path).unwrap();
        assert_eq!(reloaded.shows.len(), 2);
        assert_eq!(reloaded.shows[0].done, eps(&["1"]));
        assert_eq!(reloaded.shows[1].id, "b-2");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_entry_missing_entry_is_noop() {
        let dir = temp_dir();
        let path = dir.join("follows.toml");
        let mut list = FollowList::default();
        list.shows.push(followed("a-1", "Show A", false));
        list.save_to(&path).unwrap();

        FollowList::update_entry(&path, "gone-9", false, |s| s.mark_done("1")).unwrap();
        let reloaded = FollowList::load_from(&path).unwrap();
        assert_eq!(reloaded.shows.len(), 1);
        assert!(reloaded.shows[0].done.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
