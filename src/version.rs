use std::sync::LazyLock;

use crate::constants::ANIDB_BASE;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ANI_CLI_PARITY: &str = "5.0.4";

const GIT_SHA: &str = env!("ANI_DL_GIT_SHA");
const TARGET: &str = env!("ANI_DL_TARGET");

/// `-V`: `1.0.0 (a1b2c3d45)`. clap needs a `'static` string.
pub fn short() -> &'static str {
    static SHORT: LazyLock<String> = LazyLock::new(|| {
        if GIT_SHA == "unknown" {
            VERSION.to_string()
        } else {
            format!("{VERSION} ({GIT_SHA})")
        }
    });
    &SHORT
}

/// `--version`. clap prefixes the binary name, so this must not.
pub fn long() -> &'static str {
    static LONG: LazyLock<String> = LazyLock::new(|| {
        format!(
            "{VERSION}\n\
             commit:         {GIT_SHA}\n\
             target:         {TARGET}\n\
             ani-cli parity: {ANI_CLI_PARITY}\n\
             provider:       {ANIDB_BASE}"
        )
    });
    &LONG
}
