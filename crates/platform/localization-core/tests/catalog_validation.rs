use localization_core::{
    Catalog, CatalogError, CatalogMode, FormatArg, LocaleId, MessageId, fallback_tags,
    validate_catalog,
};

const ITEMS: MessageId = MessageId("fixture.items");
const THANKS: MessageId = MessageId("fixture.thanks");
const IDS: [MessageId; 2] = [THANKS, ITEMS];
const ITEMS_PATTERN: &str = "{count, plural, one {# item} other {# items}}";

fn complete_catalog() -> Catalog {
    let mut catalog = Catalog::new(IDS);
    for locale in LocaleId::ALL {
        catalog.insert(THANKS, locale, "Thanks!");
        catalog.insert(ITEMS, locale, ITEMS_PATTERN);
    }
    catalog
}

#[test]
fn ten_locales_times_fixture_ids_pass_strict() {
    validate_catalog(&complete_catalog(), CatalogMode::Strict).unwrap();
}

#[test]
fn missing_ko_fails_strict() {
    let catalog = strip_locale(LocaleId::Ko);
    let errors = validate_catalog(&catalog, CatalogMode::Strict).unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        CatalogError::MissingLocale(LocaleId::Ko)
            | CatalogError::MissingId {
                locale: LocaleId::Ko,
                ..
            }
    )));
}

#[test]
fn missing_thanks_fails_strict() {
    let mut catalog = Catalog::new(IDS);
    for locale in LocaleId::ALL {
        catalog.insert(ITEMS, locale, ITEMS_PATTERN);
    }
    let errors = validate_catalog(&catalog, CatalogMode::Strict).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, CatalogError::MissingId { id, .. } if *id == THANKS))
    );
}

#[test]
fn unknown_id_fails() {
    let mut catalog = complete_catalog();
    catalog.insert(MessageId("fixture.unknown"), LocaleId::En, "nope");
    let errors = validate_catalog(&catalog, CatalogMode::Strict).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, CatalogError::UnknownId(id) if id.as_str() == "fixture.unknown"))
    );
}

#[test]
fn empty_string_fails() {
    let mut catalog = complete_catalog();
    catalog.insert(THANKS, LocaleId::En, "");
    let errors = validate_catalog(&catalog, CatalogMode::Strict).unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        CatalogError::EmptyString { id, tag } if *id == THANKS && tag == "en"
    )));
}

#[test]
fn empty_parent_locale_string_fails() {
    let mut catalog = complete_catalog();
    catalog.insert_tag(THANKS, "pt", "").unwrap();
    let errors = validate_catalog(&catalog, CatalogMode::Strict).unwrap_err();
    assert!(errors.iter().any(|error| matches!(
        error,
        CatalogError::EmptyString { id, tag } if *id == THANKS && tag == "pt"
    )));
}

#[test]
fn release_mode_is_strict_not_dev_fallback() {
    assert_eq!(CatalogMode::release(), CatalogMode::Strict);
    assert_ne!(CatalogMode::release(), CatalogMode::DevEnglishFallback);
}

#[test]
fn dev_english_fallback_formats_missing_locale_from_en() {
    let mut catalog = Catalog::new(IDS);
    catalog.insert(THANKS, LocaleId::En, "Thanks!");
    catalog.insert(ITEMS, LocaleId::En, ITEMS_PATTERN);
    validate_catalog(&catalog, CatalogMode::DevEnglishFallback).unwrap();
    assert!(validate_catalog(&catalog, CatalogMode::Strict).is_err());
    let text = catalog
        .format(THANKS, LocaleId::De, &[], CatalogMode::DevEnglishFallback)
        .unwrap();
    assert_eq!(text, "Thanks!");
    assert!(
        catalog
            .format(THANKS, LocaleId::De, &[], CatalogMode::Strict)
            .is_err()
    );
}

#[test]
fn icu_parse_and_fallback_and_format() {
    assert_eq!(LocaleId::parse("pt-BR").unwrap(), LocaleId::PtBr);
    assert_eq!(LocaleId::parse("zh-Hans").unwrap(), LocaleId::ZhHans);
    assert_eq!(
        LocaleId::parse("pt"),
        Err(localization_core::LocaleError::Unsupported)
    );
    assert_eq!(
        LocaleId::parse("zh"),
        Err(localization_core::LocaleError::Unsupported)
    );
    let pt = fallback_tags("pt-BR").unwrap();
    assert!(pt.iter().any(|t| t == "pt-BR"), "{pt:?}");
    assert!(pt.iter().any(|t| t == "pt"), "{pt:?}");
    assert!(pt.iter().any(|t| t == "en"), "{pt:?}");
    let zh = fallback_tags("zh-Hans").unwrap();
    assert!(zh.iter().any(|t| t == "zh-Hans"), "{zh:?}");
    assert!(zh.iter().any(|t| t == "zh"), "{zh:?}");
    assert!(zh.iter().any(|t| t == "en"), "{zh:?}");
    let catalog = complete_catalog();
    let items = catalog
        .format(
            ITEMS,
            LocaleId::En,
            &[("count", FormatArg::Number(1))],
            CatalogMode::Strict,
        )
        .unwrap();
    assert_eq!(items, "1 item");
}

#[test]
fn parent_locale_patterns_resolve_without_becoming_release_locales() {
    let mut catalog = Catalog::new(IDS);
    catalog
        .insert_tag(ITEMS, "pt", "{count, plural, one {# item} other {# itens}}")
        .unwrap();
    catalog
        .insert_tag(ITEMS, "zh", "{count, plural, other {# 个}}")
        .unwrap();
    assert_eq!(
        catalog
            .format(
                ITEMS,
                LocaleId::PtBr,
                &[("count", FormatArg::Number(2))],
                CatalogMode::Strict,
            )
            .unwrap(),
        "2 itens"
    );
    assert_eq!(
        catalog
            .format(
                ITEMS,
                LocaleId::ZhHans,
                &[("count", FormatArg::Number(2))],
                CatalogMode::Strict,
            )
            .unwrap(),
        "2 个"
    );
    assert_eq!(LocaleId::ALL.len(), 10);
    assert!(LocaleId::parse("pt").is_err());
    assert!(LocaleId::parse("zh").is_err());
}

#[test]
fn english_fallback_uses_english_plural_rules() {
    let mut catalog = Catalog::new(IDS);
    catalog.insert(
        ITEMS,
        LocaleId::En,
        "{count, plural, one {# item} few {BAD} other {# items}}",
    );
    let text = catalog
        .format(
            ITEMS,
            LocaleId::Pl,
            &[("count", FormatArg::Number(2))],
            CatalogMode::DevEnglishFallback,
        )
        .unwrap();
    assert_eq!(text, "2 items");
}

fn strip_locale(drop: LocaleId) -> Catalog {
    let mut catalog = Catalog::new(IDS);
    for locale in LocaleId::ALL {
        if locale == drop {
            continue;
        }
        catalog.insert(THANKS, locale, "Thanks!");
        catalog.insert(ITEMS, locale, ITEMS_PATTERN);
    }
    catalog
}
