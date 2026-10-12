//! New versions from the GitHub releases: looked for at start-up and every
//! few hours, and installed over this binary when you click the button by
//! your name, unless something else (cargo, the system's package manager,
//! nix, homebrew) owns it; then it only tells how to update.

use crate::app::Event;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LATEST: &str = "https://api.github.com/repos/leonavas/slick/releases/latest";
const REPO: &str = "https://github.com/leonavas/slick";
/// Between checks while slick is open.
const EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// Download links by file name.
pub type Assets = Vec<(String, String)>;

pub enum Note {
    /// A newer release than this binary.
    Found { ver: String, assets: Assets },
    /// The install asked for finished: the version, or why not.
    Installed(Result<String, String>),
}

/// Where the update stands, shown as the button by your name.
pub enum State {
    /// Found; a click installs it.
    Available { ver: String, assets: Assets },
    /// Downloading and swapping the binary.
    Installing { ver: String, assets: Assets },
    /// Installed over the running binary; a restart picks it up.
    Installed(String),
}

impl State {
    /// After a check found `ver`: kept while installing or once installed.
    pub fn found(prev: Option<State>, ver: String, assets: Assets) -> State {
        match prev {
            Some(s @ State::Installing { .. }) => s,
            Some(State::Installed(v)) if !newer(&ver, &v) => State::Installed(v),
            _ => State::Available { ver, assets },
        }
    }
}

enum Mode {
    /// A cargo build, or neither Linux nor Windows: nothing is checked.
    Off,
    /// Owned by another tool, or in a folder we can't write to.
    Managed,
    /// Ours to replace.
    Own,
}

/// The installed binary. Once replaced, Linux names the running one
/// "… (deleted)"; Windows has renamed it to `OLD`.
pub fn exe() -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?;
    if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(OLD)) {
        return Some(p.with_file_name("slick.exe"));
    }
    let s = p.to_string_lossy();
    Some(s.strip_suffix(" (deleted)").map(PathBuf::from).unwrap_or(p))
}

/// Windows: where the running .exe goes when a new one takes its place.
const OLD: &str = "slick.old.exe";

/// Deletes the `OLD` an update left behind, which was still running then.
pub fn cleanup() {
    if cfg!(windows) {
        if let Some(exe) = exe() {
            let _ = std::fs::remove_file(exe.with_file_name(OLD));
        }
    }
}

fn cargo_bin() -> PathBuf {
    std::env::var_os("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|| crate::config::home().join(".cargo")).join("bin")
}

/// From a release (install.sh in ~/.local/bin, install.ps1 in
/// %LOCALAPPDATA%\Programs\slick, or any folder you can write to) it is ours.
fn mode() -> Mode {
    let Some(exe) = exe().filter(|_| cfg!(any(target_os = "linux", windows))) else { return Mode::Off };
    if exe.components().any(|c| c.as_os_str() == "target") {
        return Mode::Off;
    }
    let s = exe.to_string_lossy();
    let system = ["/usr/", "/nix/", "/opt/homebrew"].iter().any(|o| s.starts_with(o));
    let dir = exe.parent().unwrap_or(Path::new("/"));
    if exe.starts_with(cargo_bin()) || system || !writable(dir) {
        return Mode::Managed;
    }
    Mode::Own
}

/// `SLICK_UPDATE_DEV=1` checks even from `target/`, to see the button.
fn dev() -> bool {
    std::env::var("SLICK_UPDATE_DEV").is_ok_and(|v| v == "1")
}

/// None when a click can install; else how to update, for the status line.
pub fn how() -> Option<String> {
    let exe = exe().unwrap_or_default();
    let s = exe.to_string_lossy();
    let dir = exe.parent().map(|d| d.display().to_string()).unwrap_or_default();
    let msg = match mode() {
        Mode::Own => return None,
        Mode::Off => "build de desenvolvimento: não se atualiza, compile de novo".to_string(),
        Mode::Managed if exe.starts_with(cargo_bin()) => {
            format!("instalado via cargo: rode cargo install --locked --git {REPO}")
        }
        Mode::Managed if s.starts_with("/nix/") => "instalado via nix: atualize pelo nix".into(),
        Mode::Managed if s.starts_with("/opt/homebrew") => "instalado via homebrew: atualize pelo brew".into(),
        Mode::Managed if s.starts_with("/usr/") => "instalado pelo sistema: atualize pelo gerenciador de pacotes".into(),
        Mode::Managed => format!("{dir} não é gravável: baixe a versão nova em {REPO}/releases"),
    };
    Some(msg)
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".slick-probe-{}", std::process::id()));
    std::fs::write(&probe, b"").and_then(|_| std::fs::remove_file(&probe)).is_ok()
}

/// "1.10.0" > "1.9.2"; whatever follows the digits of a part ("0-rc1") is ignored.
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split('.').map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)).collect()
    };
    parts(a) > parts(b)
}

/// The latest release's version (without the "v") and its download links by file name.
fn latest() -> Result<(String, Assets), String> {
    let v: serde_json::Value = ureq::get(LATEST)
        .set("User-Agent", concat!("slick/", env!("CARGO_PKG_VERSION")))
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().ok_or("release sem tag")?.trim_start_matches('v').to_string();
    let assets = v["assets"].as_array().into_iter().flatten();
    let assets = assets
        .filter_map(|a| Some((a["name"].as_str()?.to_string(), a["browser_download_url"].as_str()?.to_string())))
        .collect();
    Ok((tag, assets))
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let res = ureq::get(url).set("User-Agent", "slick").call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    res.into_reader().take(200 << 20).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// This machine's archive in a release.
fn archive(ver: &str) -> String {
    let arch = std::env::consts::ARCH;
    if cfg!(windows) {
        format!("slick-{ver}-{arch}-pc-windows-msvc.zip")
    } else {
        format!("slick-{ver}-{arch}-unknown-linux-gnu.tar.gz")
    }
}

/// Downloads this machine's archive, checks its sha256 and swaps `slick` for the new one.
fn install(ver: &str, assets: &[(String, String)], log: &dyn Fn(&str)) -> Result<(), String> {
    let exe = exe().ok_or("não achou o executável")?;
    let dir = exe.parent().ok_or("executável sem pasta")?;
    let name = archive(ver);
    let url = |n: &str| assets.iter().find(|(a, _)| a == n).map(|(_, u)| u.clone()).ok_or(format!("release sem {n}"));
    let (tar_url, sum_url) = (url(&name)?, url(&format!("{name}.sha256"))?);
    log(&format!("baixando {name}…"));
    let tar = download(&tar_url)?;
    let sum = String::from_utf8_lossy(&download(&sum_url)?).split_whitespace().next().unwrap_or("").to_lowercase();
    let got: String = Sha256::digest(&tar).iter().map(|b| format!("{b:02x}")).collect();
    if got != sum {
        return Err(format!("sha256 não confere ({got} ≠ {sum})"));
    }
    // Unpacked next to the binary, so the rename below stays on one filesystem.
    let tmp = dir.join(format!(".slick-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let done = (|| {
        std::fs::write(tmp.join(&name), &tar).map_err(|e| e.to_string())?;
        // Windows 10 and later ship bsdtar, which unpacks the .zip too.
        let (tar, flags) = match std::env::var_os("SystemRoot").filter(|_| cfg!(windows)) {
            Some(root) => (PathBuf::from(root).join("System32").join("tar.exe"), "-xf"),
            None => (PathBuf::from("tar"), "-xzf"),
        };
        let ok = std::process::Command::new(tar).arg(flags).arg(&name).current_dir(&tmp).status();
        if !ok.is_ok_and(|s| s.success()) {
            return Err("tar não extraiu o pacote".to_string());
        }
        let file = if cfg!(windows) { "slick.exe" } else { "slick" };
        let new = PathBuf::from(format!("{}.new", exe.display()));
        std::fs::rename(tmp.join(file), &new).map_err(|e| format!("{file}: {e}"))?;
        #[cfg(unix)]
        std::fs::set_permissions(&new, std::os::unix::fs::PermissionsExt::from_mode(0o755)).map_err(|e| e.to_string())?;
        replace(&new, &exe).map_err(|e| format!("{file}: {e}"))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    done
}

/// Puts `new` in place of the binary.
#[cfg(unix)]
fn replace(new: &Path, exe: &Path) -> std::io::Result<()> {
    std::fs::rename(new, exe)
}

/// Windows can't overwrite a running .exe but can rename it: the old one
/// steps aside as `OLD`, deleted at the next start (`cleanup`).
#[cfg(windows)]
fn replace(new: &Path, exe: &Path) -> std::io::Result<()> {
    let old = exe.with_file_name(OLD);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)?;
    std::fs::rename(new, exe).inspect_err(|_| {
        let _ = std::fs::rename(&old, exe);
    })
}

/// At start-up and every `EVERY` after, unless `auto_update = false` in the
/// config. Quiet on errors: the next round tries again.
pub fn background(tx: Sender<Event>) {
    let dev = dev();
    if (matches!(mode(), Mode::Off) && !dev) || !crate::config::auto_update() {
        return;
    }
    std::thread::spawn(move || loop {
        let found = match latest() {
            Ok((ver, assets)) if dev || newer(&ver, VERSION) => Some((ver, assets)),
            // No release to show: a made-up one, so the button still shows.
            Err(_) if dev => Some((format!("{VERSION}-dev"), vec![])),
            _ => None,
        };
        if let Some((ver, assets)) = found {
            if tx.send(Event::Update(Note::Found { ver, assets })).is_err() {
                return;
            }
        }
        std::thread::sleep(EVERY);
    });
}

/// Installs `ver` for a click on the button; it blocks, so it runs in a thread.
pub fn install_now(ver: String, assets: Assets) -> Event {
    Event::Update(Note::Installed(install(&ver, &assets, &|_| {}).map(|()| ver)))
}

/// `slick update`: checks now and installs, telling what it does.
pub fn cli() -> i32 {
    let path = exe().map(|p| p.display().to_string()).unwrap_or_default();
    if matches!(mode(), Mode::Off) {
        eprintln!("{path}: build de desenvolvimento, não se atualiza");
        return 1;
    }
    eprintln!("slick {VERSION} · procurando versão nova…");
    let (ver, assets) = match latest() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("não consultou as releases: {e}");
            return 1;
        }
    };
    if !newer(&ver, VERSION) {
        eprintln!("já é a mais recente");
        return 0;
    }
    if matches!(mode(), Mode::Managed) {
        eprintln!("slick {ver} disponível; {path} é de outra ferramenta (ou a pasta não é gravável): atualize por ela");
        return 1;
    }
    match install(&ver, &assets, &|s| eprintln!("{s}")) {
        Ok(()) => {
            eprintln!("atualizado para {ver}");
            0
        }
        Err(e) => {
            eprintln!("não atualizou: {e}");
            1
        }
    }
}

/// Runs the new binary in this process's place, with the same arguments.
/// Returns only when that fails.
pub fn restart() -> std::io::Error {
    let Some(exe) = exe() else { return std::io::Error::other("não achou o executável") };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(std::env::args_os().skip(1));
    #[cfg(unix)]
    return std::os::unix::process::CommandExt::exec(&mut cmd);
    // No exec on Windows: the new one runs as a child on the same console,
    // and this one waits to exit with its code, so the shell waits too.
    #[cfg(not(unix))]
    match cmd.status() {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => e,
    }
}
