//! Lexical C++ type spellings shared by parsing, lowering and signature matching.

/// A keyword scalar, `auto`, or a standard integer / `nullptr_t` name: never
/// a class, so never qualified to the namespace it is spelled in.
pub fn is_fundamental_type_name(name: &str) -> bool {
    !name.is_empty()
        && name.split_whitespace().all(|word| {
            matches!(
                word,
                "int"
                    | "char"
                    | "void"
                    | "bool"
                    | "float"
                    | "double"
                    | "short"
                    | "long"
                    | "signed"
                    | "unsigned"
                    | "wchar_t"
                    | "char8_t"
                    | "char16_t"
                    | "char32_t"
                    | "nullptr_t"
                    | "auto"
            ) || (!word.starts_with("std::") && is_standard_integer_type_name(word))
        })
}

/// Standard integer typedefs whose underlying width/sign are unknown without
/// a declaration and target ABI. Character types and nullptr_t are excluded.
pub fn is_standard_integer_type_name(name: &str) -> bool {
    matches!(
        name.strip_prefix("std::").unwrap_or(name),
        "size_t"
            | "ssize_t"
            | "ptrdiff_t"
            | "intptr_t"
            | "uintptr_t"
            | "int8_t"
            | "int16_t"
            | "int32_t"
            | "int64_t"
            | "uint8_t"
            | "uint16_t"
            | "uint32_t"
            | "uint64_t"
            | "intmax_t"
            | "uintmax_t"
    )
}

/// A template argument split into what names the type and the pointer /
/// reference punctuation after it, every level kept and cv-qualifiers at any
/// level dropped: `T * const *` is (`T`, `**`), `T &` is (`T`, `&`).
pub fn split_pointer_suffix(arg: &str) -> (&str, String) {
    let mut rest = arg.trim();
    let mut suffix = String::new();
    loop {
        let bare = strip_trailing_cv(rest).trim_end();
        match bare.strip_suffix(['*', '&']) {
            Some(shorter) => {
                suffix.insert(0, bare.chars().next_back().unwrap_or('*'));
                rest = shorter;
            }
            None => return (bare, suffix),
        }
    }
}

/// A non-type template argument spelled as a literal (`4`, `-1`, `true`,
/// `'a'`): never a class, so never qualified to a namespace.
pub fn is_template_literal(arg: &str) -> bool {
    arg.chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, '\'' | '"' | '-'))
        || matches!(arg, "true" | "false" | "nullptr")
}

/// `T const` / `T volatile` spelled after the type, also directly after
/// pointer or reference punctuation (`T*const`).
pub fn strip_trailing_cv(mut s: &str) -> &str {
    loop {
        let t = s.trim_end();
        match t
            .strip_suffix("const")
            .or_else(|| t.strip_suffix("volatile"))
        {
            Some(rest) if rest.ends_with(|c: char| c.is_whitespace() || matches!(c, '*' | '&')) => {
                s = rest;
            }
            _ => return t,
        }
    }
}

/// Portion of a function spelling that can contain its enclosing scope.
/// A conversion operator's target type may itself contain `::`.
pub fn function_scope_spelling(name: &str) -> &str {
    let mut from = 0;
    while let Some(rel) = name[from..].find("operator") {
        let at = from + rel;
        from = at + "operator".len();
        // `operators::f` merely starts with the keyword; `operator ns::S`,
        // `operator=` and `operator new` are the real thing.
        let continues_identifier = name[from..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if (at == 0 || name[..at].ends_with("::")) && !continues_identifier {
            return &name[..at];
        }
    }
    name
}

/// Supported integral non-type arguments, independent of spelling and suffix.
/// Unsupported constant expressions remain unresolved by callers.
pub fn integral_template_value(spelling: &str) -> Option<i128> {
    let spelling = spelling.trim();
    // Apostrophes separate digits only inside numeric tokens. In particular,
    // `'1'` is a character value, not the integer 1; its value depends on the
    // execution character set, which the index does not model.
    let unsigned = spelling.strip_prefix(['-', '+']).unwrap_or(spelling);
    if !unsigned.starts_with(|c: char| c.is_ascii_digit()) {
        return match spelling {
            "true" => Some(1),
            "false" => Some(0),
            _ => None,
        };
    }
    let text = spelling.replace('\'', "");
    let (negative, text) = text
        .strip_prefix('-')
        .map_or((false, text.as_str()), |s| (true, s));
    let text = text
        .strip_prefix('+')
        .unwrap_or(text)
        .trim_end_matches(['u', 'U', 'l', 'L']);
    let (digits, radix) =
        if let Some(s) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (s, 16)
        } else if let Some(s) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
            (s, 2)
        } else if text.len() > 1 && text.starts_with('0') {
            (&text[1..], 8)
        } else {
            (text, 10)
        };
    let value = i128::from_str_radix(digits, radix).ok()?;
    if negative {
        value.checked_neg()
    } else {
        Some(value)
    }
}

/// Character-valued template arguments whose execution character set is not
/// modeled. Keep them distinct from numeric spellings before comparing values.
pub(crate) fn is_character_template_literal(spelling: &str) -> bool {
    let spelling = spelling.trim();
    let body = ["u8", "u", "U", "L"]
        .into_iter()
        .find_map(|prefix| spelling.strip_prefix(prefix))
        .unwrap_or(spelling);
    body.starts_with('\'') && body.ends_with('\'') && body.len() > 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_separators_do_not_turn_characters_into_integers() {
        for spelling in ["'1'", "'a'", "'\\n'", "L'1'", "u8'1'"] {
            assert_eq!(integral_template_value(spelling), None, "{spelling}");
            assert!(is_character_template_literal(spelling), "{spelling}");
        }
        assert_eq!(integral_template_value("1'000u"), Some(1000));
        assert_eq!(integral_template_value("-1'000"), Some(-1000));
    }
}
