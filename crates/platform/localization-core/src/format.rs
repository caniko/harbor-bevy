use icu_locale::Locale;
use icu_plurals::{PluralCategory, PluralOperands, PluralRules};

use crate::LocaleId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatArg<'a> {
    Text(&'a str),
    Number(i64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    MissingMessage {
        id: crate::MessageId,
        locale: LocaleId,
    },
    MissingArg(String),
    InvalidPattern,
    PluralData,
}

pub fn format_pattern(
    pattern: &str,
    locale: LocaleId,
    args: &[(&str, FormatArg<'_>)],
) -> Result<String, FormatError> {
    format_pattern_locale(pattern, &locale.icu_locale(), args)
}

pub(crate) fn format_pattern_locale(
    pattern: &str,
    locale: &Locale,
    args: &[(&str, FormatArg<'_>)],
) -> Result<String, FormatError> {
    format_body(pattern, locale, args, None)
}

fn format_body(
    pattern: &str,
    locale: &Locale,
    args: &[(&str, FormatArg<'_>)],
    hash: Option<i64>,
) -> Result<String, FormatError> {
    let mut out = String::new();
    let mut i = 0;
    while i < pattern.len() {
        let ch = pattern[i..]
            .chars()
            .next()
            .unwrap_or_else(|| unreachable!());
        match ch {
            '#' if let Some(n) = hash => {
                out.push_str(&n.to_string());
                i += 1;
            }
            '{' => {
                i += 1;
                let (chunk, next) = format_placeholder(pattern, i, locale, args, hash)?;
                out.push_str(&chunk);
                i = next;
            }
            '}' => return Err(FormatError::InvalidPattern),
            _ => {
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    Ok(out)
}

fn format_placeholder(
    pattern: &str,
    mut i: usize,
    locale: &Locale,
    args: &[(&str, FormatArg<'_>)],
    hash: Option<i64>,
) -> Result<(String, usize), FormatError> {
    skip_ws(pattern, &mut i);
    let name = take_ident(pattern, &mut i)?;
    skip_ws(pattern, &mut i);
    if pattern[i..].starts_with('}') {
        i += 1;
        return Ok((render_arg(arg(args, name)?)?, i));
    }
    expect(pattern, &mut i, ',')?;
    skip_ws(pattern, &mut i);
    let kind = take_ident(pattern, &mut i)?;
    skip_ws(pattern, &mut i);
    expect(pattern, &mut i, ',')?;
    match kind {
        "plural" => format_plural(pattern, i, locale, args, name),
        "select" => format_select(pattern, i, locale, args, name, hash),
        _ => Err(FormatError::InvalidPattern),
    }
}

fn format_plural(
    pattern: &str,
    mut i: usize,
    locale: &Locale,
    args: &[(&str, FormatArg<'_>)],
    name: &str,
) -> Result<(String, usize), FormatError> {
    let n = match arg(args, name)? {
        FormatArg::Number(n) => n,
        FormatArg::Text(_) => return Err(FormatError::InvalidPattern),
    };
    let category = plural_category(locale, n)?;
    let mut exact = None;
    let mut keyword = None;
    let mut other = None;
    loop {
        skip_ws(pattern, &mut i);
        if pattern[i..].starts_with('}') {
            i += 1;
            break;
        }
        let key = take_branch_key(pattern, &mut i)?;
        skip_ws(pattern, &mut i);
        expect(pattern, &mut i, '{')?;
        let start = i;
        i = skip_balanced(pattern, i)?;
        let body = &pattern[start..i.saturating_sub(1)];
        if key == "other" {
            other = Some(body);
        } else if let Some(value) = key.strip_prefix('=') {
            if exact.is_none() && value.parse::<i64>().ok() == Some(n) {
                exact = Some(body);
            }
        } else if keyword.is_none() && matches_plural_keyword(key, category) {
            keyword = Some(body);
        }
    }
    let other = other.ok_or(FormatError::InvalidPattern)?;
    let body = exact.or(keyword).unwrap_or(other);
    Ok((format_body(body, locale, args, Some(n))?, i))
}

fn format_select(
    pattern: &str,
    mut i: usize,
    locale: &Locale,
    args: &[(&str, FormatArg<'_>)],
    name: &str,
    hash: Option<i64>,
) -> Result<(String, usize), FormatError> {
    let value = match arg(args, name)? {
        FormatArg::Text(s) => s,
        FormatArg::Number(_) => return Err(FormatError::InvalidPattern),
    };
    let mut chosen = None;
    let mut other = None;
    loop {
        skip_ws(pattern, &mut i);
        if pattern[i..].starts_with('}') {
            i += 1;
            break;
        }
        let key = take_ident(pattern, &mut i)?;
        skip_ws(pattern, &mut i);
        expect(pattern, &mut i, '{')?;
        let start = i;
        i = skip_balanced(pattern, i)?;
        let body = &pattern[start..i.saturating_sub(1)];
        if key == "other" {
            other = Some(body);
        } else if chosen.is_none() && key == value {
            chosen = Some(body);
        }
    }
    let other = other.ok_or(FormatError::InvalidPattern)?;
    let body = chosen.unwrap_or(other);
    Ok((format_body(body, locale, args, hash)?, i))
}

fn matches_plural_keyword(key: &str, category: PluralCategory) -> bool {
    matches!(
        (key, category),
        ("zero", PluralCategory::Zero)
            | ("one", PluralCategory::One)
            | ("two", PluralCategory::Two)
            | ("few", PluralCategory::Few)
            | ("many", PluralCategory::Many)
            | ("other", PluralCategory::Other)
    )
}

fn plural_category(locale: &Locale, n: i64) -> Result<PluralCategory, FormatError> {
    let rules =
        PluralRules::try_new_cardinal(locale.into()).map_err(|_| FormatError::PluralData)?;
    Ok(rules.category_for(PluralOperands::from(n)))
}

fn arg<'a>(args: &[(&str, FormatArg<'a>)], name: &str) -> Result<FormatArg<'a>, FormatError> {
    args.iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| *v)
        .ok_or_else(|| FormatError::MissingArg(name.to_string()))
}

fn render_arg(arg: FormatArg<'_>) -> Result<String, FormatError> {
    Ok(match arg {
        FormatArg::Text(s) => s.to_string(),
        FormatArg::Number(n) => n.to_string(),
    })
}

fn skip_ws(s: &str, i: &mut usize) {
    while let Some(c) = s[*i..].chars().next() {
        if !c.is_whitespace() {
            break;
        }
        *i += c.len_utf8();
    }
}

fn expect(s: &str, i: &mut usize, ch: char) -> Result<(), FormatError> {
    if s[*i..].starts_with(ch) {
        *i += ch.len_utf8();
        Ok(())
    } else {
        Err(FormatError::InvalidPattern)
    }
}

fn take_ident<'a>(s: &'a str, i: &mut usize) -> Result<&'a str, FormatError> {
    let start = *i;
    while let Some(c) = s[*i..].chars().next() {
        if c.is_ascii_alphanumeric() || c == '_' {
            *i += c.len_utf8();
        } else {
            break;
        }
    }
    if *i == start {
        return Err(FormatError::InvalidPattern);
    }
    Ok(&s[start..*i])
}

fn take_branch_key<'a>(s: &'a str, i: &mut usize) -> Result<&'a str, FormatError> {
    if s[*i..].starts_with('=') {
        let start = *i;
        *i += 1;
        take_ident(s, i)?;
        Ok(&s[start..*i])
    } else {
        take_ident(s, i)
    }
}

fn skip_balanced(s: &str, mut i: usize) -> Result<usize, FormatError> {
    let mut depth = 1;
    while i < s.len() {
        let ch = s[i..].chars().next().unwrap_or_else(|| unreachable!());
        i += ch.len_utf8();
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }
    Err(FormatError::InvalidPattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_and_plural_and_select() {
        let args = [
            ("name", FormatArg::Text("Ada")),
            ("count", FormatArg::Number(1)),
        ];
        assert_eq!(
            format_pattern("Hello {name}!", LocaleId::En, &args).unwrap(),
            "Hello Ada!"
        );
        assert_eq!(
            format_pattern(
                "{count, plural, one {# item} other {# items}}",
                LocaleId::En,
                &args
            )
            .unwrap(),
            "1 item"
        );
        let two = [("count", FormatArg::Number(2))];
        assert_eq!(
            format_pattern(
                "{count, plural, one {# item} other {# items}}",
                LocaleId::En,
                &two
            )
            .unwrap(),
            "2 items"
        );
        let pl = [("count", FormatArg::Number(2))];
        assert_eq!(
            format_pattern(
                "{count, plural, one {# a} few {# b} many {# c} other {# d}}",
                LocaleId::Pl,
                &pl
            )
            .unwrap(),
            "2 b"
        );
        let sel = [("g", FormatArg::Text("female"))];
        assert_eq!(
            format_pattern(
                "{g, select, male {he} female {she} other {they}}",
                LocaleId::En,
                &sel
            )
            .unwrap(),
            "she"
        );
    }

    #[test]
    fn exact_plural_selector_beats_keyword_regardless_of_order() {
        let one = [("count", FormatArg::Number(1))];
        let keyword_first = "{count, plural, other {# items} one {# item} =1 {exactly one}}";
        let exact_first = "{count, plural, =1 {exactly one} one {# item} other {# items}}";
        assert_eq!(
            format_pattern(keyword_first, LocaleId::En, &one).unwrap(),
            "exactly one"
        );
        assert_eq!(
            format_pattern(exact_first, LocaleId::En, &one).unwrap(),
            "exactly one"
        );
    }

    #[test]
    fn plural_and_select_require_other() {
        let one = [("count", FormatArg::Number(1))];
        assert_eq!(
            format_pattern("{count, plural, one {# item}}", LocaleId::En, &one),
            Err(FormatError::InvalidPattern)
        );
        let sel = [("g", FormatArg::Text("female"))];
        assert_eq!(
            format_pattern("{g, select, female {she}}", LocaleId::En, &sel),
            Err(FormatError::InvalidPattern)
        );
    }

    #[test]
    fn nested_select_keeps_outer_plural_hash() {
        let args = [
            ("count", FormatArg::Number(1)),
            ("g", FormatArg::Text("female")),
        ];
        assert_eq!(
            format_pattern(
                "{count, plural, one {{g, select, female {# woman} other {# man}}} other {# people}}",
                LocaleId::En,
                &args
            )
            .unwrap(),
            "1 woman"
        );
    }
}
