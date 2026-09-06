//! ani-dl's own version identity.
//!
//! Two version numbers matter here and they move for different reasons, so
//! both are reported. `VERSION` is ani-dl's semver — it describes this CLI's
//! own behaviour and options. `ANI_CLI_PARITY` names the ani-cli release whose
//! scraping behaviour this build mirrors, which changes whenever upstream
//! changes how the provider is scraped, independently of ani-dl's own release
//! cadence.

use std::sync::OnceLock;

use crate::constants::ANIDB_BASE;

/// ani-dl's semver. Cargo.toml is the single source of truth.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The ani-cli release this build's scraping paths track.
pub const ANI_CLI_PARITY: &str = "5.0.4";

/// Short commit this binary was built from, or `unknown` when built outside a
/// git checkout (a release tarball, for instance).
pub const GIT_SHA: &str = env!("ANI_DL_GIT_SHA");

/// Target triple this binary was compiled for.
pub const TARGET: &str = env!("ANI_DL_TARGET");

/// One line, for `-V`: `1.0.0 (a1b2c3d45)`.
pub fn short() -> &'static str {
    static SHORT: OnceLock<String> = OnceLock::new();
    SHORT.get_or_init(|| {
        if GIT_SHA == "unknown" {
            VERSION.to_string()
        } else {
            format!("{VERSION} ({GIT_SHA})")
        }
    })
}

/// Full build provenance, for `--version`. clap prefixes the binary name.
pub fn long() -> &'static str {
    static LONG: OnceLock<String> = OnceLock::new();
    LONG.get_or_init(|| {
        format!(
            "{VERSION}\n\
             commit:         {GIT_SHA}\n\
             target:         {TARGET}\n\
             ani-cli parity: {ANI_CLI_PARITY}\n\
             provider:       {ANIDB_BASE}"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_semver_from_cargo() {
        let parts: Vec<&str> = VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "expected MAJOR.MINOR.PATCH, got {VERSION}");
        assert!(parts.iter().all(|p| p.parse::<u32>().is_ok()));
    }

    #[test]
    fn parity_is_the_ani_cli_version_string() {
        // Guards against pasting ani-dl's own version in here by mistake.
        assert!(ANI_CLI_PARITY.starts_with("5."));
        assert_ne!(ANI_CLI_PARITY, VERSION);
    }

    #[test]
    fn long_version_leads_with_the_number_not_the_binary_name() {
        // clap renders "ani-dl {long}", so a leading name would duplicate it.
        assert!(long().starts_with(VERSION));
        assert!(long().contains("ani-cli parity: "));
    }
}
