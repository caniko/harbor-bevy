//! Bevy-independent localization core.

mod catalog;
mod format;

pub use catalog::{Catalog, CatalogError, CatalogMode, validate_catalog};
pub use format::{FormatArg, FormatError, format_pattern};

use icu_locale::Locale;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageId(pub &'static str);

impl MessageId {
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LocaleId {
    En,
    De,
    Fr,
    Es,
    PtBr,
    Pl,
    Ru,
    ZhHans,
    Ja,
    Ko,
}

impl LocaleId {
    pub const ALL: [Self; 10] = [
        Self::En,
        Self::De,
        Self::Fr,
        Self::Es,
        Self::PtBr,
        Self::Pl,
        Self::Ru,
        Self::ZhHans,
        Self::Ja,
        Self::Ko,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::De => "de",
            Self::Fr => "fr",
            Self::Es => "es",
            Self::PtBr => "pt-BR",
            Self::Pl => "pl",
            Self::Ru => "ru",
            Self::ZhHans => "zh-Hans",
            Self::Ja => "ja",
            Self::Ko => "ko",
        }
    }

    pub fn parse(input: &str) -> Result<Self, LocaleError> {
        let locale: Locale = input.parse().map_err(|_| LocaleError::Invalid)?;
        Self::ALL
            .into_iter()
            .find(|id| id.icu_locale() == locale)
            .ok_or(LocaleError::Unsupported)
    }

    pub fn icu_locale(self) -> Locale {
        self.as_str()
            .parse()
            .unwrap_or_else(|_| unreachable!("LocaleId tags are valid BCP-47"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocaleError {
    Invalid,
    Unsupported,
}

pub fn fallback_tags(input: &str) -> Result<Vec<String>, LocaleError> {
    catalog::fallback_tags(input)
}
