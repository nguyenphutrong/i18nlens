use std::collections::HashSet;

use regex::Regex;

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

        let scoped_keys = Self::find_next_intl_scoped_keys(content);
        if !scoped_keys.is_empty() {
            let scoped_offsets: HashSet<usize> =
                scoped_keys.iter().map(|key| key.start_offset).collect();
            found_keys.retain(|key| !scoped_offsets.contains(&key.start_offset));
            found_keys.extend(scoped_keys);
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

        let start_char = start_offset - line_start;
        let end_char = end_offset - line_start;

        (line, start_char, end_char)
    }

    fn find_next_intl_scoped_keys(content: &str) -> Vec<FoundKey> {
        let Ok(translator_regex) = Regex::new(
            r#"(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:await\s+)?(?:useTranslations|getTranslations)\s*\(\s*["']([^"']+)["']\s*\)"#,
        ) else {
            return Vec::new();
        };

        let mut found_keys = Vec::new();

        for translator in translator_regex.captures_iter(content) {
            let Some(variable_match) = translator.get(1) else {
                continue;
            };
            let Some(namespace_match) = translator.get(2) else {
                continue;
            };

            let variable = variable_match.as_str();
            let namespace = namespace_match.as_str();
            if namespace.is_empty() {
                continue;
            }

            let call_pattern = format!(
                r#"(?:^|[^\w.]){}(?:\.(?:rich|markup|raw|has))?\s*\(\s*["']([^"']+)["']"#,
                regex::escape(variable)
            );
            let Ok(call_regex) = Regex::new(&call_pattern) else {
                continue;
            };

            let declaration_end = translator.get(0).map(|m| m.end()).unwrap_or(0);
            for call in call_regex.captures_iter(content) {
                let Some(key_match) = call.get(1) else {
                    continue;
                };
                if key_match.start() < declaration_end {
                    continue;
                }

                let key = format!("{}.{}", namespace, key_match.as_str());
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

        found_keys
    }
}

impl Default for KeyFinder {
    fn default() -> Self {
        Self::new(&default_patterns())
    }
}

fn default_patterns() -> Vec<String> {
    vec![
        // JavaScript/TypeScript patterns
        // Match t() but not .post(), .get(), .put(), .delete(), etc.
        r#"(?:^|[^\w.])t\s*\(\s*["']([^"']+)["']"#.to_string(),
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
