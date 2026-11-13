//! File-local lookup syntax replayed when a header's declarations are shared.
use super::*;
use std::ops::Range;

#[derive(Debug, Clone)]
pub struct ConsumerContext {
    pub text: Arc<str>,
    pub line_map: Arc<LineMap>,
}

struct Scope {
    head: Range<usize>,
    tail: Option<Range<usize>>,
}

struct Using {
    start: usize,
    depth: u32,
    scopes: Option<Vec<usize>>,
}

/// Preserve using declarations and their original namespace/linkage blocks.
/// Unknown scopes retain the inline fallback rather than guessing ownership.
#[cfg(test)]
pub(super) fn extract(text: &str, map: &LineMap) -> Option<ConsumerContext> {
    scan(text, map).context
}

pub(super) struct HeaderContext {
    pub context: Option<ConsumerContext>,
    pub static_data: bool,
}

pub(super) fn scan(text: &str, map: &LineMap) -> HeaderContext {
    let mut static_data = false;
    let context = extract_inner(text, map, &mut static_data);
    // An unclassified scope cannot prove that a static declaration is local.
    static_data |= context.is_none() && text.contains("static");
    HeaderContext {
        context,
        static_data,
    }
}

fn extract_inner(text: &str, map: &LineMap, static_data: &mut bool) -> Option<ConsumerContext> {
    let starts: Vec<_> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let tokens = Lexer::new(text, Language::Cpp).tokenize();
    let offset = |token: &Token| {
        starts
            .get(token.line as usize - 1)
            .copied()
            .map(|line| line + token.col as usize - 1)
    };
    let mut scope = OutputScope::default();
    let mut delimiters: Vec<Option<usize>> = Vec::new();
    let mut scopes: Vec<Scope> = Vec::new();
    let mut needed = FxHashSet::default();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut namespace = None;
    let mut namespace_alias: Option<Using> = None;
    let mut using: Option<Using> = None;
    let mut previous = [None::<usize>; 2];
    let mut pending_static: Option<(u32, bool, bool)> = None;
    for (i, token) in tokens.iter().enumerate() {
        if matches!(token.kind, TokenKind::Newline | TokenKind::Eof) {
            continue;
        }
        if matches!(token.kind, TokenKind::Hash) {
            return None;
        }
        let at = offset(token)?;
        let depth = scope.depth;
        let enclosing = || delimiters.iter().copied().collect::<Option<Vec<_>>>();
        if token.is_ident("static") && scope.class_scopes == 0 && enclosing().is_some() {
            pending_static = Some((depth, false, false));
        }
        if let Some((level, parentheses, uncertain)) = pending_static.as_mut() {
            if matches!(&token.kind, TokenKind::Identifier(name) if matches!(name.as_str(), "__attribute__" | "__declspec" | "alignas"))
            {
                *uncertain = true;
            }
            if depth == *level {
                match &token.kind {
                    TokenKind::Punct("(") => *parentheses = true,
                    TokenKind::Punct("=") => *uncertain = true,
                    TokenKind::Punct("{") | TokenKind::Punct("<%") => {
                        *static_data |= !*parentheses || *uncertain;
                        pending_static = None;
                    }
                    TokenKind::Punct(";") => {
                        *static_data = true;
                        pending_static = None;
                    }
                    _ => {}
                }
            }
        }
        if token.is_ident("namespace") && enclosing().is_some() {
            let start = previous[0]
                .filter(|&last| tokens[last].is_ident("inline"))
                .and_then(|last| offset(&tokens[last]))
                .unwrap_or(at);
            namespace = Some((depth, start));
        }
        if token.is_ident("using") && scope.class_scopes == 0 {
            using = Some(Using {
                start: at,
                depth,
                scopes: enclosing(),
            });
        }
        let is_import = scope.using_declaration.is_some();
        scope.token(&token.kind);
        if !scope.valid {
            return None;
        }
        if let TokenKind::Punct(punct) = &token.kind {
            match *punct {
                "{" | "<%" | "(" | "[" | "<:" => {
                    if matches!(*punct, "{" | "<%")
                        && namespace.is_some_and(|(d, _)| d == depth)
                        && previous[0].is_none_or(|last| !matches!(&tokens[last].kind, TokenKind::Identifier(name) if name != "namespace"))
                    {
                        *static_data = true;
                    }
                    let linkage = previous[0].zip(previous[1]).and_then(|(last, before)| {
                        (tokens[before].is_ident("extern")
                            && matches!(&tokens[last].kind,
                            TokenKind::String(s) if s == "\"C\"" || s == "\"C++\""))
                        .then(|| offset(&tokens[before]))
                        .flatten()
                    });
                    let head = if matches!(*punct, "{" | "<%") {
                        namespace
                            .filter(|(d, _)| *d == depth)
                            .map(|(_, start)| start)
                            .or(linkage.filter(|_| enclosing().is_some()))
                    } else {
                        None
                    };
                    let id = head.map(|start| {
                        let id = scopes.len();
                        scopes.push(Scope {
                            head: start..at + punct.len(),
                            tail: None,
                        });
                        namespace = None;
                        id
                    });
                    delimiters.push(id);
                }
                "}" | "%>" | ")" | "]" | ":>" => {
                    if let Some(id) = delimiters.pop()? {
                        scopes[id].tail = Some(at..at + punct.len());
                    }
                    if namespace.is_some_and(|(d, _)| d > scope.depth) {
                        namespace = None;
                    }
                }
                ";" => {
                    if let Some(item) = namespace_alias.take() {
                        needed.extend(item.scopes?);
                        ranges.push(item.start..at + 1);
                    }
                    if let Some(item) = using.take().filter(|u| u.depth == depth && is_import) {
                        needed.extend(item.scopes?);
                        ranges.push(item.start..at + 1);
                    }
                    if namespace.is_some_and(|(d, _)| d == depth) {
                        namespace = None;
                    }
                }
                "=" => {
                    if let Some((_, start)) = namespace.take().filter(|(d, _)| *d == depth) {
                        namespace_alias = Some(Using {
                            start,
                            depth,
                            scopes: enclosing(),
                        });
                    }
                }
                _ => {}
            }
        }
        if scope.using_declaration.is_none() {
            using = None;
        }
        previous = [Some(i), previous[0]];
    }
    if scope.depth != 0 || !scope.at_boundary() {
        return None;
    }
    for id in needed {
        ranges.push(scopes[id].head.clone());
        ranges.push(scopes[id].tail.clone()?);
    }
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut selected: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = selected.last_mut().filter(|last| last.end >= range.start) {
            last.end = last.end.max(range.end);
        } else {
            selected.push(range);
        }
    }
    let mut output = String::new();
    let mut line_map = LineMap::new();
    for range in selected {
        let at = output.len();
        output.push_str(&text[range.clone()]);
        line_map.splice_range(map, range, at);
        output.push('\n');
    }
    Some(ConsumerContext {
        text: output.into(),
        line_map: Arc::new(line_map),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_static_storage_is_distinguished_from_functions_and_class_members() {
        for (text, expected) in [
            ("namespace N { static void F() {} using X::f; }", false),
            ("struct C { static int value; };", false),
            ("namespace N { static Handler slot = Callback; }", true),
            ("void F() { static int value; };", false),
            ("static T value __attribute__((used)) {};", true),
            ("namespace { Handler slot = Callback; }", true),
        ] {
            assert_eq!(scan(text, &LineMap::new()).static_data, expected, "{text}");
        }
    }
    #[test]
    fn projection_keeps_namespace_grouping_and_excludes_class_members() {
        let text = "namespace A { namespace B { struct C { using Base::f; }; using X::f; using namespace Y; } } using Q::g;";
        let context = extract(text, &LineMap::new()).unwrap();
        let compact = context.text.split_whitespace().collect::<String>();
        assert_eq!(
            compact,
            "namespaceA{namespaceB{usingX::f;usingnamespaceY;}}usingQ::g;"
        );
    }
    #[test]
    fn unclassified_function_scope_and_unbalanced_input_keep_the_fallback() {
        assert!(extract("void F() { using X::f; };", &LineMap::new()).is_none());
        assert!(extract("namespace N { using X::f;", &LineMap::new()).is_none());
    }

    #[test]
    fn projection_keeps_aliases_used_by_imports_and_inline_namespace_ownership() {
        let text = "inline namespace N { namespace Short = SDK; using Short::Make; }";
        let context = extract(text, &LineMap::new()).unwrap();
        assert_eq!(
            context.text.split_whitespace().collect::<String>(),
            "inlinenamespaceN{namespaceShort=SDK;usingShort::Make;}"
        );
    }
}
