use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Worktrees store `.git` as a file; ask git for the real path so HEAD
    // moves still trigger a rebuild of the embedded commit.
    for name in ["HEAD", "index"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name])
            && Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let sha = git_sha().unwrap_or_else(|| "unknown".to_string());
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=ANI_DL_GIT_SHA={sha}");
    println!("cargo:rustc-env=ANI_DL_TARGET={target}");
}

fn git_sha() -> Option<String> {
    let sha = git(&["rev-parse", "--short=9", "HEAD"])?;
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|s| !s.trim().is_empty());
    Some(if dirty { format!("{sha}-dirty") } else { sha })
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}
