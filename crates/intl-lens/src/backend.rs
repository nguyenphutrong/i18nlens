use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::config::{DisplayMode, I18nConfig};
use crate::document::DocumentStore;
use crate::i18n::{KeyFinder, TranslationStore};

fn truncate_string(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }

    let truncated: String = s.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{}...", truncated)
}

pub struct I18nBackend {
    client: Client,
    config: Arc<RwLock<I18nConfig>>,
    documents: Arc<RwLock<DocumentStore>>,
    translation_store: Arc<RwLock<Option<TranslationStore>>>,
    key_finder: Arc<RwLock<KeyFinder>>,
    workspace_root: Arc<RwLock<Option<PathBuf>>>,
    inlay_hint_dynamic_registration_supported: Arc<RwLock<bool>>,
    inlay_hint_refresh_supported: Arc<RwLock<bool>>,
    code_lens_refresh_supported: Arc<RwLock<bool>>,
    watched_files_dynamic_registration_supported: Arc<RwLock<bool>>,
    watched_files_relative_pattern_supported: Arc<RwLock<bool>>,
}

impl I18nBackend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            config: Arc::new(RwLock::new(I18nConfig::default())),
            documents: Arc::new(RwLock::new(DocumentStore::new())),
            translation_store: Arc::new(RwLock::new(None)),
            key_finder: Arc::new(RwLock::new(KeyFinder::default())),
            workspace_root: Arc::new(RwLock::new(None)),
            inlay_hint_dynamic_registration_supported: Arc::new(RwLock::new(false)),
            inlay_hint_refresh_supported: Arc::new(RwLock::new(false)),
            code_lens_refresh_supported: Arc::new(RwLock::new(false)),
            watched_files_dynamic_registration_supported: Arc::new(RwLock::new(false)),
            watched_files_relative_pattern_supported: Arc::new(RwLock::new(false)),
        }
    }

    async fn initialize_workspace(&self, root: PathBuf) {
        tracing::info!("Initializing workspace at {:?}", root);

        let config = I18nConfig::load_from_workspace(&root);
        tracing::info!("Config loaded, locale_paths: {:?}", config.locale_paths);

        let key_finder = KeyFinder::new(&config.function_patterns);
        *self.key_finder.write().await = key_finder;

        let store = TranslationStore::new(root.clone());
        store.scan_and_load(&config.locale_paths);

        let locales = store.get_locales();
        let keys = store.get_all_keys();

        tracing::info!("Found {} locales: {:?}", locales.len(), locales);
        tracing::info!("Found {} translation keys", keys.len());

        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "i18n-lsp initialized: {} locales, {} keys in {:?}",
                    locales.len(),
                    keys.len(),
                    root
                ),
            )
            .await;

        *self.translation_store.write().await = Some(store);
        *self.config.write().await = config;
        *self.workspace_root.write().await = Some(root);
    }

    async fn register_inlay_hint_capability(&self) {
        if !self.use_inlay_hints().await {
            tracing::debug!("Skipping inlay hint registration (displayMode=codeLens)");
            return;
        }

        let supports_dynamic = *self.inlay_hint_dynamic_registration_supported.read().await;

        if !supports_dynamic {
            tracing::debug!("Skipping inlay hint dynamic registration (dynamicRegistration=false)");
            return;
        }

        let document_selector = Some(vec![
            DocumentFilter {
                language: Some("typescript".to_string()),
                scheme: None,
                pattern: None,
            },
            DocumentFilter {
                language: Some("typescriptreact".to_string()),
                scheme: None,
                pattern: None,
            },
            DocumentFilter {
                language: Some("javascript".to_string()),
                scheme: None,
                pattern: None,
            },
            DocumentFilter {
                language: Some("javascriptreact".to_string()),
                scheme: None,
                pattern: None,
            },
            DocumentFilter {
                language: Some("html".to_string()),
                scheme: None,
                pattern: None,
            },
            DocumentFilter {
                language: Some("angular".to_string()),
                scheme: None,
                pattern: None,
            },
            DocumentFilter {
                language: Some("php".to_string()),
                scheme: None,
                pattern: None,
            },
            DocumentFilter {
                language: Some("blade".to_string()),
                scheme: None,
                pattern: None,
            },
            DocumentFilter {
                language: Some("dart".to_string()),
                scheme: None,
                pattern: None,
            },
            DocumentFilter {
                language: Some("vue".to_string()),
                scheme: None,
                pattern: None,
            },
            DocumentFilter {
                language: Some("svelte".to_string()),
                scheme: None,
                pattern: None,
            },
        ]);

        let register_options = InlayHintRegistrationOptions {
            inlay_hint_options: InlayHintOptions {
                resolve_provider: Some(false),
                work_done_progress_options: Default::default(),
            },
            text_document_registration_options: TextDocumentRegistrationOptions {
                document_selector,
            },
            static_registration_options: StaticRegistrationOptions {
                id: Some("intl-lens-inlay-hint".to_string()),
            },
        };

        let register_options = match serde_json::to_value(register_options) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    "Failed to serialize inlay hint registration options: {:?}",
                    err
                );
                return;
            }
        };

        let registration = Registration {
            id: "intl-lens-inlay-hint".to_string(),
            method: "textDocument/inlayHint".to_string(),
            register_options: Some(register_options),
        };

        match self.client.register_capability(vec![registration]).await {
            Ok(_) => tracing::info!("Registered inlay hint capability dynamically"),
            Err(err) => tracing::warn!("Dynamic inlay hint registration failed: {:?}", err),
        }
    }

    async fn register_watched_files_capability(&self) {
        let supports_dynamic = *self
            .watched_files_dynamic_registration_supported
            .read()
            .await;

        if !supports_dynamic {
            tracing::debug!("Skipping watched files registration (dynamicRegistration=false)");
            return;
        }

        let locale_paths = { self.config.read().await.locale_paths.clone() };
        let workspace_root = { self.workspace_root.read().await.clone() };
        let relative_pattern_support = *self.watched_files_relative_pattern_supported.read().await;

        let watchers = Self::build_file_watchers(
            &locale_paths,
            workspace_root.as_deref(),
            relative_pattern_support,
        );
        if watchers.is_empty() {
            tracing::debug!("Skipping watched files registration (no locale paths)");
            return;
        }

        let register_options = DidChangeWatchedFilesRegistrationOptions { watchers };
        let register_options = match serde_json::to_value(register_options) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    "Failed to serialize watched files registration options: {:?}",
                    err
                );
                return;
            }
        };

        let registration = Registration {
            id: "intl-lens-watched-files".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: Some(register_options),
        };

        match self.client.register_capability(vec![registration]).await {
            Ok(_) => tracing::info!("Registered watched files capability dynamically"),
            Err(err) => tracing::warn!("Dynamic watched files registration failed: {:?}", err),
        }
    }

    async fn diagnose_document(&self, uri: &Url, content: &str) {
        let diagnostics = self.compute_diagnostics(content).await;

        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
    }

    async fn compute_diagnostics(&self, content: &str) -> Vec<Diagnostic> {
        let key_finder = self.key_finder.read().await;
        let found_keys = key_finder.find_keys(content);

        let translation_store = self.translation_store.read().await;

        let Some(store) = translation_store.as_ref() else {
            return vec![];
        };

        let mut diagnostics = Vec::new();
        let source_locale = self.config.read().await.source_locale.clone();

        for found_key in found_keys {
            if !store.key_exists(&found_key.key) {
                diagnostics.push(Diagnostic {
                    range: Range {
                        start: Position {
                            line: found_key.line as u32,
                            character: found_key.start_char as u32,
                        },
                        end: Position {
                            line: found_key.line as u32,
                            character: found_key.end_char as u32,
                        },
                    },
                    severity: Some(DiagnosticSeverity::WARNING),
                    code: Some(NumberOrString::String("missing-translation".to_string())),
                    source: Some("i18n".to_string()),
                    message: format!("Translation key '{}' not found", found_key.key),
                    ..Default::default()
                });
            } else {
                // Check if the source locale value is a raw placeholder (_key_)
                if let Some(value) = store.get_translation(&found_key.key, &source_locale) {
                    if value.starts_with('_') && value.ends_with('_') && value.len() > 2 {
                        diagnostics.push(Diagnostic {
                            range: Range {
                                start: Position {
                                    line: found_key.line as u32,
                                    character: found_key.start_char as u32,
                                },
                                end: Position {
                                    line: found_key.line as u32,
                                    character: found_key.end_char as u32,
                                },
                            },
                            severity: Some(DiagnosticSeverity::WARNING),
                            code: Some(NumberOrString::String("raw-translation".to_string())),
                            source: Some("i18n".to_string()),
                            message: format!(
                                "Translation '{}' has a raw placeholder value — use Go to Definition to edit",
                                found_key.key
                            ),
                            ..Default::default()
                        });
                        continue;
                    }
                }

                let missing_locales = store.get_missing_locales(&found_key.key);
                if !missing_locales.is_empty() {
                    diagnostics.push(Diagnostic {
                        range: Range {
                            start: Position {
                                line: found_key.line as u32,
                                character: found_key.start_char as u32,
                            },
                            end: Position {
                                line: found_key.line as u32,
                                character: found_key.end_char as u32,
                            },
                        },
                        severity: Some(DiagnosticSeverity::HINT),
                        code: Some(NumberOrString::String("incomplete-translation".to_string())),
                        source: Some("i18n".to_string()),
                        message: format!(
                            "Translation '{}' missing in: {}",
                            found_key.key,
                            missing_locales.join(", ")
                        ),
                        ..Default::default()
                    });
                }
            }
        }

        diagnostics
    }

    async fn get_hover_content(&self, key: &str) -> Option<String> {
        let translation_store = self.translation_store.read().await;
        let config = self.config.read().await;
        let store = translation_store.as_ref()?;

        let translations = store.get_all_translations(key);
        if translations.is_empty() {
            return None;
        }

        let mut content = format!("### 🌍 `{}`\n\n", key);

        let source_locale = &config.source_locale;
        let format_line = |locale: &str| -> Option<String> {
            let entry = translations.get(locale)?;
            let mut line = format!("**{}**: {}", locale, entry.value);

            if let Some(location) = store.get_translation_location(key, locale) {
                if let Ok(uri) = Url::from_file_path(&location.file_path) {
                    let link = format!("{}#L{}", uri, location.line + 1);
                    line.push_str(&format!(" ([↗]({} \"Go to Definition\"))", link));
                }
            }

            line.push_str("\n\n");
            Some(line)
        };

        if let Some(line) = format_line(source_locale) {
            content.push_str(&line);
        }

        content.push_str("---\n\n");

        let mut other_locales: Vec<String> = translations
            .keys()
            .filter(|locale| *locale != source_locale)
            .cloned()
            .collect();
        other_locales.sort();

        for locale in other_locales {
            if let Some(line) = format_line(&locale) {
                content.push_str(&line);
            }
        }

        Some(content)
    }

    async fn get_completions(&self, prefix: &str) -> Vec<CompletionItem> {
        let translation_store = self.translation_store.read().await;
        let config = self.config.read().await;

        let Some(store) = translation_store.as_ref() else {
            return vec![];
        };

        let all_keys = store.get_all_keys();
        let source_locale = &config.source_locale;

        all_keys
            .into_iter()
            .filter(|key| key.starts_with(prefix) || prefix.is_empty())
            .take(100)
            .map(|key| {
                let translation = store.get_translation(&key, source_locale);
                CompletionItem {
                    label: key.clone(),
                    kind: Some(CompletionItemKind::TEXT),
                    detail: translation.clone(),
                    documentation: translation.map(|t| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!("**{}**: {}", source_locale, t),
                        })
                    }),
                    insert_text: Some(key.clone()),
                    ..Default::default()
                }
            })
            .collect()
    }

    fn build_file_watchers(
        locale_paths: &[String],
        workspace_root: Option<&Path>,
        relative_pattern_support: bool,
    ) -> Vec<FileSystemWatcher> {
        let mut patterns = Vec::new();

        for locale_path in locale_paths {
            let trimmed = locale_path.trim_end_matches(['/', '\\']);
            if trimmed.is_empty() {
                continue;
            }

            if Self::is_translation_file_path(trimmed) {
                patterns.push(trimmed.to_string());
                continue;
            }

            for extension in Self::translation_extensions() {
                patterns.push(format!("{}/**/*{}", trimmed, extension));
            }
        }

        patterns.sort();
        patterns.dedup();

        let base_uri = if relative_pattern_support {
            workspace_root.and_then(|root| Url::from_directory_path(root).ok())
        } else {
            None
        };

        patterns
            .into_iter()
            .map(|pattern| {
                let glob_pattern = if let Some(base_uri) = base_uri.clone() {
                    GlobPattern::Relative(RelativePattern {
                        base_uri: OneOf::Right(base_uri),
                        pattern,
                    })
                } else if let Some(root) = workspace_root {
                    GlobPattern::String(Self::to_absolute_pattern(root, &pattern))
                } else {
                    GlobPattern::String(pattern)
                };

                FileSystemWatcher {
                    glob_pattern,
                    kind: None,
                }
            })
            .collect()
    }

    fn to_absolute_pattern(root: &Path, pattern: &str) -> String {
        let mut root_str = root.to_string_lossy().replace('\\', "/");
        root_str = root_str.trim_end_matches('/').to_string();

        if pattern.is_empty() {
            return root_str;
        }

        if pattern.starts_with('/') {
            format!("{}{}", root_str, pattern)
        } else {
            format!("{}/{}", root_str, pattern)
        }
    }

    fn translation_extensions() -> [&'static str; 5] {
        [".json", ".yaml", ".yml", ".php", ".arb"]
    }

    fn has_translation_extension(path: &Path) -> bool {
        let lower = path.to_string_lossy().to_ascii_lowercase();
        Self::translation_extensions()
            .iter()
            .any(|extension| lower.ends_with(extension))
    }

    fn is_translation_file_path(path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        Self::translation_extensions()
            .iter()
            .any(|extension| lower.ends_with(extension))
    }

    fn is_translation_file_in_paths(path: &Path, root: &Path, locale_paths: &[String]) -> bool {
        if !Self::has_translation_extension(path) {
            return false;
        }

        for locale_path in locale_paths {
            let trimmed = locale_path.trim_end_matches(['/', '\\']);
            if trimmed.is_empty() {
                continue;
            }

            if Self::path_matches_locale_glob(path, root, trimmed) {
                return true;
            }

            let candidate = if Path::new(trimmed).is_absolute() {
                PathBuf::from(trimmed)
            } else {
                root.join(trimmed)
            };

            if candidate.is_file() {
                if path == candidate {
                    return true;
                }
            } else if path.starts_with(&candidate) {
                return true;
            }
        }

        false
    }

    fn path_matches_locale_glob(path: &Path, root: &Path, pattern: &str) -> bool {
        if !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[') {
            return false;
        }

        let Ok(glob) = globset::Glob::new(pattern) else {
            return false;
        };
        let matcher = glob.compile_matcher();

        let Ok(relative_path) = path.strip_prefix(root) else {
            return false;
        };

        if matcher.is_match(relative_path) {
            return true;
        }

        relative_path
            .parent()
            .is_some_and(|parent| matcher.is_match(parent))
    }

    async fn is_translation_uri(&self, uri: &Url) -> bool {
        let Some(path) = uri.to_file_path().ok() else {
            return false;
        };

        let workspace_root = { self.workspace_root.read().await.clone() };
        let locale_paths = { self.config.read().await.locale_paths.clone() };

        let Some(root) = workspace_root.as_ref() else {
            return false;
        };

        Self::is_translation_file_in_paths(&path, root, &locale_paths)
    }

    async fn reload_translations(&self) {
        let workspace_root = { self.workspace_root.read().await.clone() };
        let locale_paths = { self.config.read().await.locale_paths.clone() };

        let Some(root) = workspace_root.as_ref() else {
            return;
        };

        let store = TranslationStore::new(root.clone());
        store.scan_and_load(&locale_paths);

        let locales = store.get_locales();
        let keys = store.get_all_keys();

        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "Reloaded translations: {} locales, {} keys",
                    locales.len(),
                    keys.len()
                ),
            )
            .await;

        *self.translation_store.write().await = Some(store);
        self.refresh_active_lenses().await;
    }

    async fn refresh_active_lenses(&self) {
        match self.config.read().await.display_mode {
            DisplayMode::InlayHints => self.refresh_inlay_hints().await,
            DisplayMode::CodeLens => self.refresh_code_lenses().await,
        }
    }

    async fn refresh_inlay_hints(&self) {
        if *self.inlay_hint_refresh_supported.read().await {
            if let Err(err) = self.client.inlay_hint_refresh().await {
                tracing::warn!("Inlay hint refresh failed: {:?}", err);
            }
        }
    }

    async fn refresh_code_lenses(&self) {
        if *self.code_lens_refresh_supported.read().await {
            if let Err(err) = self.client.code_lens_refresh().await {
                tracing::warn!("Code lens refresh failed: {:?}", err);
            }
        }
    }

    async fn use_inlay_hints(&self) -> bool {
        self.config.read().await.display_mode == DisplayMode::InlayHints
    }

    async fn use_code_lens(&self) -> bool {
        self.config.read().await.display_mode == DisplayMode::CodeLens
    }

    async fn re_diagnose_open_documents(&self) {
        let docs = self.documents.read().await;
        let entries: Vec<(String, String)> = docs
            .uris()
            .into_iter()
            .filter_map(|uri| {
                let content = docs.get(&uri)?.content.clone();
                Some((uri, content))
            })
            .collect();
        drop(docs);

        for (uri_str, content) in entries {
            if let Ok(uri) = Url::parse(&uri_str) {
                self.diagnose_document(&uri, &content).await;
            }
        }
    }

    async fn get_definition_locations(&self, key: &str) -> Vec<Location> {
        let translation_store = self.translation_store.read().await;
        let config = self.config.read().await;
        let Some(store) = translation_store.as_ref() else {
            return Vec::new();
        };

        let translations = store.get_all_translations(key);
        if translations.is_empty() {
            return Vec::new();
        }

        let source_locale = &config.source_locale;
        let mut other_locales: Vec<String> = translations
            .keys()
            .filter(|locale| *locale != source_locale)
            .cloned()
            .collect();
        other_locales.sort();

        let mut locales = Vec::new();
        if translations.contains_key(source_locale) {
            locales.push(source_locale.clone());
        }
        locales.extend(other_locales);

        let mut locations = Vec::new();
        for locale in locales {
            if let Some(location) = store.get_translation_location(key, &locale) {
                if let Ok(uri) = Url::from_file_path(&location.file_path) {
                    locations.push(Location {
                        uri,
                        range: Range {
                            start: Position {
                                line: location.line as u32,
                                character: 0,
                            },
                            end: Position {
                                line: location.line as u32,
                                character: 0,
                            },
                        },
                    });
                }
            }
        }

        locations
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for I18nBackend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        tracing::info!("i18n-lsp initialize called");
        tracing::debug!("Client capabilities: {:?}", params.capabilities);

        let inlay_hint_dynamic_registration_support = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|text_document| text_document.inlay_hint.as_ref())
            .and_then(|inlay| inlay.dynamic_registration)
            .unwrap_or(false);

        *self.inlay_hint_dynamic_registration_supported.write().await =
            inlay_hint_dynamic_registration_support;

        let inlay_hint_refresh_supported = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.inlay_hint.as_ref())
            .and_then(|inlay| inlay.refresh_support)
            .unwrap_or(false);

        *self.inlay_hint_refresh_supported.write().await = inlay_hint_refresh_supported;

        let code_lens_refresh_supported = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.code_lens.as_ref())
            .and_then(|code_lens| code_lens.refresh_support)
            .unwrap_or(false);

        *self.code_lens_refresh_supported.write().await = code_lens_refresh_supported;

        let watched_files = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.did_change_watched_files.as_ref());

        let watched_files_dynamic_registration_support = watched_files
            .and_then(|watch| watch.dynamic_registration)
            .unwrap_or(false);

        let watched_files_relative_pattern_support = watched_files
            .and_then(|watch| watch.relative_pattern_support)
            .unwrap_or(false);

        *self
            .watched_files_dynamic_registration_supported
            .write()
            .await = watched_files_dynamic_registration_support;
        *self.watched_files_relative_pattern_supported.write().await =
            watched_files_relative_pattern_support;

        tracing::info!(
            "Client inlay hint dynamicRegistration: {}",
            inlay_hint_dynamic_registration_support
        );
        tracing::info!(
            "Client inlay hint refreshSupport: {}",
            inlay_hint_refresh_supported
        );
        tracing::info!(
            "Client codeLens refreshSupport: {}",
            code_lens_refresh_supported
        );
        tracing::info!(
            "Client didChangeWatchedFiles dynamicRegistration: {}",
            watched_files_dynamic_registration_support
        );
        tracing::info!(
            "Client didChangeWatchedFiles relativePatternSupport: {}",
            watched_files_relative_pattern_support
        );

        let root_path = params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| folder.uri.to_file_path().ok())
            .or_else(|| {
                params
                    .root_uri
                    .as_ref()
                    .and_then(|uri| uri.to_file_path().ok())
            });

        if let Some(root) = root_path {
            self.initialize_workspace(root).await;
        } else {
            tracing::warn!("No workspace root found in initialize params");
        }

        let display_mode = self.config.read().await.display_mode;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        will_save: None,
                        will_save_wait_until: None,
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(false),
                        })),
                    },
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        "\"".to_string(),
                        "'".to_string(),
                        ".".to_string(),
                    ]),
                    ..Default::default()
                }),
                definition_provider: Some(OneOf::Left(true)),
                inlay_hint_provider: (display_mode == DisplayMode::InlayHints).then(|| {
                    OneOf::Right(InlayHintServerCapabilities::Options(InlayHintOptions {
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    }))
                }),
                code_lens_provider: (display_mode == DisplayMode::CodeLens).then_some(
                    CodeLensOptions {
                        resolve_provider: Some(false),
                    },
                ),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    },
                )),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        "i18nlens.createRawTranslationKey".to_string(),
                        "intl-lens.createRawTranslationKey".to_string(),
                        "intlLens.showTranslation".to_string(),
                    ],
                    work_done_progress_options: Default::default(),
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "i18nlens".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "i18nlens server initialized")
            .await;
        self.register_inlay_hint_capability().await;
        self.register_watched_files_capability().await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let mut has_translation_changes = false;
        for change in &params.changes {
            if self.is_translation_uri(&change.uri).await {
                has_translation_changes = true;
                break;
            }
        }

        if has_translation_changes {
            tracing::info!("Translation files changed, reloading...");
            self.reload_translations().await;
        }
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let content = params.text_document.text.clone();
        let version = params.text_document.version;

        {
            let mut docs = self.documents.write().await;
            docs.open(uri.to_string(), content.clone(), version);
        }

        self.diagnose_document(&uri, &content).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();

        if let Some(change) = params.content_changes.into_iter().next_back() {
            let content = change.text;
            let version = params.text_document.version;

            {
                let mut docs = self.documents.write().await;
                docs.update(uri.as_str(), content.clone(), version);
            }

            self.diagnose_document(&uri, &content).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        if self.is_translation_uri(&params.text_document.uri).await {
            tracing::info!("Translation file saved, reloading...");
            self.reload_translations().await;
        } else if self.use_code_lens().await {
            self.refresh_code_lenses().await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let mut docs = self.documents.write().await;
        docs.close(params.text_document.uri.as_str());
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.read().await;
        let Some(doc) = docs.get(uri.as_str()) else {
            return Ok(None);
        };

        let content = doc.content.to_string();
        let key_finder = self.key_finder.read().await;

        let Some(found_key) = key_finder.find_key_at_position(
            &content,
            position.line as usize,
            position.character as usize,
        ) else {
            return Ok(None);
        };

        let Some(hover_content) = self.get_hover_content(&found_key.key).await else {
            return Ok(None);
        };

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: hover_content,
            }),
            range: None,
        }))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let docs = self.documents.read().await;
        let Some(doc) = docs.get(uri.as_str()) else {
            return Ok(None);
        };

        let content = doc.content.to_string();
        let line_content: String = content
            .lines()
            .nth(position.line as usize)
            .unwrap_or("")
            .to_string();

        let Some(prefix) =
            Self::extract_completion_prefix(&line_content, position.character as usize)
        else {
            return Ok(None);
        };

        let completions = self.get_completions(&prefix).await;

        if completions.is_empty() {
            return Ok(None);
        }

        Ok(Some(CompletionResponse::Array(completions)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.read().await;
        let Some(doc) = docs.get(uri.as_str()) else {
            return Ok(None);
        };

        let content = doc.content.to_string();
        let key_finder = self.key_finder.read().await;

        let Some(found_key) = key_finder.find_key_at_position(
            &content,
            position.line as usize,
            position.character as usize,
        ) else {
            return Ok(None);
        };

        let locations = self.get_definition_locations(&found_key.key).await;
        if locations.is_empty() {
            return Ok(None);
        }

        if locations.len() == 1 {
            return Ok(Some(GotoDefinitionResponse::Scalar(locations[0].clone())));
        }

        Ok(Some(GotoDefinitionResponse::Array(locations)))
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        tracing::debug!(">>> code_lens: uri={}", uri);

        if !self.use_code_lens().await {
            return Ok(None);
        }

        let source_locale = self.config.read().await.source_locale.clone();

        let docs = self.documents.read().await;
        let Some(doc) = docs.get(uri.as_str()) else {
            tracing::warn!("<<< code_lens: document NOT in store: {}", uri);
            return Ok(None);
        };

        let content = doc.content.as_str();
        let key_finder = self.key_finder.read().await;
        let found_keys = key_finder.find_keys(content);

        let translation_store = self.translation_store.read().await;
        let Some(store) = translation_store.as_ref() else {
            return Ok(None);
        };

        let mut lenses = Vec::new();

        for found_key in found_keys {
            let Some(translation) = store.get_translation(&found_key.key, &source_locale) else {
                continue;
            };

            let display_text = truncate_string(&translation, 30);
            let start = Position {
                line: found_key.line as u32,
                character: found_key.start_char as u32,
            };
            let end = Position {
                line: found_key.line as u32,
                character: found_key.end_char as u32,
            };

            lenses.push(CodeLens {
                range: Range { start, end },
                command: Some(Command {
                    title: format!("= {}", display_text),
                    command: "intlLens.showTranslation".to_string(),
                    arguments: None,
                }),
                data: None,
            });
        }

        tracing::debug!("<<< code_lens: returning {} lenses", lenses.len());
        Ok(Some(lenses))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let mut actions = Vec::new();

        for diagnostic in &params.context.diagnostics {
            let is_missing = diagnostic
                .code
                .as_ref()
                .map(|c| matches!(c, NumberOrString::String(s) if s == "missing-translation"))
                .unwrap_or(false);

            if !is_missing {
                continue;
            }

            let key = diagnostic
                .message
                .strip_prefix("Translation key '")
                .and_then(|s| s.strip_suffix("' not found"))
                .map(|s| s.to_string());

            let Some(key) = key else {
                continue;
            };

            let action = CodeAction {
                title: format!("Create raw translation key '{}'", key),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic.clone()]),
                command: Some(Command {
                    title: format!("Create raw translation key '{}'", key),
                    command: "i18nlens.createRawTranslationKey".to_string(),
                    arguments: Some(vec![Value::String(key)]),
                }),
                ..Default::default()
            };

            actions.push(CodeActionOrCommand::CodeAction(action));
        }

        if actions.is_empty() {
            return Ok(None);
        }

        Ok(Some(actions))
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        if params.command == "intlLens.showTranslation" {
            return Ok(None);
        }

        if params.command != "i18nlens.createRawTranslationKey"
            && params.command != "intl-lens.createRawTranslationKey"
        {
            return Ok(None);
        }

        let key = params
            .arguments
            .first()
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let Some(key) = key else {
            tracing::warn!("createRawTranslationKey: missing key argument");
            return Ok(None);
        };

        let raw_value = format!("_{}_", key);
        tracing::info!(
            "Creating raw translation key '{}' with value '{}' in all locale files",
            key,
            raw_value
        );

        let translation_store = self.translation_store.read().await;
        let Some(store) = translation_store.as_ref() else {
            tracing::warn!("createRawTranslationKey: no translation store");
            return Ok(None);
        };

        // Collect all locale file paths
        let locales = store.get_locales();
        let mut all_files: Vec<PathBuf> = Vec::new();
        for locale in &locales {
            for path in store.get_locale_file_paths(locale) {
                if !all_files.contains(&path) {
                    all_files.push(path);
                }
            }
        }
        drop(translation_store);

        let mut files_written = 0;
        for file_path in &all_files {
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "json" {
                tracing::debug!("Skipping non-JSON file: {:?}", file_path);
                continue;
            }

            let file_content = match std::fs::read_to_string(file_path) {
                Ok(content) => content,
                Err(e) => {
                    tracing::warn!("Failed to read {:?}: {}", file_path, e);
                    continue;
                }
            };

            let result = Self::insert_key_into_json(&file_content, &key, &raw_value);
            let Some((new_content, _, _)) = result else {
                tracing::warn!("Failed to insert key into {:?}", file_path);
                continue;
            };

            if let Err(e) = std::fs::write(file_path, &new_content) {
                tracing::warn!("Failed to write {:?}: {}", file_path, e);
                continue;
            }

            files_written += 1;
        }

        tracing::info!(
            "Inserted raw key '{}' into {}/{} locale files",
            key,
            files_written,
            all_files.len()
        );

        // Reload translations so the new key is recognized immediately
        self.reload_translations().await;

        // Re-diagnose all open documents to clear stale warnings
        self.re_diagnose_open_documents().await;

        Ok(None)
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        tracing::debug!(">>> inlay_hint: uri={}, range={:?}", uri, params.range);

        if !self.use_inlay_hints().await {
            return Ok(None);
        }

        let source_locale = self.config.read().await.source_locale.clone();

        let docs = self.documents.read().await;
        let Some(doc) = docs.get(uri.as_str()) else {
            tracing::warn!("<<< inlay_hint: document NOT in store: {}", uri);
            return Ok(None);
        };

        let content = doc.content.as_str();
        let key_finder = self.key_finder.read().await;
        let found_keys = key_finder.find_keys(content);

        let translation_store = self.translation_store.read().await;
        let Some(store) = translation_store.as_ref() else {
            return Ok(None);
        };

        let mut hints = Vec::new();
        let request_range = params.range;
        let request_is_empty = request_range.start == request_range.end;

        let position_leq = |a: Position, b: Position| -> bool {
            a.line < b.line || (a.line == b.line && a.character <= b.character)
        };

        let ranges_overlap = |start: Position, end: Position, range: &Range| -> bool {
            position_leq(range.start, end) && position_leq(start, range.end)
        };

        for found_key in found_keys {
            let key_start = Position {
                line: found_key.line as u32,
                character: found_key.start_char as u32,
            };
            let key_end = Position {
                line: found_key.line as u32,
                character: found_key.end_char as u32,
            };

            if !request_is_empty && !ranges_overlap(key_start, key_end, &request_range) {
                continue;
            }

            if let Some(translation) = store.get_translation(&found_key.key, &source_locale) {
                let display_text = truncate_string(&translation, 30);

                let mut hint_char = found_key.end_char;
                if let Some(line) = content.lines().nth(found_key.line) {
                    if matches!(
                        line.encode_utf16().nth(hint_char),
                        Some(unit) if unit == b'\'' as u16 || unit == b'"' as u16
                    ) {
                        hint_char += 1;
                    }
                }

                hints.push(InlayHint {
                    position: Position {
                        line: found_key.line as u32,
                        character: hint_char as u32,
                    },
                    label: InlayHintLabel::String(format!("= {}", display_text)),
                    kind: Some(InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: Some(true),
                    padding_right: None,
                    data: None,
                });
            }
        }

        tracing::debug!("<<< inlay_hint: returning {} hints", hints.len());
        Ok(Some(hints))
    }
}

impl I18nBackend {
    /// Insert a (possibly nested) key into a JSON string with the given value,
    /// using text-based insertion to preserve existing formatting and key order.
    /// Returns `(new_content, cursor_line, cursor_character)`.
    fn insert_key_into_json(content: &str, key: &str, value: &str) -> Option<(String, u32, u32)> {
        // Validate JSON and detect style
        let root: Value = serde_json::from_str(content).ok()?;
        let root_obj = root.as_object()?;
        let parts: Vec<&str> = key.split('.').collect();
        let indent = Self::detect_indent_unit(content);

        // Flat style: all top-level values are non-objects (i.e. no nesting)
        let is_flat = root_obj.values().all(|v| !v.is_object());

        if is_flat || parts.len() == 1 {
            // Insert the full dotted key before the root closing }
            let entry = format!("{}\"{}\": \"{}\"", indent, key, value);
            let brace_offset = content.rfind('}')?;
            let (new_content, insert_line) =
                Self::insert_text_before_offset(content, brace_offset, &entry)?;
            let cursor_line = insert_line as u32;
            let cursor_col = (indent.len() + format!("\"{}\": \"", key).len()) as u32;
            Some((new_content, cursor_line, cursor_col))
        } else {
            // Nested: walk existing parents, then insert remaining structure
            let mut parent_brace_end: Option<usize> = None; // byte offset of parent's closing }
            let mut depth_found = 0usize;

            for i in 0..parts.len() - 1 {
                let range = Self::find_nested_object_range(content, &parts[..=i]);
                if let Some((_, close)) = range {
                    parent_brace_end = Some(close);
                    depth_found = i + 1;
                } else {
                    break;
                }
            }

            let remaining = &parts[depth_found..];
            let base_indent_level = depth_found + 1;

            // Build text for the new key (and any intermediate objects)
            let mut entry_lines = Vec::new();
            for (i, part) in remaining.iter().enumerate() {
                let level = base_indent_level + i;
                if i == remaining.len() - 1 {
                    entry_lines.push(format!(
                        "{}\"{}\": \"{}\"",
                        indent.repeat(level),
                        part,
                        value
                    ));
                } else {
                    entry_lines.push(format!("{}\"{}\": {{", indent.repeat(level), part));
                }
            }
            // Close any intermediate braces we opened (in reverse)
            for i in (0..remaining.len().saturating_sub(1)).rev() {
                let level = base_indent_level + i;
                entry_lines.push(format!("{}}}", indent.repeat(level)));
            }

            let entry = entry_lines.join("\n");

            // Find the closing } offset to insert before
            let brace_offset =
                parent_brace_end.unwrap_or_else(|| content.rfind('}').unwrap_or(content.len()));

            let (new_content, insert_line) =
                Self::insert_text_before_offset(content, brace_offset, &entry)?;

            // Cursor goes on the leaf key's line (the first of the inserted lines
            // if remaining.len() == 1, otherwise deeper)
            let leaf_line_offset = remaining.len() - 1;
            let cursor_line = (insert_line + leaf_line_offset) as u32;
            let leaf_indent_level = base_indent_level + remaining.len() - 1;
            let leaf_part = remaining.last()?;
            let cursor_col = (indent.repeat(leaf_indent_level).len()
                + format!("\"{}\": \"", leaf_part).len()) as u32;

            Some((new_content, cursor_line, cursor_col))
        }
    }

    /// Detect the indentation unit used in a JSON file (e.g. "  ", "    ", or "\t").
    fn detect_indent_unit(content: &str) -> String {
        let mut min_indent: Option<String> = None;
        for line in content.lines() {
            let stripped = line.trim_start();
            if stripped.is_empty() {
                continue;
            }
            let leading: String = line[..line.len() - stripped.len()].to_string();
            if leading.is_empty() {
                continue;
            }
            if leading.starts_with('\t') {
                return "\t".to_string();
            }
            match &min_indent {
                None => min_indent = Some(leading),
                Some(current) if leading.len() < current.len() => {
                    min_indent = Some(leading);
                }
                _ => {}
            }
        }
        min_indent.unwrap_or_else(|| "  ".to_string())
    }

    /// Walk a chain of key segments from the root to find the byte range
    /// `(open_brace, close_brace)` of the innermost object.
    /// E.g. for `["common", "buttons"]` it finds `"common": { "buttons": { ... } }`
    /// and returns the range of the `buttons` object.
    fn find_nested_object_range(content: &str, key_chain: &[&str]) -> Option<(usize, usize)> {
        let mut search_from = 0usize;
        let mut result: Option<(usize, usize)> = None;

        for key_name in key_chain {
            let range = Self::find_key_object_range(content, search_from, key_name)?;
            search_from = range.0 + 1; // search inside this object
            result = Some(range);
        }
        result
    }

    /// Find the byte offsets `(open_brace, close_brace)` of the JSON object
    /// that is the value of `"key_name"`, searching forward from `search_from`.
    fn find_key_object_range(
        content: &str,
        search_from: usize,
        key_name: &str,
    ) -> Option<(usize, usize)> {
        let needle = format!("\"{}\"", key_name);
        let slice = &content[search_from..];
        let key_rel = slice.find(&needle)?;
        let key_abs = search_from + key_rel;
        let after_key = &content[key_abs + needle.len()..];

        // Expect `:` then `{` (with optional whitespace)
        let mut found_colon = false;
        let mut open_brace_abs = None;

        for (i, ch) in after_key.char_indices() {
            match ch {
                ':' if !found_colon => found_colon = true,
                '{' if found_colon => {
                    open_brace_abs = Some(key_abs + needle.len() + i);
                    break;
                }
                c if c.is_whitespace() => continue,
                _ if !found_colon => return None,
                _ => return None, // value is not an object
            }
        }

        let open = open_brace_abs?;

        // Track braces to find matching }, respecting strings
        let mut depth = 1i32;
        let mut in_string = false;
        let mut escape = false;

        for (i, ch) in content[open + 1..].char_indices() {
            if escape {
                escape = false;
                continue;
            }
            match ch {
                '\\' if in_string => escape = true,
                '"' => in_string = !in_string,
                '{' if !in_string => depth += 1,
                '}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((open, open + 1 + i));
                    }
                }
                _ => {}
            }
        }

        None
    }

    /// Insert `entry` text before the `}` at `brace_offset` in `content`.
    /// Adds a trailing comma to the previous entry if needed.
    /// Returns `(new_content, first_inserted_line_number)`.
    fn insert_text_before_offset(
        content: &str,
        brace_offset: usize,
        entry: &str,
    ) -> Option<(String, usize)> {
        let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
        let has_trailing_newline = content.ends_with('\n');

        // Find which line index contains `brace_offset`
        let mut cumulative = 0usize;
        let mut brace_line = lines.len().saturating_sub(1);
        for (i, line_text) in content.lines().enumerate() {
            let line_end = cumulative + line_text.len();
            if brace_offset >= cumulative && brace_offset <= line_end {
                brace_line = i;
                break;
            }
            cumulative = line_end + 1; // +1 for newline
        }

        // Ensure the last content line before the brace has a trailing comma
        for i in (0..brace_line).rev() {
            let trimmed = lines[i].trim();
            if !trimmed.is_empty() {
                if !trimmed.ends_with(',') && !trimmed.ends_with('{') && !trimmed.ends_with('[') {
                    lines[i].push(',');
                }
                break;
            }
        }

        // Splice in the entry lines right before brace_line
        let entry_lines: Vec<String> = entry.lines().map(|l| l.to_string()).collect();
        let insert_at = brace_line;

        let mut new_lines = Vec::with_capacity(lines.len() + entry_lines.len());
        new_lines.extend_from_slice(&lines[..brace_line]);
        new_lines.extend(entry_lines);
        new_lines.extend_from_slice(&lines[brace_line..]);

        let mut result = new_lines.join("\n");
        if has_trailing_newline {
            result.push('\n');
        }

        Some((result, insert_at))
    }

    fn extract_completion_prefix(line: &str, character: usize) -> Option<String> {
        let before_cursor = &line[..character.min(line.len())];

        let quote_patterns = ["t(\"", "t('", "$t(\"", "$t('", "i18n.t(\"", "i18n.t('"];

        for pattern in quote_patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                let after_quote = pos + pattern.len();
                let prefix = &before_cursor[after_quote..];

                if !prefix.contains('"') && !prefix.contains('\'') {
                    return Some(prefix.to_string());
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_flat_key_into_flat_json() {
        let content = "{\n  \"hello\": \"world\",\n  \"foo\": \"bar\"\n}\n";
        let result = I18nBackend::insert_key_into_json(content, "new.key", "_new.key_");
        assert!(result.is_some());
        let (new_content, cursor_line, _cursor_col) = result.unwrap();
        assert!(new_content.contains("\"new.key\": \"_new.key_\""));
        let line = new_content.lines().nth(cursor_line as usize).unwrap();
        assert!(line.contains("\"new.key\": \"_new.key_\""));
    }

    #[test]
    fn test_insert_nested_key_into_existing_parent() {
        let content = "{\n  \"common\": {\n    \"hello\": \"world\"\n  }\n}\n";
        let result =
            I18nBackend::insert_key_into_json(content, "common.goodbye", "_common.goodbye_");
        assert!(result.is_some());
        let (new_content, cursor_line, _cursor_col) = result.unwrap();
        assert!(new_content.contains("\"goodbye\": \"_common.goodbye_\""));
        let line = new_content.lines().nth(cursor_line as usize).unwrap();
        assert!(line.contains("\"goodbye\": \"_common.goodbye_\""));
    }

    #[test]
    fn test_insert_creates_intermediate_objects() {
        let content = "{\n  \"common\": {\n    \"hello\": \"world\"\n  }\n}\n";
        let result =
            I18nBackend::insert_key_into_json(content, "pages.home.title", "_pages.home.title_");
        assert!(result.is_some());
        let (new_content, cursor_line, _cursor_col) = result.unwrap();
        assert!(new_content.contains("\"pages\": {"));
        assert!(new_content.contains("\"home\": {"));
        assert!(new_content.contains("\"title\": \"_pages.home.title_\""));
        let line = new_content.lines().nth(cursor_line as usize).unwrap();
        assert!(line.contains("\"title\": \"_pages.home.title_\""));
    }

    #[test]
    fn test_insert_single_segment_key() {
        let content = "{\n  \"hello\": \"world\"\n}\n";
        let result = I18nBackend::insert_key_into_json(content, "goodbye", "_goodbye_");
        assert!(result.is_some());
        let (new_content, _, _) = result.unwrap();
        assert!(new_content.contains("\"goodbye\": \"_goodbye_\""));
        assert!(new_content.contains("\"hello\": \"world\""));
    }

    #[test]
    fn test_insert_preserves_existing_content() {
        let content = "{\n  \"hello\": \"world\",\n  \"foo\": \"bar\"\n}\n";
        let result = I18nBackend::insert_key_into_json(content, "baz", "_baz_");
        assert!(result.is_some());
        let (new_content, _, _) = result.unwrap();
        assert!(new_content.contains("\"hello\": \"world\""));
        assert!(new_content.contains("\"foo\": \"bar\""));
        assert!(new_content.contains("\"baz\": \"_baz_\""));
    }

    #[test]
    fn test_detect_indent_two_spaces() {
        let content = "{\n  \"hello\": \"world\"\n}";
        assert_eq!(I18nBackend::detect_indent_unit(content), "  ");
    }

    #[test]
    fn test_detect_indent_four_spaces() {
        let content = "{\n    \"hello\": \"world\"\n}";
        assert_eq!(I18nBackend::detect_indent_unit(content), "    ");
    }

    #[test]
    fn test_detect_indent_tab() {
        let content = "{\n\t\"hello\": \"world\"\n}";
        assert_eq!(I18nBackend::detect_indent_unit(content), "\t");
    }

    #[test]
    fn test_translation_file_matches_glob_locale_directory() {
        let root = Path::new("/workspace/project");
        let path = Path::new("/workspace/project/layers/foo/i18n/locales/en.json");
        let locale_paths = vec!["**/*/i18n/locales".to_string()];

        assert!(I18nBackend::is_translation_file_in_paths(
            path,
            root,
            &locale_paths
        ));
    }

    #[test]
    fn test_cursor_position_points_inside_value_quotes() {
        let content = "{\n  \"hello\": \"world\"\n}\n";
        let result = I18nBackend::insert_key_into_json(content, "test", "_test_");
        let (_new_content, cursor_line, cursor_col) = result.unwrap();
        let line = _new_content.lines().nth(cursor_line as usize).unwrap();
        let before_cursor = &line[..cursor_col as usize];
        assert!(
            before_cursor.ends_with("\"test\": \""),
            "cursor should be inside the value quotes, got before_cursor: '{}'",
            before_cursor
        );
    }

    #[test]
    fn test_insert_adds_trailing_comma_to_previous_entry() {
        // No trailing comma after "world"
        let content = "{\n  \"hello\": \"world\"\n}\n";
        let result = I18nBackend::insert_key_into_json(content, "goodbye", "_goodbye_");
        assert!(result.is_some());
        let (new_content, _, _) = result.unwrap();
        assert!(
            new_content.contains("\"hello\": \"world\","),
            "previous entry should get a trailing comma, got:\n{}",
            new_content
        );
    }

    #[test]
    fn test_insert_into_second_nested_parent() {
        let content =
            "{\n  \"buttons\": {\n    \"save\": \"Save\"\n  },\n  \"labels\": {\n    \"name\": \"Name\"\n  }\n}\n";
        let result =
            I18nBackend::insert_key_into_json(content, "buttons.cancel", "_buttons.cancel_");
        assert!(result.is_some());
        let (new_content, cursor_line, _) = result.unwrap();
        assert!(new_content.contains("\"cancel\": \"_buttons.cancel_\""));
        let line = new_content.lines().nth(cursor_line as usize).unwrap();
        assert!(
            line.contains("\"cancel\": \"_buttons.cancel_\""),
            "cancel should appear on the cursor line"
        );
        // "cancel" should be inside "buttons", not at root or inside "labels"
        let buttons_line = new_content
            .lines()
            .position(|l| l.contains("\"buttons\""))
            .unwrap();
        let labels_line = new_content
            .lines()
            .position(|l| l.contains("\"labels\""))
            .unwrap();
        assert!(
            cursor_line as usize > buttons_line && (cursor_line as usize) < labels_line,
            "cancel should be between buttons and labels, cursor_line={}, buttons={}, labels={}",
            cursor_line,
            buttons_line,
            labels_line
        );
    }

    #[test]
    fn test_insert_deeply_nested_into_flat_file_uses_dotted_key() {
        // When the file is flat-style (no nested objects), the key is inserted as a dotted string
        let content = "{\n  \"existing\": \"value\"\n}\n";
        let result = I18nBackend::insert_key_into_json(content, "a.b.c.d", "_a.b.c.d_");
        assert!(result.is_some());
        let (new_content, cursor_line, _) = result.unwrap();
        assert!(
            new_content.contains("\"a.b.c.d\": \"_a.b.c.d_\""),
            "flat-style file should use dotted key, got:\n{}",
            new_content
        );
        let line = new_content.lines().nth(cursor_line as usize).unwrap();
        assert!(line.contains("\"a.b.c.d\": \"_a.b.c.d_\""));
    }

    #[test]
    fn test_insert_deeply_nested_creates_all_parents() {
        // When the file already has nested objects, new keys should be nested too
        let content = "{\n  \"common\": {\n    \"hello\": \"world\"\n  }\n}\n";
        let result = I18nBackend::insert_key_into_json(content, "a.b.c.d", "_a.b.c.d_");
        assert!(result.is_some());
        let (new_content, cursor_line, _) = result.unwrap();
        assert!(new_content.contains("\"a\": {"));
        assert!(new_content.contains("\"b\": {"));
        assert!(new_content.contains("\"c\": {"));
        assert!(new_content.contains("\"d\": \"_a.b.c.d_\""));
        let line = new_content.lines().nth(cursor_line as usize).unwrap();
        assert!(line.contains("\"d\": \"_a.b.c.d_\""));
    }

    #[test]
    fn test_find_nested_object_range_finds_existing() {
        let content = "{\n  \"common\": {\n    \"hello\": \"world\"\n  }\n}";
        let range = I18nBackend::find_nested_object_range(content, &["common"]);
        assert!(range.is_some());
        let (open, close) = range.unwrap();
        assert_eq!(&content[open..=open], "{");
        assert_eq!(&content[close..=close], "}");
    }

    #[test]
    fn test_find_nested_object_range_returns_none_for_missing() {
        let content = "{\n  \"common\": {\n    \"hello\": \"world\"\n  }\n}";
        let range = I18nBackend::find_nested_object_range(content, &["missing"]);
        assert!(range.is_none());
    }

    #[test]
    fn test_invalid_json_returns_none() {
        let content = "not valid json";
        let result = I18nBackend::insert_key_into_json(content, "test", "_test_");
        assert!(result.is_none());
    }
}
