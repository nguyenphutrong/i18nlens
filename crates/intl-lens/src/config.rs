use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct I18nConfig {
    #[serde(default = "default_locale_paths", alias = "localesPaths")]
    pub locale_paths: Vec<String>,

    #[serde(default = "default_source_locale")]
    pub source_locale: String,

    #[serde(default = "default_key_style")]
    pub key_style: KeyStyle,

    #[serde(default)]
    pub namespace_enabled: bool,

    #[serde(default = "default_function_patterns")]
    pub function_patterns: Vec<String>,

    #[serde(default = "default_display_mode")]
    pub display_mode: DisplayMode,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyStyle {
    #[default]
    Nested,
    Flat,
    Auto,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DisplayMode {
    #[default]
    InlayHints,
    CodeLens,
}

impl Default for I18nConfig {
    fn default() -> Self {
        Self {
            locale_paths: default_locale_paths(),
            source_locale: default_source_locale(),
            key_style: default_key_style(),
            namespace_enabled: false,
            function_patterns: default_function_patterns(),
            display_mode: default_display_mode(),
        }
    }
}

impl I18nConfig {
    pub fn load_from_workspace(root: &Path) -> Self {
        let config_paths = [
            root.join(".i18nlens.json"),
            root.join("i18nlens.config.json"),
            root.join(".intl-lens.json"),
            root.join("intl-lens.config.json"),
            root.join(".zed/i18n.json"),
        ];

        for config_path in config_paths {
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                let raw_config = serde_json::from_str::<Value>(&content).ok();

                if let Ok(mut config) = serde_json::from_str::<I18nConfig>(&content) {
                    let has_locale_paths = raw_config
                        .as_ref()
                        .and_then(|value| value.as_object())
                        .is_some_and(|object| {
                            object.contains_key("localePaths")
                                || object.contains_key("locale_paths")
                                || object.contains_key("localesPaths")
                        });

                    if !has_locale_paths {
                        config.add_detected_locale_paths(root);
                    }

                    tracing::info!("Loaded config from {:?}", config_path);
                    return config;
                }
            }
        }

        tracing::info!("Using default config");
        let mut config = Self::default();
        config.add_detected_locale_paths(root);
        config
    }

    fn add_detected_locale_paths(&mut self, root: &Path) {
        let detected_paths = detect_framework_locale_paths(root);
        if detected_paths.is_empty() {
            return;
        }

        let mut existing: HashSet<String> = self.locale_paths.iter().cloned().collect();
        for path in detected_paths {
            if existing.insert(path.clone()) {
                self.locale_paths.push(path);
            }
        }
    }
}

fn default_locale_paths() -> Vec<String> {
    vec![
        "locales".to_string(),
        "i18n".to_string(),
        "translations".to_string(),
        "public/locales".to_string(),
        "src/locales".to_string(),
        "src/i18n".to_string(),
    ]
}

fn default_source_locale() -> String {
    "en".to_string()
}

fn default_key_style() -> KeyStyle {
    KeyStyle::Auto
}

fn default_function_patterns() -> Vec<String> {
    vec![
        // JavaScript/TypeScript patterns (`t` is handled by the TSX parser)
        r#"i18n\.t\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"\$t\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"\$tc\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"\$te\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"useI18n\s*\(\s*\)\s*.*?\.t\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"formatMessage\s*\(\s*\{\s*id:\s*["']([^"']+)["']"#.to_string(),
        // Svelte patterns (svelte-i18n)
        r#"\$_\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"\$format\s*\(\s*["']([^"']+)["']"#.to_string(),
        // Angular patterns
        r#"translateService\.(?:instant|get|stream)\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"translocoService\.(?:translate|selectTranslate)\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"["']([^"']+)["']\s*\|\s*(?:translate|transloco)\b"#.to_string(),
        // PHP/Laravel patterns
        r#"__\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"trans(?:_choice)?\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"Lang::(?:get|choice)\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"@lang\s*\(\s*["']([^"']+)["']"#.to_string(),
        r#"@choice\s*\(\s*["']([^"']+)["']"#.to_string(),
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

fn default_display_mode() -> DisplayMode {
    DisplayMode::InlayHints
}

fn detect_framework_locale_paths(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();

    if is_angular_project(root) {
        paths.push("src/assets/i18n".to_string());
    }

    if is_laravel_project(root) {
        paths.push("resources/lang".to_string());
        paths.push("lang".to_string());
    }

    if is_flutter_project(root) {
        // Check l10n.yaml for custom arb-dir
        if let Some(arb_dir) = parse_l10n_yaml(root) {
            paths.push(arb_dir);
        }
        // Default Flutter locale paths
        paths.push("lib/l10n".to_string());
        paths.push("assets/translations".to_string());
        paths.push("assets/flutter_i18n".to_string());
        paths.push("assets/i18n".to_string());
    }

    if is_vue_project(root) {
        paths.push("src/locales".to_string());
        paths.push("src/i18n".to_string());
        paths.push("src/lang".to_string());
        paths.push("locales".to_string());
        paths.push("i18n".to_string());
        paths.push("public/locales".to_string());
        paths.extend(detect_nuxt_layer_locale_paths(root));
    }

    if is_svelte_project(root) {
        paths.push("src/lib/i18n".to_string());
        paths.push("src/lib/locales".to_string());
        paths.push("src/locales".to_string());
        paths.push("src/i18n".to_string());
        paths.push("locales".to_string());
        paths.push("messages".to_string());
        paths.push("static/locales".to_string());
    }

    paths
        .into_iter()
        .filter(|path| root.join(path).exists())
        .collect()
}

fn detect_nuxt_layer_locale_paths(root: &Path) -> Vec<String> {
    let layers_dir = root.join("layers");
    let Ok(entries) = std::fs::read_dir(layers_dir) else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    for entry in entries.filter_map(|entry| entry.ok()) {
        let layer_path = entry.path();
        if !layer_path.is_dir() {
            continue;
        }

        for locale_dir in ["i18n/locales", "i18n"] {
            let candidate = layer_path.join(locale_dir);
            if !candidate.is_dir() {
                continue;
            }

            if let Ok(relative_path) = candidate.strip_prefix(root) {
                paths.push(relative_path.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    paths
}

fn is_angular_project(root: &Path) -> bool {
    let package_json = root.join("package.json");
    let Some(value) = read_json(&package_json) else {
        return false;
    };

    json_has_dependency(
        &value,
        "@angular/core",
        &["dependencies", "devDependencies"],
    ) || json_has_dependency(&value, "@angular/cli", &["dependencies", "devDependencies"])
}

fn is_laravel_project(root: &Path) -> bool {
    let composer_json = root.join("composer.json");
    let Some(value) = read_json(&composer_json) else {
        return false;
    };

    json_has_dependency(&value, "laravel/framework", &["require", "require-dev"])
        || json_has_name(&value, "laravel/laravel")
}

fn read_json(path: &Path) -> Option<Value> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&content).ok()
}

fn json_has_dependency(value: &Value, dependency: &str, sections: &[&str]) -> bool {
    sections.iter().any(|section| {
        value
            .get(*section)
            .and_then(|deps| deps.as_object())
            .is_some_and(|deps| deps.contains_key(dependency))
    })
}

fn json_has_name(value: &Value, name: &str) -> bool {
    value.get("name").and_then(|v| v.as_str()) == Some(name)
}

fn is_flutter_project(root: &Path) -> bool {
    let pubspec = root.join("pubspec.yaml");
    let Ok(content) = std::fs::read_to_string(&pubspec) else {
        return false;
    };

    // Check for flutter sdk dependency in pubspec.yaml
    content.contains("flutter:") && content.contains("sdk: flutter")
}

fn parse_l10n_yaml(root: &Path) -> Option<String> {
    let l10n_yaml = root.join("l10n.yaml");
    let content = std::fs::read_to_string(&l10n_yaml).ok()?;

    // Parse l10n.yaml to find arb-dir
    let yaml: serde_yaml::Value = serde_yaml::from_str(&content).ok()?;
    yaml.get("arb-dir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn is_svelte_project(root: &Path) -> bool {
    let package_json = root.join("package.json");
    let Some(value) = read_json(&package_json) else {
        return false;
    };

    json_has_dependency(&value, "svelte", &["dependencies", "devDependencies"])
        || json_has_dependency(&value, "svelte-i18n", &["dependencies", "devDependencies"])
        || json_has_dependency(
            &value,
            "sveltekit-i18n",
            &["dependencies", "devDependencies"],
        )
        || json_has_dependency(
            &value,
            "@inlang/paraglide-sveltekit",
            &["dependencies", "devDependencies"],
        )
        || root.join("svelte.config.js").exists()
        || root.join("svelte.config.ts").exists()
}

fn is_vue_project(root: &Path) -> bool {
    let package_json = root.join("package.json");
    let Some(value) = read_json(&package_json) else {
        return false;
    };

    json_has_dependency(&value, "vue", &["dependencies", "devDependencies"])
        || json_has_dependency(&value, "vue-i18n", &["dependencies", "devDependencies"])
        || json_has_dependency(
            &value,
            "@intlify/vue-i18n",
            &["dependencies", "devDependencies"],
        )
        || json_has_dependency(&value, "@nuxtjs/i18n", &["dependencies", "devDependencies"])
        || root.join("vue.config.js").exists()
        || root.join("vite.config.js").exists()
        || root.join("vite.config.ts").exists()
        || root.join("nuxt.config.js").exists()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn detects_nuxt_layer_locale_paths() {
        let root = test_workspace("nuxt-layer-detection");
        fs::create_dir_all(root.join("layers/foo/i18n/locales")).expect("create layer locales");
        fs::write(
            root.join("nuxt.config.js"),
            "export default defineNuxtConfig({})",
        )
        .expect("write nuxt config");
        fs::write(
            root.join("package.json"),
            r#"{"dependencies":{"@nuxtjs/i18n":"10.2.1"}}"#,
        )
        .expect("write package json");

        let config = I18nConfig::load_from_workspace(&root);

        assert!(config
            .locale_paths
            .contains(&"layers/foo/i18n/locales".to_string()));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reads_locales_paths_alias() {
        let config = serde_json::from_str::<I18nConfig>(
            r#"{"localesPaths":["src/lang","**/*/i18n/locales"]}"#,
        )
        .expect("parse config");

        assert_eq!(
            config.locale_paths,
            vec!["src/lang".to_string(), "**/*/i18n/locales".to_string()]
        );
    }

    #[test]
    fn loads_i18nlens_config_before_legacy_and_zed_config() {
        let root = test_workspace("i18nlens-config-priority");
        fs::create_dir_all(root.join(".zed")).expect("create zed dir");
        fs::write(
            root.join(".i18nlens.json"),
            r#"{"localePaths":["i18nlens-locales"],"sourceLocale":"ja"}"#,
        )
        .expect("write i18nlens config");
        fs::write(
            root.join(".intl-lens.json"),
            r#"{"localePaths":["intl-locales"],"sourceLocale":"vi"}"#,
        )
        .expect("write intl lens config");
        fs::write(
            root.join(".zed/i18n.json"),
            r#"{"localePaths":["zed-locales"],"sourceLocale":"en"}"#,
        )
        .expect("write zed config");

        let config = I18nConfig::load_from_workspace(&root);

        assert_eq!(config.locale_paths, vec!["i18nlens-locales".to_string()]);
        assert_eq!(config.source_locale, "ja");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn loads_legacy_intl_lens_config_before_zed_config() {
        let root = test_workspace("intl-lens-config-priority");
        fs::create_dir_all(root.join(".zed")).expect("create zed dir");
        fs::write(
            root.join(".intl-lens.json"),
            r#"{"localePaths":["intl-locales"],"sourceLocale":"vi"}"#,
        )
        .expect("write intl lens config");
        fs::write(
            root.join(".zed/i18n.json"),
            r#"{"localePaths":["zed-locales"],"sourceLocale":"en"}"#,
        )
        .expect("write zed config");

        let config = I18nConfig::load_from_workspace(&root);

        assert_eq!(config.locale_paths, vec!["intl-locales".to_string()]);
        assert_eq!(config.source_locale, "vi");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ignores_legacy_config_files() {
        let root = test_workspace("legacy-config-ignored");
        fs::create_dir_all(&root).expect("create root");
        let legacy_file = [".i18n", "-ally.json"].concat();
        fs::write(
            root.join(legacy_file),
            r#"{"localePaths":["legacy-locales"],"sourceLocale":"vi"}"#,
        )
        .expect("write legacy config");

        let config = I18nConfig::load_from_workspace(&root);

        assert_ne!(config.locale_paths, vec!["legacy-locales".to_string()]);
        assert_eq!(config.source_locale, "en");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn defaults_to_inlay_hints_display_mode() {
        let config = serde_json::from_str::<I18nConfig>(r#"{}"#).expect("parse config");

        assert_eq!(config.display_mode, DisplayMode::InlayHints);
    }

    #[test]
    fn reads_code_lens_display_mode() {
        let config = serde_json::from_str::<I18nConfig>(r#"{"displayMode":"codeLens"}"#)
            .expect("parse config");

        assert_eq!(config.display_mode, DisplayMode::CodeLens);
    }

    fn test_workspace(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("intl-lens-{name}-{nonce}"))
    }
}
