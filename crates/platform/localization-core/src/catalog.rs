use std::collections::{BTreeMap, BTreeSet};

use icu_locale::{Locale, LocaleFallbacker};

use crate::format::{FormatArg, FormatError, format_pattern_locale};
use crate::{LocaleError, LocaleId, MessageId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogMode {
    Strict,
    DevEnglishFallback,
}

impl CatalogMode {
    pub const fn release() -> Self {
        Self::Strict
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogError {
    MissingLocale(LocaleId),
    MissingId { id: MessageId, locale: LocaleId },
    EmptyString { id: MessageId, tag: String },
    UnknownId(MessageId),
}

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    required_ids: BTreeSet<MessageId>,
    patterns: BTreeMap<MessageId, BTreeMap<String, String>>,
}

impl Catalog {
    pub fn new(required_ids: impl IntoIterator<Item = MessageId>) -> Self {
        Self {
            required_ids: required_ids.into_iter().collect(),
            patterns: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, id: MessageId, locale: LocaleId, pattern: impl Into<String>) {
        self.patterns
            .entry(id)
            .or_default()
            .insert(locale.as_str().to_string(), pattern.into());
    }

    pub fn insert_tag(
        &mut self,
        id: MessageId,
        tag: &str,
        pattern: impl Into<String>,
    ) -> Result<(), LocaleError> {
        let locale: Locale = tag.parse().map_err(|_| LocaleError::Invalid)?;
        self.patterns
            .entry(id)
            .or_default()
            .insert(locale.to_string(), pattern.into());
        Ok(())
    }

    pub fn format(
        &self,
        id: MessageId,
        locale: LocaleId,
        args: &[(&str, FormatArg<'_>)],
        mode: CatalogMode,
    ) -> Result<String, FormatError> {
        let (pattern, pattern_locale) = self.resolve(id, locale, mode)?;
        format_pattern_locale(pattern, &pattern_locale, args)
    }

    fn resolve(
        &self,
        id: MessageId,
        locale: LocaleId,
        mode: CatalogMode,
    ) -> Result<(&str, Locale), FormatError> {
        let allow_en = mode == CatalogMode::DevEnglishFallback || locale == LocaleId::En;
        let Some(by_tag) = self.patterns.get(&id) else {
            return Err(FormatError::MissingMessage { id, locale });
        };
        let tags = fallback_tags(locale.as_str()).unwrap_or_else(|_| {
            vec![
                locale.as_str().to_string(),
                LocaleId::En.as_str().to_string(),
            ]
        });
        for tag in tags {
            if is_english_tag(&tag) && !allow_en {
                continue;
            }
            if let Some(pattern) = by_tag.get(&tag) {
                if pattern.is_empty() {
                    continue;
                }
                let pattern_locale: Locale = tag.parse().unwrap_or_else(|_| locale.icu_locale());
                return Ok((pattern.as_str(), pattern_locale));
            }
        }
        Err(FormatError::MissingMessage { id, locale })
    }
}

pub fn validate_catalog(catalog: &Catalog, mode: CatalogMode) -> Result<(), Vec<CatalogError>> {
    let mut errors = Vec::new();
    let mut seen_unknown = BTreeSet::new();
    for (id, by_tag) in &catalog.patterns {
        if !catalog.required_ids.contains(id) && seen_unknown.insert(*id) {
            errors.push(CatalogError::UnknownId(*id));
        }
        for (tag, pattern) in by_tag {
            if pattern.is_empty() {
                errors.push(CatalogError::EmptyString {
                    id: *id,
                    tag: tag.clone(),
                });
            }
        }
    }
    let required_locales: &[LocaleId] = match mode {
        CatalogMode::Strict => &LocaleId::ALL,
        CatalogMode::DevEnglishFallback => &[LocaleId::En],
    };
    for locale in required_locales {
        let mut any = false;
        for id in &catalog.required_ids {
            match catalog
                .patterns
                .get(id)
                .and_then(|by_tag| by_tag.get(locale.as_str()))
            {
                None => errors.push(CatalogError::MissingId {
                    id: *id,
                    locale: *locale,
                }),
                Some(pattern) => {
                    any = true;
                    if pattern.is_empty() {
                        // already recorded
                    }
                }
            }
        }
        if !any && mode == CatalogMode::Strict {
            errors.push(CatalogError::MissingLocale(*locale));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub fn fallback_tags(input: &str) -> Result<Vec<String>, LocaleError> {
    let locale: Locale = input.parse().map_err(|_| LocaleError::Invalid)?;
    let fallbacker = LocaleFallbacker::new();
    let mut iter = fallbacker
        .for_config(Default::default())
        .fallback_for((&locale).into());
    let mut tags = vec![locale.to_string()];
    loop {
        let current = iter.get();
        if current.is_unknown() {
            break;
        }
        let tag = current.to_string();
        if !tags.contains(&tag) {
            tags.push(tag);
        }
        iter.step();
    }
    if tags.is_empty() {
        tags.push(locale.to_string());
    }
    if let Some(parent) = language_parent(&locale.to_string())
        && !tags.iter().any(|t| t == parent)
    {
        match tags.iter().position(|t| t == "en") {
            Some(i) => tags.insert(i, parent.to_string()),
            None => tags.push(parent.to_string()),
        }
    }
    if !tags.iter().any(|t| t == "en") {
        tags.push("en".into());
    }
    Ok(tags)
}

fn language_parent(tag: &str) -> Option<&str> {
    let lang = tag.split(['-', '_']).next()?;
    if lang.is_empty() || lang == tag || lang == "und" {
        None
    } else {
        Some(lang)
    }
}

fn is_english_tag(tag: &str) -> bool {
    tag == "en" || tag.starts_with("en-") || tag.starts_with("en_")
}
