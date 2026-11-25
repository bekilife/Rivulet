//! Interface language: English or Russian, by default the one Windows uses.

use std::sync::atomic::{AtomicBool, Ordering};

static EN: AtomicBool = AtomicBool::new(true);

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    System,
    En,
    Ru,
}

/// True when the interface is in English.
pub fn en() -> bool {
    EN.load(Ordering::Relaxed)
}

pub fn set(lang: Lang) {
    let en = match lang {
        Lang::En => true,
        Lang::Ru => false,
        Lang::System => !system_reads_russian(),
    };
    EN.store(en, Ordering::Relaxed);
}

/// Russian for Windows set to Russian or to a language whose speakers
/// commonly read it; English otherwise.
fn system_reads_russian() -> bool {
    let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    // Primary language ids: Russian, Ukrainian, Belarusian, Kazakh, Kyrgyz.
    matches!(id & 0x3FF, 0x19 | 0x22 | 0x23 | 0x3F | 0x40)
}

/// Picks the string for the current language.
pub fn tt(ru: &'static str, en: &'static str) -> &'static str {
    if self::en() { en } else { ru }
}

/// `format!` in the current language: `tr!("Russian {}", "English {}", args)`.
macro_rules! tr {
    ($ru:literal, $en:literal $(, $a:expr)* $(,)?) => {
        if $crate::lang::en() { format!($en $(, $a)*) } else { format!($ru $(, $a)*) }
    };
}
