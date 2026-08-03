//! clap argument definitions.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "ani-dl",
    version,
    about = "Download anime via anidb.app (ani-cli v5 provider, no playback)."
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

    /// Episode range: "1", "1-12", "1 2 5" (bypasses TUI selection).
    #[arg(short = 'e', long)]
    pub episodes: Option<String>,

    /// Use the dubbed version (default: subbed).
    #[arg(short = 'D', long)]
    pub dubbed: bool,

    /// Season number for the SxxExx filename tag (auto-inferred when omitted).
    #[arg(short = 's', long)]
    pub season: Option<u32>,

    /// Auto-select the nth search result (skips TUI).
    #[arg(short = 'n', long)]
    pub number: Option<usize>,

    /// Parallel HLS segment downloads.
    #[arg(short = 'c', long)]
    pub concurrency: Option<usize>,

    /// Print resolved stream URLs and exit (debug).
    #[arg(long = "list-providers")]
    pub list_providers: bool,

    /// Plain-text output, no TUI (for scripting; requires -n and -e).
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
    /// Hidden: download a raw HLS/mp4 URL directly (for testing hls.rs).
    #[command(hide = true)]
    Hls {
        url: String,
        out: String,
        #[arg(short = 'c', long, default_value_t = 16)]
        concurrency: usize,
        #[arg(short = 'q', long, default_value = "best")]
        quality: String,
        #[arg(long, default_value = "https://anidb.app/")]
        referer: String,
    },
}
