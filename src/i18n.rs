//! The interface language: en-US by default, pt-BR as an option. Picked by
//! `SLICK_LANG`, else `language` in the config, and switchable at run time
//! from the account menu.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Pt,
}

static LANG: AtomicU8 = AtomicU8::new(0);

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Pt];

    /// `en-US`, `en`, `pt-BR`, `pt_BR.UTF-8`, `pt`…
    pub fn parse(s: &str) -> Option<Lang> {
        let s = s.trim().to_ascii_lowercase();
        if s.starts_with("pt") {
            Some(Lang::Pt)
        } else if s.starts_with("en") {
            Some(Lang::En)
        } else {
            None
        }
    }

    /// What the config stores.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en-US",
            Lang::Pt => "pt-BR",
        }
    }

    /// Next to "Language" in the account menu.
    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Pt => "Português",
        }
    }

    /// In the language submenu.
    pub fn label(self) -> &'static str {
        match self {
            Lang::En => "English (US)",
            Lang::Pt => "Português (Brasil)",
        }
    }
}

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 1 { Lang::Pt } else { Lang::En }
}

pub fn set(l: Lang) {
    LANG.store(l as u8, Ordering::Relaxed);
}

pub fn is_pt() -> bool {
    lang() == Lang::Pt
}

/// At start-up: `SLICK_LANG`, else the config, else English.
pub fn init() {
    let env = std::env::var("SLICK_LANG").ok().and_then(|v| Lang::parse(&v));
    set(env.or_else(crate::config::language).unwrap_or(Lang::En));
}

/// The text in the current language.
pub fn tr<'a>(en: &'a str, pt: &'a str) -> &'a str {
    if is_pt() { pt } else { en }
}

/// `format!` with a template per language: `trf!("{n} new", "{n} novas")`.
macro_rules! trf {
    ($en:literal, $pt:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_pt() { format!($pt $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}

// ---- dates -----------------------------------------------------------------

const WEEKDAYS_EN: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const WEEKDAYS_PT: [&str; 7] = ["segunda-feira", "terça-feira", "quarta-feira", "quinta-feira", "sexta-feira", "sábado", "domingo"];
const MONTHS_EN: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
];
const MONTHS_PT: [&str; 12] = [
    "janeiro", "fevereiro", "março", "abril", "maio", "junho", "julho", "agosto", "setembro", "outubro", "novembro", "dezembro",
];

/// The time as Slack shows it: "9:29 PM", or "21h29" in Portuguese.
pub fn clock<T: chrono::Timelike>(t: &T) -> String {
    match lang() {
        Lang::Pt => format!("{:02}h{:02}", t.hour(), t.minute()),
        Lang::En => {
            let (pm, h) = t.hour12();
            format!("{h}:{:02} {}", t.minute(), if pm { "PM" } else { "AM" })
        }
    }
}

pub fn weekday<T: chrono::Datelike>(t: &T) -> &'static str {
    let i = t.weekday().num_days_from_monday() as usize;
    if is_pt() { WEEKDAYS_PT[i] } else { WEEKDAYS_EN[i] }
}

pub fn month<T: chrono::Datelike>(t: &T) -> &'static str {
    let i = t.month0() as usize;
    if is_pt() { MONTHS_PT[i] } else { MONTHS_EN[i] }
}

/// "1st", "2nd", "3rd", "11th", "28th".
pub fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}
