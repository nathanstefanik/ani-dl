//! clap argument definitions.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "ani-dl",
    version = crate::version::short(),
    long_version = crate::version::long(),
    about = "Download anime via hianime.at (ani-cli v5 provider, no playback)."
)]
pub struct Cli {
    /// Search term. If omitted (and a TUI is used) you'll be prompted.
    pub query: Option<String>,

    /// Override download directory (default: current directory).
    #[arg(short = 'd', long = "download-dir")]
    pub download_dir: Option<String>,

    /// Video quality: best | worst | 1080 | 720 | 480 (default: config value).
    #[arg(short = 'q', long)]
    pub quality: Option<String>,

    /// Episodes: "1", "1-12", "1 2 5"; 0 = first and -1 = last (e.g. "5--1"
    /// for episode 5 through the end, "0--1" for everything). Skips the TUI
    /// and defaults the search pick to 1 (override with -n).
    // allow_hyphen_values so a leading "-1" is read as the value, not a flag.
    #[arg(short = 'e', long, allow_hyphen_values = true)]
    pub episodes: Option<String>,

    /// Use the dubbed version (default: subbed).
    #[arg(short = 'D', long)]
    pub dubbed: bool,

    /// Season number for the SxxExx filename tag (auto-inferred when omitted).
    #[arg(short = 's', long)]
    pub season: Option<u32>,

    /// Auto-select the nth search result (1-based). Defaults to 1 when -e is set.
    #[arg(short = 'n', long)]
    pub number: Option<usize>,

    /// Parallel HLS segment downloads.
    #[arg(short = 'c', long)]
    pub concurrency: Option<usize>,

    /// Print resolved stream URLs and exit (debug).
    #[arg(long = "list-providers")]
    pub list_providers: bool,

    /// Plain-text output, no TUI. Implied when -e is set or stdin/stdout is not a TTY.
    #[arg(long = "no-tui")]
    pub no_tui: bool,

    /// Re-download episodes even when the output file already exists.
    #[arg(short = 'f', long)]
    pub force: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run provider health check.
    Sync {
        /// Run detached as a daily background daemon.
        #[arg(long)]
        daemon: bool,
    },
    /// Print the config path and contents.
    Config,
    /// Manage followed shows (`ani-dl update` fetches their new episodes).
    Follow {
        #[command(subcommand)]
        action: FollowAction,
    },
    /// Download new episodes for all (or selected) followed shows.
    Update {
        /// Only check these shows: `follow list` index, slug id, or a unique
        /// name substring. All followed shows when omitted.
        shows: Vec<String>,
        /// List pending episodes without downloading or writing state.
        #[arg(long)]
        dry_run: bool,
        /// Parallel HLS segment downloads.
        #[arg(short = 'c', long)]
        concurrency: Option<usize>,
        /// Re-download episodes even when the output file already exists.
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Hidden: download a raw HLS/mp4 URL directly (for testing hls.rs).
    #[command(hide = true)]
    Hls {
        url: String,
        out: String,
        #[arg(short = 'c', long, default_value_t = 16)]
        concurrency: usize,
        #[arg(short = 'q', long, default_value = "best")]
        quality: String,
        #[arg(long, default_value = "https://zokoanime.video/")]
        referer: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum FollowAction {
    /// Search for a show and follow it for new episodes.
    Add(FollowAdd),
    /// List followed shows.
    #[command(visible_alias = "ls")]
    List,
    /// Stop following a show (downloaded files are kept).
    #[command(visible_alias = "rm")]
    Remove {
        /// Show: `follow list` index, slug id, or unique name substring.
        show: String,
    },
    /// Add or remove episodes in a show's done list.
    Mark {
        /// Show: `follow list` index, slug id, or unique name substring.
        show: String,
        /// Episode range: "1", "1-12", "5--1"; 0 = first, -1 = last.
        #[arg(allow_hyphen_values = true)]
        range: String,
        /// Remove episodes from the done list instead of adding them.
        #[arg(long)]
        unmark: bool,
    },
}

#[derive(clap::Args, Debug)]
pub struct FollowAdd {
    /// Search term.
    pub query: String,
    /// Auto-select the nth search result (1-based).
    #[arg(short = 'n', long)]
    pub number: Option<usize>,
    /// Follow the dubbed version (default: subbed).
    #[arg(short = 'D', long)]
    pub dubbed: bool,
    /// Season number for the SxxExx filename tag (auto-inferred when omitted).
    #[arg(short = 's', long)]
    pub season: Option<u32>,
    /// Video quality for this show: best | worst | 1080 | 720 | 480 (default: config value).
    #[arg(short = 'q', long)]
    pub quality: Option<String>,
    /// Download directory (default: config value). Stored as an absolute path.
    #[arg(short = 'd', long = "download-dir")]
    pub download_dir: Option<String>,
    /// Leave episodes before EP marked done: "0" fetches everything,
    /// "-1" only fetches new ones (default: everything already aired is done).
    #[arg(long, allow_hyphen_values = true)]
    pub from: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_add_from_accepts_hyphen_values() {
        let cli =
            Cli::try_parse_from(["ani-dl", "follow", "add", "frieren", "--from", "-1"]).unwrap();
        let Some(Command::Follow {
            action: FollowAction::Add(a),
        }) = cli.command
        else {
            panic!("expected follow add, got {:?}", cli.command);
        };
        assert_eq!(a.query, "frieren");
        assert_eq!(a.from.as_deref(), Some("-1"));
    }

    #[test]
    fn follow_mark_parses_range_and_show() {
        let cli = Cli::try_parse_from(["ani-dl", "follow", "mark", "1", "5--1"]).unwrap();
        let Some(Command::Follow {
            action:
                FollowAction::Mark {
                    show,
                    range,
                    unmark,
                },
        }) = cli.command
        else {
            panic!("expected follow mark, got {:?}", cli.command);
        };
        assert_eq!(show, "1");
        assert_eq!(range, "5--1");
        assert!(!unmark);
    }

    #[test]
    fn follow_ls_alias_parses() {
        let cli = Cli::try_parse_from(["ani-dl", "follow", "ls"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Follow {
                action: FollowAction::List
            })
        ));
    }

    #[test]
    fn update_parses_dry_run_and_selectors() {
        let cli = Cli::try_parse_from(["ani-dl", "update", "--dry-run", "frieren"]).unwrap();
        let Some(Command::Update {
            shows,
            dry_run,
            concurrency,
            force,
        }) = cli.command
        else {
            panic!("expected update, got {:?}", cli.command);
        };
        assert_eq!(shows, ["frieren"]);
        assert!(dry_run);
        assert_eq!(concurrency, None);
        assert!(!force);
    }

    #[test]
    fn bare_query_still_parses_without_subcommand() {
        let cli = Cli::try_parse_from(["ani-dl", "attack on titan", "-e", "1"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.query.as_deref(), Some("attack on titan"));
        assert_eq!(cli.episodes.as_deref(), Some("1"));
    }
}
