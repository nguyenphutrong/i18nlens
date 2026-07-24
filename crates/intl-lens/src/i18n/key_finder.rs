use std::collections::HashSet;

use regex::Regex;

use super::next_intl;

#[derive(Debug, Clone)]
pub struct FoundKey {
    pub key: String,
    pub start_offset: usize,
    pub line: usize,
    pub start_char: usize,
    pub end_char: usize,
}

pub struct KeyFinder {
    patterns: Vec<Regex>,
}

impl KeyFinder {
    pub fn new(patterns: &[String]) -> Self {
        let compiled_patterns: Vec<Regex> =
            patterns.iter().filter_map(|p| Regex::new(p).ok()).collect();

        Self {
            patterns: compiled_patterns,
        }
    }

    pub fn find_keys(&self, content: &str) -> Vec<FoundKey> {
        let mut found_keys = Vec::new();

        for pattern in &self.patterns {
            for cap in pattern.captures_iter(content) {
                if let Some(key_match) = cap.get(1) {
                    let key = key_match.as_str().to_string();
                    let start_offset = key_match.start();
                    let end_offset = key_match.end();

                    let (line, start_char, end_char) =
                        Self::offset_to_position(content, start_offset, end_offset);

                    found_keys.push(FoundKey {
                        key,
                        start_offset,
                        line,
                        start_char,
                        end_char,
                    });
                }
            }
        }

        let analysis = next_intl::analyze(content);
        let ast_offsets: HashSet<_> = analysis.claimed_offsets.into_iter().collect();
        found_keys.retain(|key| !ast_offsets.contains(&key.start_offset));

        for candidate in analysis.candidates {
            let (line, start_char, end_char) =
                Self::offset_to_position(content, candidate.start, candidate.end);
            found_keys.push(FoundKey {
                key: candidate.key,
                start_offset: candidate.start,
                line,
                start_char,
                end_char,
            });
        }

        found_keys.sort_by_key(|k| k.start_offset);
        found_keys.dedup_by(|a, b| a.start_offset == b.start_offset);
        found_keys
    }

    pub fn find_key_at_position(
        &self,
        content: &str,
        line: usize,
        character: usize,
    ) -> Option<FoundKey> {
        let keys = self.find_keys(content);

        keys.into_iter()
            .find(|k| k.line == line && character >= k.start_char && character <= k.end_char)
    }

    fn offset_to_position(
        content: &str,
        start_offset: usize,
        end_offset: usize,
    ) -> (usize, usize, usize) {
        let mut line = 0;
        let mut line_start = 0;

        for (i, ch) in content.char_indices() {
            if i >= start_offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                line_start = i + 1;
            }
        }

        let start_char = content[line_start..start_offset].encode_utf16().count();
        let end_char = content[line_start..end_offset].encode_utf16().count();

        (line, start_char, end_char)
    }
}

impl Default for KeyFinder {
    fn default() -> Self {
        Self::new(&default_patterns())
    }
}

fn default_patterns() -> Vec<String> {
    vec![
        // JavaScript/TypeScript patterns (`t` is handled by the TSX parser)
        r#"i18n\.t\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"\$t\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"\$tc\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"\$te\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"useI18n\s*\(\s*\)\s*.*?\.t\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"formatMessage\s*\(\s*\{\s*id:\s*["']([^"']+)["']"#.to_string(),
        r#"<Trans\s+i18nKey\s*=\s*["']([^"']+)["']"#.to_string(),
        // Svelte patterns (svelte-i18n)
        r#"\$_\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"\$format\s*\(\s*["']([^"']+)["']"#.to_string(),
        // Flutter/Dart patterns - easy_localization
        r#"['"]([^'"]+)['"]\s*\.tr\("#.to_string(),
        r#"['"]([^'"]+)['"]\s*\.tr\(\)"#.to_string(),
        r#"(?:^|[^\w.])tr\(\s*['"]([^'"]+)['"]"#.to_string(),
        r#"context\.tr\(\s*['"]([^'"]+)['"]"#.to_string(),
        r#"['"]([^'"]+)['"]\s*\.plural\("#.to_string(),
        // Flutter/Dart patterns - flutter_i18n
        r#"FlutterI18n\.translate\([^,]+,\s*['"]([^'"]+)['"]"#.to_string(),
        r#"FlutterI18n\.plural\([^,]+,\s*['"]([^'"]+)['"]"#.to_string(),
        r#"I18nText\(\s*['"]([^'"]+)['"]"#.to_string(),
        r#"I18nPlural\(\s*['"]([^'"]+)['"]"#.to_string(),
        // Flutter/Dart patterns - GetX
        r#"['"]([^'"]+)['"]\s*\.tr(?:\s|$|\)|,)"#.to_string(),
        r#"['"]([^'"]+)['"]\s*\.trParams\("#.to_string(),
        r#"['"]([^'"]+)['"]\s*\.trPlural\("#.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_t_function() {
        let finder = KeyFinder::default();
        let content = r#"const msg = t("hello.world");"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "hello.world");
    }

    #[test]
    fn test_find_next_intl_namespaced_key() {
        let finder = KeyFinder::default();
        let content = r#"
            import { useTranslations } from "next-intl";
            const t = useTranslations("Landing.Contact");
            const selectPlaceholder = t("SelectPlaceholder");
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Contact.SelectPlaceholder");
    }

    #[test]
    fn test_find_next_intl_namespaced_key_with_custom_translator_name() {
        let finder = KeyFinder::default();
        let content = r#"
            const contactT = useTranslations("Landing.Contact");
            const selectPlaceholder = contactT("SelectPlaceholder");
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Contact.SelectPlaceholder");
    }

    #[test]
    fn test_find_next_intl_namespaced_rich_key() {
        let finder = KeyFinder::default();
        let content = r#"
            const t = useTranslations("Landing.Contact");
            const label = t.rich("SelectLabel", {strong: (chunks) => <strong>{chunks}</strong>});
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Contact.SelectLabel");
    }

    #[test]
    fn test_find_next_intl_unscoped_rich_key() {
        let finder = KeyFinder::default();
        let content = r#"
            const t = useTranslations();
            const label = t.rich("Auth.Login.GoToDiscord", {a: (chunks) => <a>{chunks}</a>});
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Auth.Login.GoToDiscord");
    }

    #[test]
    fn test_find_next_intl_unscoped_custom_translator_name() {
        let finder = KeyFinder::default();
        let content = r#"
            const tRoot = useTranslations();
            const title = tRoot("App.Hub.Title");
            const label = tRoot.rich("App.Hub.Label", {b: (chunks) => <b>{chunks}</b>});
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "App.Hub.Title");
        assert_eq!(keys[1].key, "App.Hub.Label");
    }

    #[test]
    fn test_find_next_intl_get_translations_object_arg_unscoped() {
        let finder = KeyFinder::default();
        let content = r#"
            const t = await getTranslations({ locale });
            const title = t("Landing.Features.Title");
            const desc = t.rich("Landing.Features.Description", {strong: (c) => <strong>{c}</strong>});
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "Landing.Features.Title");
        assert_eq!(keys[1].key, "Landing.Features.Description");
    }

    #[test]
    fn test_find_next_intl_get_translations_object_arg_with_namespace() {
        let finder = KeyFinder::default();
        let content = r#"
            const t = await getTranslations({ locale, namespace: "Landing.Hero" });
            const title = t("Title");
            const tagline = t.rich("Tagline", {em: (c) => <em>{c}</em>});
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "Landing.Hero.Title");
        assert_eq!(keys[1].key, "Landing.Hero.Tagline");
    }

    #[test]
    fn test_find_next_intl_get_translations_multiline_object_arg() {
        let finder = KeyFinder::default();
        let content = r#"
            const t = await getTranslations({
                locale,
                namespace: "Landing.Hero",
            });
            const title = t("Title");
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Hero.Title");
    }

    #[test]
    fn test_find_rich_key_without_translator_declaration() {
        // `t` received via props/params, no declaration in this file.
        let finder = KeyFinder::default();
        let content = r#"
            export function Headline({ t }) {
                return <h1>{t.rich("Landing.Hero.HeadlineFull", {highlight: (c) => <span>{c}</span>})}</h1>;
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Hero.HeadlineFull");
    }

    #[test]
    fn test_find_next_intl_multiline_rich_call() {
        let finder = KeyFinder::default();
        let content = "
            const t = useTranslations(\"Events.WinPopup\");
            const body = t.rich(
                \"BodyNoCoupon\",
                { strong: (c) => <strong>{c}</strong> },
            );
        ";

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Events.WinPopup.BodyNoCoupon");
    }

    #[test]
    fn test_should_not_match_rich_on_other_objects() {
        let finder = KeyFinder::default();
        // `.rich` on identifiers other than a translator must not match.
        let content = r#"
            editor.rich("some.setting");
            format.rich("other.value");
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 0);
    }

    #[test]
    fn test_find_next_intl_mixed_unscoped_and_scoped_same_variable() {
        let finder = KeyFinder::default();
        let content = r#"
            function Breadcrumbs() {
                const t = useTranslations();
                const home = t("App.Nav.Home");
            }
            function Hero() {
                const t = useTranslations("Landing.Hero");
                const title = t("Title");
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "App.Nav.Home");
        assert_eq!(keys[1].key, "Landing.Hero.Title");
    }

    #[test]
    fn test_find_next_intl_same_variable_different_namespaces() {
        let finder = KeyFinder::default();
        let content = r#"
            function Rewards() {
                const t = useTranslations("Events.Rewards");
                const title = t("Title");
            }
            function Rules() {
                const t = useTranslations("Events.Rules");
                const heading = t.rich("Heading", {b: (c) => <b>{c}</b>});
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "Events.Rewards.Title");
        assert_eq!(keys[1].key, "Events.Rules.Heading");
    }

    #[test]
    fn test_scoped_translator_does_not_leak_to_props_translator_in_later_component() {
        let finder = KeyFinder::default();
        // `Body` receives `t` via props; the scoped `t` in the earlier `Header`
        // component must not leak across the function boundary, so the props
        // call resolves as the literal (unscoped) key.
        let content = r#"
            function Header() {
                const t = useTranslations("Header");
                return <h1>{t("Title")}</h1>;
            }

            function Body({ t }) {
                return <p>{t.rich("Page.Body", {b: (c) => <b>{c}</b>})}</p>;
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "Header.Title");
        assert_eq!(keys[1].key, "Page.Body");
    }

    #[test]
    fn test_scoped_translator_does_not_leak_backwards_regardless_of_source_order() {
        let finder = KeyFinder::default();
        // Same as above but the props component appears *before* the scoped
        // declaration. Byte-offset proximity is irrelevant; only lexical scope
        // decides, so the props call stays unscoped.
        let content = r#"
            function Body({ t }) {
                return <p>{t.rich("Page.Body", {b: (c) => <b>{c}</b>})}</p>;
            }

            function Header() {
                const t = useTranslations("Header");
                return <h1>{t("Title")}</h1>;
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "Page.Body");
        assert_eq!(keys[1].key, "Header.Title");
    }

    #[test]
    fn test_scoped_translator_covers_nested_block_within_its_component() {
        let finder = KeyFinder::default();
        // A call nested inside an inner block of the same component must still
        // resolve against the component's scoped translator.
        let content = r#"
            function Hero({ show }) {
                const t = useTranslations("Landing.Hero");
                if (show) {
                    return <h1>{t.rich("Title", {b: (c) => <b>{c}</b>})}</h1>;
                }
                return null;
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Hero.Title");
    }

    #[test]
    fn test_scoped_translator_ignores_braces_in_strings() {
        let finder = KeyFinder::default();
        let content = r#"
            function Hero() {
                const t = useTranslations("Landing.Hero");
                const markers = ["}", '}', `}`];
                return <h1>{t("Title")}</h1>;
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Hero.Title");
    }

    #[test]
    fn test_scoped_translator_ignores_braces_in_comments() {
        let finder = KeyFinder::default();
        let content = r#"
            function Hero() {
                const t = useTranslations("Landing.Hero");
                // }
                /* } */
                return <h1>{t("Title")}</h1>;
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Hero.Title");
    }

    #[test]
    fn test_scoped_translator_ignores_braces_in_regex_literals() {
        let finder = KeyFinder::default();
        let content = r#"
            function Hero() {
                const t = useTranslations("Landing.Hero");
                const closingBrace = /[}]/;
                return <h1>{t("Title")}</h1>;
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Hero.Title");
    }

    #[test]
    fn test_ignores_translator_declarations_in_comments_and_strings() {
        let finder = KeyFinder::default();
        let content = r#"
            function Hero() {
                const t = useTranslations("Landing.Hero");
                // const t = useTranslations("Comment");
                const example = `const t = useTranslations("String")`;
                return <h1>{t("Title")}</h1>;
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Hero.Title");
    }

    #[test]
    fn test_scoped_translator_handles_nested_template_literals() {
        let finder = KeyFinder::default();
        let content = r#"
            function Hero(flag) {
                const t = useTranslations("Landing.Hero");
                const marker = `${flag ? `}` : ""}`;
                return `${t("Title")} ${marker}`;
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Hero.Title");
    }

    #[test]
    fn test_callback_parameter_shadows_scoped_translator() {
        let finder = KeyFinder::default();
        let content = r#"
            function Page(items) {
                const t = useTranslations("Page");
                return items.map((t) => t("Item.Title"));
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Item.Title");
    }

    #[test]
    fn test_unparenthesized_arrow_parameter_shadows_scoped_translator() {
        let finder = KeyFinder::default();
        let content = r#"
            function Page(items) {
                const t = useTranslations("Page");
                return items.map(t => t("Item.Title"));
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Item.Title");
    }

    #[test]
    fn test_local_binding_shadows_scoped_translator() {
        let finder = KeyFinder::default();
        let content = r#"
            function Page() {
                const t = useTranslations("Page");
                if (preview) {
                    const t = makePreviewTranslator();
                    return t("Preview.Title");
                }
                return t("Title");
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "Preview.Title");
        assert_eq!(keys[1].key, "Page.Title");
    }

    #[test]
    fn test_var_translator_is_function_scoped() {
        let finder = KeyFinder::default();
        let content = r#"
            async function Page(enabled) {
                if (enabled) {
                    var t = await getTranslations("Page");
                }
                return t("Title");
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Page.Title");
    }

    #[test]
    fn test_var_translator_does_not_apply_before_assignment() {
        let finder = KeyFinder::default();
        let content = r#"
            async function Page() {
                t("Before.Assignment");
                var t = await getTranslations("Page");
                return t("Title");
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "Before.Assignment");
        assert_eq!(keys[1].key, "Page.Title");
    }

    #[test]
    fn test_static_keys_allow_other_and_escaped_quotes() {
        let finder = KeyFinder::default();
        let content = r#"
            const t = useTranslations();
            t("don't");
            t('say "hi"');
            t("say \"hello\"");
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].key, "don't");
        assert_eq!(keys[1].key, "say \"hi\"");
        assert_eq!(keys[2].key, "say \"hello\"");
    }

    #[test]
    fn test_ast_result_takes_precedence_over_configured_t_regex() {
        let finder = KeyFinder::new(&[r#"(?:^|[^\w.])t\s*\(\s*["']([^"']+)["']"#.to_string()]);
        let content = r#"
            const t = useTranslations("Landing");
            t("Title");
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Landing.Title");
    }

    #[test]
    fn test_unknown_namespaces_do_not_invent_translation_keys() {
        let finder = KeyFinder::default();
        let content = r#"
            const dynamic = useTranslations(namespace);
            dynamic("Dynamic.Title");
            const template = useTranslations(`Template`);
            template("Template.Title");
            const shorthand = useTranslations({namespace});
            shorthand("Shorthand.Title");
            const spread = useTranslations({...options});
            spread("Spread.Title");
            const overridden = useTranslations({namespace: "Known", ...options});
            overridden("Overridden.Title");
            const known = useTranslations({...options, namespace: "Known"});
            known("Title");
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Known.Title");
    }

    #[test]
    fn test_function_and_catch_bindings_shadow_outer_translator() {
        let finder = KeyFinder::default();
        let content = r#"
            const tr = useTranslations("Outer");
            function page() {
                tr("Function.Title");
                function tr() {}
            }
            const callback = function tr() {
                return tr("Expression.Title");
            };
            try {
                run();
            } catch (tr) {
                tr("Catch.Title");
            }
        "#;

        let keys = finder.find_keys(content);

        assert!(keys.is_empty(), "{keys:?}");
    }

    #[test]
    fn test_scope_end_is_exclusive_without_whitespace() {
        let finder = KeyFinder::default();
        let content = r#"const t=makeTranslator();function A(){const t=useTranslations("A");t("Inside")}t("Outside")"#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "A.Inside");
        assert_eq!(keys[1].key, "Outside");
    }

    #[test]
    fn test_method_name_does_not_shadow_outer_translator() {
        let finder = KeyFinder::default();
        let content = r#"
            const tr = useTranslations("Outer");
            class View {
                tr() {
                    return tr("Title");
                }
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Outer.Title");
    }

    #[test]
    fn test_later_static_namespace_overrides_dynamic_namespace() {
        let finder = KeyFinder::default();
        let content = r#"
            const t = getTranslations({
                namespace: selectedNamespace,
                namespace: "Known",
            });
            t("Title");
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Known.Title");
    }

    #[test]
    fn test_custom_translator_claims_only_its_lexical_scope() {
        let finder = KeyFinder::new(&[r#"translate\(\s*["']([^"']+)["']"#.to_string()]);
        let content = r#"
            function A() {
                const translate = useTranslations("A");
                return translate("One");
            }
            function B() {
                return translate("External");
            }
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "A.One");
        assert_eq!(keys[1].key, "External");
    }

    #[test]
    fn test_all_supported_translator_methods_and_property_exclusions() {
        let finder = KeyFinder::default();
        let content = r#"
            const t = useTranslations("Page");
            t.markup("Markup");
            t.raw("Raw");
            t.has("Has");
            props.t("Ignored.Prop");
            this.t.rich("Ignored.This");
            object.t("Ignored.Object");
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].key, "Page.Markup");
        assert_eq!(keys[1].key, "Page.Raw");
        assert_eq!(keys[2].key, "Page.Has");
    }

    #[test]
    fn test_method_parameter_shadows_outer_translator() {
        let finder = KeyFinder::default();
        let content = r#"
            const tr = useTranslations("Outer");
            class Preview {
                render(tr = fallback) {
                    return tr("Preview.Title");
                }
            }
        "#;

        let keys = finder.find_keys(content);

        assert!(keys.is_empty(), "{keys:?}");
    }

    #[test]
    fn test_escaped_key_offsets_and_utf16_positions() {
        let finder = KeyFinder::default();
        let content = "const t = useTranslations();\n\"😀\"; t(\"say \\\"hi\\\"\");";

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        let key = &keys[0];
        assert_eq!(key.key, "say \"hi\"");
        assert_eq!(
            &content[key.start_offset..key.start_offset + 10],
            r#"say \"hi\""#
        );
        assert_eq!(key.line, 1, "{key:?}");
        assert_eq!(key.start_char, 9);
        assert_eq!(key.end_char, 19);
    }

    #[test]
    fn test_ignores_translation_calls_in_comments_and_strings() {
        let finder = KeyFinder::default();
        let content = r#"
            const t = useTranslations("Page");
            // t("CommentedOut");
            const example = `t("StringExample")`;
            t("Title");
        "#;

        let keys = finder.find_keys(content);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "Page.Title");
    }

    #[test]
    fn test_find_dollar_t() {
        let finder = KeyFinder::default();
        let content = r#"const msg = $t("common.button");"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "common.button");
    }

    #[test]
    fn test_find_multiple_keys() {
        let finder = KeyFinder::default();
        let content = r#"
            const a = t("first.key");
            const b = t("second.key");
        "#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "first.key");
        assert_eq!(keys[1].key, "second.key");
    }

    #[test]
    fn test_find_trans_component() {
        let finder = KeyFinder::default();
        let content = r#"<Trans i18nKey="my.key">Default</Trans>"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "my.key");
    }

    #[test]
    fn test_find_key_at_position() {
        let finder = KeyFinder::default();
        let content = r#"const msg = t("hello.world");"#;

        let found = finder.find_key_at_position(content, 0, 16);
        assert!(found.is_some());
        assert_eq!(found.unwrap().key, "hello.world");

        let not_found = finder.find_key_at_position(content, 0, 0);
        assert!(not_found.is_none());
    }

    #[test]
    fn test_find_flutter_easy_localization_tr() {
        let finder = KeyFinder::default();
        let content = r#"Text('hello.world'.tr())"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "hello.world");
    }

    #[test]
    fn test_find_flutter_easy_localization_tr_function() {
        let finder = KeyFinder::default();
        let content = r#"tr('common.greeting')"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "common.greeting");
    }

    #[test]
    fn test_find_flutter_i18n_translate() {
        let finder = KeyFinder::default();
        let content = r#"FlutterI18n.translate(context, 'messages.welcome')"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "messages.welcome");
    }

    #[test]
    fn test_find_flutter_i18n_text_widget() {
        let finder = KeyFinder::default();
        let content = r#"I18nText('button.submit')"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "button.submit");
    }

    #[test]
    fn test_find_flutter_getx_tr() {
        let finder = KeyFinder::default();
        let content = r#"Text('hello'.tr)"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "hello");
    }

    #[test]
    fn test_find_flutter_getx_tr_params() {
        let finder = KeyFinder::default();
        let content = r#"'greeting'.trParams({'name': 'John'})"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "greeting");
    }

    #[test]
    fn test_should_not_match_api_methods() {
        let finder = KeyFinder::default();
        // Should NOT match .post(), .get(), .put(), .delete(), .patch(), .request()
        let test_cases = vec![
            r#"apiClient.post('/api/products')"#,
            r#"client.get('/api/users')"#,
            r#"http.put('/api/update')"#,
            r#"axios.delete('/api/remove')"#,
            r#"fetch.request('/api/data')"#,
            r#"this.httpClient.get('/users')"#,
            r#"await api.post('/endpoint')"#,
            // More realistic cases
            r#"const response = await apiClient.post('/api/products', data);"#,
            r#"return this.http.get('/api/users');"#,
            r#"apiClient.put('/api/update', { id: 1 });"#,
            // Edge cases that should NOT match
            r#"transport('/some/path')"#,
            r#"contrast('/api/test')"#,
        ];

        for content in test_cases {
            let keys = finder.find_keys(content);
            assert_eq!(
                keys.len(),
                0,
                "Should not match: {} but got {:?}",
                content,
                keys.iter().map(|k| &k.key).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn test_should_match_t_but_not_method_ending_with_t() {
        let finder = KeyFinder::default();
        // Should match t() but not .post(), .request(), etc.
        let content = r#"
            const msg = t("hello.world");
            apiClient.post('/api/products');
        "#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "hello.world");
    }

    #[test]
    fn test_find_svelte_dollar_underscore() {
        let finder = KeyFinder::default();
        let content = r#"const msg = $_("hello.world");"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "hello.world");
    }

    #[test]
    fn test_find_svelte_dollar_underscore_single_quotes() {
        let finder = KeyFinder::default();
        let content = r#"const msg = $_('common.greeting');"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "common.greeting");
    }

    #[test]
    fn test_find_svelte_dollar_format() {
        let finder = KeyFinder::default();
        let content = r#"const msg = $format("hello.world");"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "hello.world");
    }

    #[test]
    fn test_find_svelte_dollar_underscore_in_template() {
        let finder = KeyFinder::default();
        let content = r#"<p>{$_("page.title")}</p>"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "page.title");
    }

    #[test]
    fn test_find_svelte_dollar_t_in_template() {
        let finder = KeyFinder::default();
        let content = r#"<h1>{$t("welcome.heading")}</h1>"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "welcome.heading");
    }

    #[test]
    fn test_find_svelte_multiple_keys() {
        let finder = KeyFinder::default();
        let content = r#"
            <h1>{$_("page.title")}</h1>
            <p>{$_("page.description")}</p>
            <button>{$t("common.submit")}</button>
        "#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].key, "page.title");
        assert_eq!(keys[1].key, "page.description");
        assert_eq!(keys[2].key, "common.submit");
    }

    #[test]
    fn test_find_vue_dollar_t() {
        let finder = KeyFinder::default();
        let content = r#"const msg = $t('common.greeting');"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "common.greeting");
    }

    #[test]
    fn test_find_vue_dollar_tc() {
        let finder = KeyFinder::default();
        let content = r#"const msg = $tc('messages.item', count);"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "messages.item");
    }

    #[test]
    fn test_find_vue_dollar_te() {
        let finder = KeyFinder::default();
        let content = r#"if ($te('key.exists')) { }"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "key.exists");
    }

    #[test]
    fn test_find_vue_composition_api() {
        let finder = KeyFinder::default();
        let content = r#"const { t } = useI18n(); const msg = t('welcome.message');"#;
        let keys = finder.find_keys(content);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "welcome.message");
    }
}
