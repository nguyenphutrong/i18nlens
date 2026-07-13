use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use dashmap::DashMap;
use globset::Glob;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use super::parser::TranslationParser;

#[derive(Debug, Clone)]
pub struct TranslationEntry {
    pub value: String,
    pub file_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationLocation {
    pub file_path: PathBuf,
    pub line: usize,
}

pub struct TranslationStore {
    translations: DashMap<String, HashMap<String, TranslationEntry>>,
    locale_files: DashMap<String, HashSet<PathBuf>>,
    workspace_root: PathBuf,
    namespace_enabled: bool,
}

impl TranslationStore {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            translations: DashMap::new(),
            locale_files: DashMap::new(),
            workspace_root,
            namespace_enabled: false,
        }
    }

    /// When enabled, JSON/YAML locale files nested as `<namespace>/<locale-dir>/<locale>.json`
    /// (e.g. `features/landing/locales/en.json`) get their keys prefixed with `<namespace>`,
    /// matching how `useTranslations("<namespace>")`-scoped keys are resolved by the key finder.
    pub fn with_namespace_enabled(mut self, enabled: bool) -> Self {
        self.namespace_enabled = enabled;
        self
    }

    pub fn scan_and_load(&self, locale_paths: &[String]) {
        for locale_path in locale_paths {
            let trimmed = locale_path.trim_end_matches(['/', '\\']);
            if trimmed.is_empty() {
                continue;
            }

            if has_glob_meta(trimmed) {
                self.scan_glob_path(trimmed);
                continue;
            }

            let full_path = self.workspace_root.join(trimmed);
            if full_path.is_file() {
                self.scan_file(&full_path);
            } else if full_path.exists() {
                self.scan_directory(&full_path);
            }
        }
    }

    fn scan_glob_path(&self, locale_path: &str) {
        let Ok(glob) = Glob::new(locale_path) else {
            tracing::warn!("Invalid locale path glob: {}", locale_path);
            return;
        };
        let matcher = glob.compile_matcher();

        for entry in WalkDir::new(&self.workspace_root)
            .into_iter()
            .filter_entry(|entry| !is_ignored_workspace_dir(entry.path()))
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            let Ok(relative_path) = path.strip_prefix(&self.workspace_root) else {
                continue;
            };

            if !matcher.is_match(relative_path) {
                continue;
            }

            if path.is_dir() {
                self.scan_directory(path);
            } else if path.is_file() {
                self.scan_file(path);
            }
        }
    }

    fn scan_directory(&self, dir: &Path) {
        let json_glob = Glob::new("*.json").unwrap().compile_matcher();
        let yaml_glob = Glob::new("*.{yaml,yml}").unwrap().compile_matcher();
        let php_glob = Glob::new("*.php").unwrap().compile_matcher();
        let arb_glob = Glob::new("*.arb").unwrap().compile_matcher();

        for entry in WalkDir::new(dir)
            .max_depth(3)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            let file_name = path.file_name().unwrap_or_default();

            if path.is_file()
                && (json_glob.is_match(file_name)
                    || yaml_glob.is_match(file_name)
                    || php_glob.is_match(file_name)
                    || arb_glob.is_match(file_name))
            {
                self.scan_file(path);
            }
        }
    }

    fn scan_file(&self, path: &Path) {
        if let Some(locale) = self.extract_locale_from_path(path) {
            self.locale_files
                .entry(locale.clone())
                .or_default()
                .insert(path.to_path_buf());
            self.load_translation_file(path, &locale);
        }
    }

    fn extract_locale_from_path(&self, path: &Path) -> Option<String> {
        let file_stem = path.file_stem()?.to_str()?;

        if is_locale_code(file_stem) {
            return Some(file_stem.to_string());
        }

        // Handle ARB naming convention: app_en.arb, app_es.arb, etc.
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if ext == "arb" {
                // Try to extract locale from patterns like "app_en" or "messages_en_US"
                if let Some(locale) = extract_locale_from_arb_filename(file_stem) {
                    return Some(locale);
                }
            }
        }

        if let Some(parent) = path.parent() {
            if let Some(parent_name) = parent.file_name().and_then(|n| n.to_str()) {
                if is_locale_code(parent_name) {
                    return Some(parent_name.to_string());
                }
            }
        }

        None
    }

    /// For `<namespace>/<locale-dir>/<locale>.json`-style layouts (e.g. feature-colocated
    /// `features/landing/locales/en.json`), the namespace is the directory one level above
    /// the file's own directory. Only applied when `namespace_enabled` is set, since a bare
    /// `locales/en.json` layout has no such namespace segment to derive.
    fn locale_namespace_prefix(&self, path: &Path) -> Option<String> {
        if !self.namespace_enabled {
            return None;
        }

        let locale_dir = path.parent()?;
        let namespace_dir = locale_dir.parent()?;
        if namespace_dir == self.workspace_root {
            return None;
        }

        let namespace = namespace_dir.file_name()?.to_str()?;
        if namespace.is_empty() {
            return None;
        }

        Some(namespace.to_string())
    }

    fn load_translation_file(&self, path: &Path, locale: &str) {
        match TranslationParser::parse_file(path) {
            Ok(translations) => {
                let mut locale_map = self.translations.entry(locale.to_string()).or_default();
                let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let prefix = if extension == "php"
                    && !file_stem.is_empty()
                    && !is_locale_code(file_stem)
                {
                    Some(file_stem.to_string())
                } else {
                    self.locale_namespace_prefix(path)
                };

                for (key, value) in translations {
                    let full_key = match &prefix {
                        Some(prefix) => format!("{}.{}", prefix, key),
                        None => key,
                    };

                    locale_map.insert(
                        full_key,
                        TranslationEntry {
                            value,
                            file_path: path.to_path_buf(),
                        },
                    );
                }

                tracing::debug!(
                    "Loaded {} translations from {:?} for locale {}",
                    locale_map.len(),
                    path,
                    locale
                );
            }
            Err(e) => {
                tracing::warn!("Failed to parse {:?}: {}", path, e);
            }
        }
    }

    pub fn get_translation(&self, key: &str, locale: &str) -> Option<String> {
        self.translations
            .get(locale)
            .and_then(|map| map.get(key).map(|e| e.value.clone()))
    }

    pub fn get_all_translations(&self, key: &str) -> HashMap<String, TranslationEntry> {
        let mut result = HashMap::new();
        for entry in self.translations.iter() {
            let locale = entry.key();
            if let Some(translation) = entry.value().get(key) {
                result.insert(locale.clone(), translation.clone());
            }
        }
        result
    }

    pub fn get_translation_location(&self, key: &str, locale: &str) -> Option<TranslationLocation> {
        self.translations.get(locale).and_then(|map| {
            map.get(key).map(|e| {
                let line = Self::find_key_line_in_file(&e.file_path, key).unwrap_or(0);
                TranslationLocation {
                    file_path: e.file_path.clone(),
                    line,
                }
            })
        })
    }

    fn find_key_line_in_file(file_path: &Path, key: &str) -> Option<usize> {
        let content = std::fs::read_to_string(file_path).ok()?;

        let last_part = key.split('.').next_back().unwrap_or(key);
        let search_patterns = [
            format!("\"{}\"", last_part),
            format!("'{}'", last_part),
            format!("{}: ", last_part),
            format!("{}:", last_part),
        ];

        for (line_num, line) in content.lines().enumerate() {
            for pattern in &search_patterns {
                if line.contains(pattern) {
                    return Some(line_num);
                }
            }
        }

        None
    }

    pub fn get_all_keys(&self) -> Vec<String> {
        let mut keys = std::collections::HashSet::new();
        for entry in self.translations.iter() {
            for key in entry.value().keys() {
                keys.insert(key.clone());
            }
        }
        keys.into_iter().collect()
    }

    pub fn get_locales(&self) -> Vec<String> {
        self.translations.iter().map(|e| e.key().clone()).collect()
    }

    pub fn key_exists(&self, key: &str) -> bool {
        self.translations
            .iter()
            .any(|entry| entry.value().contains_key(key))
    }

    pub fn get_locale_file_paths(&self, locale: &str) -> Vec<PathBuf> {
        let mut result: Vec<PathBuf> = self
            .locale_files
            .get(locale)
            .map(|set| set.value().iter().cloned().collect())
            .unwrap_or_default();
        result.sort();
        result
    }

    pub fn get_missing_locales(&self, key: &str) -> Vec<String> {
        let all_locales: Vec<String> = self.get_locales();
        all_locales
            .into_iter()
            .filter(|locale| {
                self.translations
                    .get(locale)
                    .map(|m| !m.contains_key(key))
                    .unwrap_or(true)
            })
            .collect()
    }
}

fn is_locale_code(s: &str) -> bool {
    let locale_patterns = [
        r"^[a-z]{2}$",
        r"^[a-z]{2}[-_][A-Z]{2}$",
        r"^[a-z]{2}[-_][a-z]{2}$",
    ];

    for pattern in &locale_patterns {
        if regex::Regex::new(pattern).unwrap().is_match(s) {
            return true;
        }
    }

    let common_locales = [
        "en", "en-US", "en-GB", "es", "es-ES", "fr", "fr-FR", "de", "de-DE", "it", "it-IT", "pt",
        "pt-BR", "ja", "ja-JP", "ko", "ko-KR", "zh", "zh-CN", "zh-TW", "ru", "ru-RU", "ar",
        "ar-SA", "vi", "vi-VN",
    ];

    common_locales.contains(&s)
}

fn has_glob_meta(path: &str) -> bool {
    path.contains('*') || path.contains('?') || path.contains('[')
}

fn is_ignored_workspace_dir(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }

    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                "node_modules" | ".git" | "target" | "dist" | "build" | ".nuxt" | ".output"
            )
        })
}

/// Extract locale from ARB filename patterns like "app_en", "messages_en_US", "intl_vi"
fn extract_locale_from_arb_filename(file_stem: &str) -> Option<String> {
    // Common ARB file prefixes
    let prefixes = ["app_", "intl_", "messages_", "l10n_", "strings_"];

    for prefix in prefixes {
        if let Some(locale_part) = file_stem.strip_prefix(prefix) {
            if is_locale_code(locale_part) {
                return Some(locale_part.to_string());
            }
        }
    }

    // Try splitting by underscore and check if last part(s) form a locale
    let parts: Vec<&str> = file_stem.split('_').collect();
    if parts.len() >= 2 {
        // Try last part as locale (e.g., "app_en" -> "en")
        let last = parts[parts.len() - 1];
        if is_locale_code(last) {
            return Some(last.to_string());
        }

        // Try last two parts as locale (e.g., "app_en_US" -> "en_US")
        if parts.len() >= 3 {
            let locale = format!("{}_{}", parts[parts.len() - 2], parts[parts.len() - 1]);
            if is_locale_code(&locale) {
                return Some(locale);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn scan_and_load_expands_glob_locale_directories() {
        let root = test_workspace("glob-locale-directories");
        let locale_dir = root.join("layers/foo/i18n/locales");
        fs::create_dir_all(&locale_dir).expect("create locale dir");
        fs::write(
            locale_dir.join("en.json"),
            r#"{"foo":{"greeting":"Hello from layer"}}"#,
        )
        .expect("write en locale");

        let store = TranslationStore::new(root.clone());
        store.scan_and_load(&["**/*/i18n/locales".to_string()]);

        assert_eq!(
            store.get_translation("foo.greeting", "en").as_deref(),
            Some("Hello from layer")
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn namespace_enabled_prefixes_keys_with_feature_folder_name() {
        let root = test_workspace("namespace-enabled");
        let locale_dir = root.join("features/landing/locales");
        fs::create_dir_all(&locale_dir).expect("create locale dir");
        fs::write(
            locale_dir.join("en.json"),
            r#"{"features":{"items":{"loans":{"title":"End-to-end loan management"}}}}"#,
        )
        .expect("write en locale");

        let store =
            TranslationStore::new(root.clone()).with_namespace_enabled(true);
        store.scan_and_load(&["features/*/locales".to_string()]);

        assert_eq!(
            store
                .get_translation("landing.features.items.loans.title", "en")
                .as_deref(),
            Some("End-to-end loan management")
        );
        assert!(store.get_translation("features.items.loans.title", "en").is_none());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn namespace_disabled_leaves_keys_unprefixed() {
        let root = test_workspace("namespace-disabled");
        let locale_dir = root.join("features/landing/locales");
        fs::create_dir_all(&locale_dir).expect("create locale dir");
        fs::write(locale_dir.join("en.json"), r#"{"hello":"Hi"}"#).expect("write en locale");

        let store = TranslationStore::new(root.clone());
        store.scan_and_load(&["features/*/locales".to_string()]);

        assert_eq!(store.get_translation("hello", "en").as_deref(), Some("Hi"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scan_and_load_expands_glob_locale_files() {
        let root = test_workspace("glob-locale-files");
        let locale_dir = root.join("layers/foo/i18n/locales");
        fs::create_dir_all(&locale_dir).expect("create locale dir");
        fs::write(locale_dir.join("fr.json"), r#"{"hello":"Bonjour"}"#).expect("write fr locale");

        let store = TranslationStore::new(root.clone());
        store.scan_and_load(&["layers/*/i18n/locales/*.json".to_string()]);

        assert_eq!(
            store.get_translation("hello", "fr").as_deref(),
            Some("Bonjour")
        );

        fs::remove_dir_all(root).ok();
    }

    fn test_workspace(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("intl-lens-{name}-{nonce}"))
    }
}
