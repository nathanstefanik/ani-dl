//! clap argument definitions.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "ani-dl",
    version,
    about = "Download anime via the AllAnime API (no playback)."
)]
pub struct Cli {
    /// Search term. If omitted (and a TUI is used) you'll be prompted.
    pub query: Option<String>,

    /// Override download directory (default: current directory).
    #[arg(short = 'd', long = "download-dir")]
    pub download_dir: Option<String>,

    /// Video quality: best | worst | 1080 | 720 | 480.
    #[arg(short = 'q', long, default_value = "best")]
    pub quality: String,

    /// Episode range: "1", "1-12", "1 2 5" (bypasses TUI selection).
    #[arg(short = 'e', long)]
    pub episodes: Option<String>,

    /// Use the dubbed version (default: subbed).
    #[arg(short = 'D', long)]
    pub dubbed: bool,

    /// Season number for the SxxExx filename tag.
    #[arg(short = 's', long, default_value_t = 1)]
    pub season: u32,

    /// Auto-select the nth search result (skips TUI).
    #[arg(short = 'n', long)]
    pub number: Option<usize>,

    /// Parallel HLS segment downloads.
    #[arg(short = 'c', long)]
    pub concurrency: Option<usize>,

    /// Print resolved provider URLs and exit (debug).
    #[arg(long = "list-providers")]
    pub list_providers: bool,

    /// Plain-text output, no TUI (for scripting; requires -n and -e).
    #[arg(long = "no-tui")]
    pub no_tui: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run provider sync + health check.
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
        #[arg(long, default_value = "https://youtu-chan.com")]
        referer: String,
    },
}
