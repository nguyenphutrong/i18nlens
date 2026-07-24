use tree_sitter::{Node, Parser};

#[derive(Debug)]
pub(super) struct Candidate {
    pub key: String,
    pub start: usize,
    pub end: usize,
}

pub(super) struct Analysis {
    pub candidates: Vec<Candidate>,
    pub claimed_offsets: Vec<usize>,
}

#[derive(Clone)]
struct Binding {
    name: String,
    kind: BindingKind,
    scope_start: usize,
    scope_end: usize,
    depth: usize,
    visible_from: usize,
}

#[derive(Clone)]
enum BindingKind {
    Other,
    Translator(String),
    UnknownTranslator,
}

pub(super) fn analyze(content: &str) -> Analysis {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
        .is_err()
    {
        return Analysis {
            candidates: Vec::new(),
            claimed_offsets: Vec::new(),
        };
    }
    let Some(tree) = parser.parse(content, None) else {
        return Analysis {
            candidates: Vec::new(),
            claimed_offsets: Vec::new(),
        };
    };
    let root = tree.root_node();
    let mut bindings = Vec::new();
    collect_bindings(root, content, &mut bindings);
    let mut analysis = Analysis {
        candidates: Vec::new(),
        claimed_offsets: Vec::new(),
    };
    collect_calls(root, content, &bindings, &mut analysis);
    analysis
}

fn walk(node: Node<'_>, mut visit: impl FnMut(Node<'_>)) {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        visit(node);
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

fn collect_bindings(root: Node<'_>, source: &str, bindings: &mut Vec<Binding>) {
    walk(root, |node| {
        if is_function(node.kind()) {
            if let Some(parameters) = node
                .child_by_field_name("parameters")
                .or_else(|| node.child_by_field_name("parameter"))
            {
                collect_pattern_names(parameters, source, &mut |name| {
                    bindings.push(binding(name, BindingKind::Other, node, node.start_byte()));
                });
            }

            if matches!(
                node.kind(),
                "function_declaration"
                    | "generator_function_declaration"
                    | "function_expression"
                    | "generator_function"
            ) {
                let Some(name) = node.child_by_field_name("name") else {
                    return;
                };
                if let Some(name) = text(name, source) {
                    let scope = if matches!(
                        node.kind(),
                        "function_declaration" | "generator_function_declaration"
                    ) {
                        scope_for(node, false)
                    } else {
                        node
                    };
                    bindings.push(binding(name, BindingKind::Other, scope, scope.start_byte()));
                }
            }
        }

        if node.kind() == "catch_clause" {
            if let Some(parameter) = node.child_by_field_name("parameter") {
                collect_pattern_names(parameter, source, &mut |name| {
                    bindings.push(binding(name, BindingKind::Other, node, node.start_byte()));
                });
            }
        }

        if node.kind() != "variable_declarator" {
            return;
        }
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let mut names = Vec::new();
        collect_pattern_names(name_node, source, &mut |name| names.push(name.to_owned()));
        if names.is_empty() {
            return;
        }
        let translator = node
            .child_by_field_name("value")
            .and_then(|value| translator_binding(value, source));
        let declaration_node = node.parent().unwrap_or(node);
        let is_var = declaration_node.kind() == "variable_declaration";
        let scope = scope_for(node, is_var);
        for name in names {
            // The lexical binding itself shadows outer translators throughout
            // its scope. A recognized translator initializer becomes active
            // only after the declaration has completed.
            bindings.push(binding(
                &name,
                BindingKind::Other,
                scope,
                scope.start_byte(),
            ));
            if name_node.kind() == "identifier" {
                if let Some(kind) = translator.clone() {
                    bindings.push(binding(&name, kind, scope, node.end_byte()));
                }
            }
        }
    });
}

fn binding(name: &str, kind: BindingKind, scope: Node<'_>, visible_from: usize) -> Binding {
    Binding {
        name: name.to_owned(),
        kind,
        scope_start: scope.start_byte(),
        scope_end: scope.end_byte(),
        depth: ancestors(scope),
        visible_from,
    }
}

fn translator_binding(mut value: Node<'_>, source: &str) -> Option<BindingKind> {
    if value.kind() == "await_expression" {
        value = value.named_child(0)?;
    }
    if value.kind() != "call_expression" {
        return None;
    }
    let function = value.child_by_field_name("function")?;
    let name = text(function, source)?;
    if name != "useTranslations" && name != "getTranslations" {
        return None;
    }
    let arguments = value.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let args: Vec<_> = arguments.named_children(&mut cursor).collect();
    let Some(argument) = args.first().copied() else {
        return Some(BindingKind::Translator(String::new()));
    };
    if argument.kind() == "string" {
        return decode_string(argument, source).map(|(value, _, _)| BindingKind::Translator(value));
    }
    if argument.kind() != "object" {
        return Some(BindingKind::UnknownTranslator);
    }
    let mut namespace = None;
    let mut namespace_is_uncertain = false;
    let mut cursor = argument.walk();
    for property in argument.named_children(&mut cursor) {
        if property.kind() == "spread_element" {
            namespace_is_uncertain = true;
            continue;
        }
        if matches!(
            property.kind(),
            "shorthand_property_identifier" | "shorthand_property_identifier_pattern"
        ) && text(property, source) == Some("namespace")
        {
            namespace_is_uncertain = true;
            continue;
        }
        if property.kind() != "pair" {
            continue;
        }
        let Some(key) = property.child_by_field_name("key") else {
            continue;
        };
        let is_namespace = text(key, source) == Some("namespace")
            || decode_string(key, source).is_some_and(|(value, _, _)| value == "namespace");
        if is_namespace {
            let Some(value) = property.child_by_field_name("value") else {
                namespace_is_uncertain = true;
                continue;
            };
            let Some((value, _, _)) = decode_string(value, source) else {
                namespace_is_uncertain = true;
                continue;
            };
            namespace = Some(value);
            namespace_is_uncertain = false;
        }
    }
    if namespace_is_uncertain {
        Some(BindingKind::UnknownTranslator)
    } else {
        Some(BindingKind::Translator(namespace.unwrap_or_default()))
    }
}

fn collect_calls(root: Node<'_>, source: &str, bindings: &[Binding], analysis: &mut Analysis) {
    walk(root, |node| {
        if node.kind() != "call_expression" {
            return;
        }
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        let name = if function.kind() == "identifier" {
            text(function, source)
        } else if function.kind() == "member_expression" {
            let object = function.child_by_field_name("object");
            let property = function.child_by_field_name("property");
            match (object, property) {
                (Some(object), Some(property))
                    if object.kind() == "identifier"
                        && matches!(
                            text(property, source),
                            Some("rich" | "markup" | "raw" | "has")
                        ) =>
                {
                    text(object, source)
                }
                _ => None,
            }
        } else {
            None
        };
        let Some(name) = name else { return };
        let Some(arguments) = node.child_by_field_name("arguments") else {
            return;
        };
        let Some(argument) = arguments.named_child(0) else {
            return;
        };
        let Some((key, start, end)) = decode_string(argument, source) else {
            return;
        };
        let is_translator_name = name == "t"
            || bindings.iter().any(|binding| {
                binding.name == name
                    && binding.scope_start <= node.start_byte()
                    && node.start_byte() < binding.scope_end
                    && matches!(
                        binding.kind,
                        BindingKind::Translator(_) | BindingKind::UnknownTranslator
                    )
            });
        if !is_translator_name {
            return;
        }
        analysis.claimed_offsets.push(start);

        let resolved = resolve(name, node.start_byte(), bindings);
        let namespace = match resolved.map(|binding| &binding.kind) {
            Some(BindingKind::Translator(namespace)) => Some(namespace.as_str()),
            Some(BindingKind::UnknownTranslator) => return,
            Some(BindingKind::Other) if name != "t" => return,
            None if name != "t" => return,
            _ => Some(""),
        };
        let key = match namespace {
            Some("") | None => key,
            Some(namespace) => format!("{namespace}.{key}"),
        };
        analysis.candidates.push(Candidate { key, start, end });
    });
}

fn resolve<'a>(name: &str, offset: usize, bindings: &'a [Binding]) -> Option<&'a Binding> {
    bindings
        .iter()
        .filter(|binding| {
            binding.name == name
                && binding.scope_start <= offset
                && offset < binding.scope_end
                && binding.visible_from <= offset
        })
        .max_by_key(|binding| (binding.depth, binding.visible_from))
}

fn scope_for(mut node: Node<'_>, function_scope: bool) -> Node<'_> {
    while let Some(parent) = node.parent() {
        node = parent;
        if (function_scope && (is_function(node.kind()) || node.kind() == "program"))
            || (!function_scope
                && matches!(
                    node.kind(),
                    "statement_block"
                        | "for_statement"
                        | "for_in_statement"
                        | "switch_body"
                        | "program"
                ))
        {
            return node;
        }
    }
    node
}

fn is_function(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "generator_function_declaration"
            | "generator_function"
    )
}

fn ancestors(mut node: Node<'_>) -> usize {
    let mut count = 0;
    while let Some(parent) = node.parent() {
        count += 1;
        node = parent;
    }
    count
}

fn collect_pattern_names(node: Node<'_>, source: &str, add: &mut impl FnMut(&str)) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            if let Some(name) = text(node, source) {
                add(name);
            }
        }
        "required_parameter" | "optional_parameter" => {
            if let Some(pattern) = node
                .child_by_field_name("pattern")
                .or_else(|| node.child_by_field_name("name"))
            {
                collect_pattern_names(pattern, source, add);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            if let Some(left) = node
                .child_by_field_name("left")
                .or_else(|| node.named_child(0))
            {
                collect_pattern_names(left, source, add);
            }
        }
        "pair_pattern" => {
            if let Some(value) = node
                .child_by_field_name("value")
                .or_else(|| node.named_child(1))
            {
                collect_pattern_names(value, source, add);
            }
        }
        "rest_pattern" => {
            if let Some(pattern) = node.named_child(0) {
                collect_pattern_names(pattern, source, add);
            }
        }
        "formal_parameters" | "array_pattern" | "object_pattern" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_pattern_names(child, source, add);
            }
        }
        _ => {}
    }
}

fn text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    source.get(node.byte_range())
}

fn decode_string(node: Node<'_>, source: &str) -> Option<(String, usize, usize)> {
    if node.kind() != "string" {
        return None;
    }
    let raw = text(node, source)?;
    let quote = raw.as_bytes().first().copied()?;
    if !matches!(quote, b'\'' | b'"') || raw.as_bytes().last().copied() != Some(quote) {
        return None;
    }
    let inner = &raw[1..raw.len() - 1];
    let mut decoded = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        let escaped = chars.next()?;
        match escaped {
            'b' => decoded.push('\u{0008}'),
            'f' => decoded.push('\u{000c}'),
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            't' => decoded.push('\t'),
            'v' => decoded.push('\u{000b}'),
            '0' if !chars.peek().is_some_and(char::is_ascii_digit) => decoded.push('\0'),
            '\\' => decoded.push('\\'),
            '\'' => decoded.push('\''),
            '"' => decoded.push('"'),
            '\n' => {}
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            'x' => decoded.push(read_hex_escape(&mut chars, 2)?),
            'u' if chars.peek() == Some(&'{') => {
                chars.next();
                let mut digits = String::new();
                for ch in chars.by_ref() {
                    if ch == '}' {
                        break;
                    }
                    digits.push(ch);
                }
                let value = u32::from_str_radix(&digits, 16).ok()?;
                decoded.push(char::from_u32(value)?);
            }
            'u' => decoded.push(read_hex_escape(&mut chars, 4)?),
            other => decoded.push(other),
        }
    }
    Some((decoded, node.start_byte() + 1, node.end_byte() - 1))
}

fn read_hex_escape(
    chars: &mut std::iter::Peekable<impl Iterator<Item = char>>,
    length: usize,
) -> Option<char> {
    let digits: String = chars.take(length).collect();
    if digits.len() != length {
        return None;
    }
    char::from_u32(u32::from_str_radix(&digits, 16).ok()?)
}
