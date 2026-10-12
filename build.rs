//! Embeds the short commit hash, shown next to the version; empty without git.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn main() {
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    println!("cargo:rustc-env=GIT_COMMIT={commit}");
    // Rebuild on a new commit: HEAD, and the branch it points to (worktrees included).
    for path in [Some("HEAD".to_string()), git(&["symbolic-ref", "-q", "HEAD"])].into_iter().flatten() {
        if let Some(p) = git(&["rev-parse", "--path-format=absolute", "--git-path", &path]) {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
