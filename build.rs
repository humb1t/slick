//! Embeds the short commit hash, shown next to the version; empty without git.
//! On Linux also builds the sign-in window helper (login/) for `login.rs` to
//! carry: slick itself then never links WebKitGTK.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// `cargo build` of login/ into its own target folder (no lock shared with
/// this build), same target and profile, then copied to `$OUT_DIR/slick-login`.
fn login_helper() {
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} not set"));
    let root = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env("OUT_DIR"));
    let target = env("TARGET");
    let release = env("PROFILE") == "release";
    let target_dir = out.join("login-target");

    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.arg("build")
        .arg("--manifest-path")
        .arg(root.join("login/Cargo.toml"))
        .args(["--target", &target])
        .arg("--target-dir")
        .arg(&target_dir);
    if release {
        cmd.arg("--release");
    }
    // What cargo hands this script is about this build, not the helper's.
    for (k, _) in std::env::vars() {
        if (k.starts_with("CARGO_") && k != "CARGO_HOME") || k == "RUSTFLAGS" || k == "RUSTC_WRAPPER" || k == "RUSTC_WORKSPACE_WRAPPER" {
            cmd.env_remove(k);
        }
    }
    // Its stdout would be read as cargo: instructions.
    let status = cmd.stdout(Stdio::null()).status().expect("cargo build of login/");
    assert!(status.success(), "building the sign-in window helper (login/) failed");

    let built = target_dir.join(&target).join(if release { "release" } else { "debug" }).join("slick-login");
    std::fs::copy(&built, out.join("slick-login")).expect("copy slick-login");

    for p in ["login/Cargo.toml", "login/Cargo.lock", "login/main.rs", "src/login_window.rs", "assets/icon-64.rgba"] {
        println!("cargo:rerun-if-changed={}", Path::new(&root).join(p).display());
    }
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
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        login_helper();
    }
}
