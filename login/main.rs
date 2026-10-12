//! The sign-in window (`src/login_window.rs`) as its own program, for Linux:
//! slick starts it from the cache as `slick-login __login-window [workspace]`.
//! The language comes from `SLICK_LANG`, which slick sets.

#[path = "../src/login_window.rs"]
mod login_window;

mod i18n {
    pub fn tr<'a>(en: &'a str, pt: &'a str) -> &'a str {
        if std::env::var("SLICK_LANG").is_ok_and(|l| l.to_ascii_lowercase().starts_with("pt")) { pt } else { en }
    }
}

mod config {
    use std::path::PathBuf;

    /// slick's data folder, as `crate::config::data_dir` in slick.
    pub fn data_dir() -> PathBuf {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
            .join("slick")
    }
}

fn main() {
    login_window::run()
}
