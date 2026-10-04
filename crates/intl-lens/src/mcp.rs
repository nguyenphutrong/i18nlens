use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use intl_lens::audit::{
    AuditReport, AuditResult, FixSuggestion, MissingTranslation, PlaceholderIssue,
};
use intl_lens::config::I18nConfig;
use intl_lens::i18n::store::TranslationStore;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use walkdir::WalkDir;

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct AuditParams {
    workspace: Option<PathBuf>,
    scope: Option<String>,
    include_suggestions: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct MissingTranslationsParams {
    workspace: Option<PathBuf>,
    locales: Vec<String>,
    include_context: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct SuggestTranslationFixesParams {
    workspace: Option<PathBuf>,
    key: String,
    target_locales: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TranslateMissingKeysParams {
    workspace: Option<PathBuf>,
    dry_run: Option<bool>,
    translations: Vec<TranslationPatchInput>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ApplyTranslationPatchParams {
    workspace: Option<PathBuf>,
    dry_run: Option<bool>,
    translations: Vec<TranslationPatchInput>,
}

#[derive(Debug, Deserialize)]
struct TranslationPatchInput {
    key: String,
    locale: String,
    value: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ValidatePlaceholdersParams {
    workspace: Option<PathBuf>,
    key: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TranslationContextParams {
    workspace: Option<PathBuf>,
    key: String,
    include_usage: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ReviewI18nPrParams {
    workspace: Option<PathBuf>,
    fail_on: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ExtractHardcodedStringsParams {
    workspace: Option<PathBuf>,
    paths: Vec<PathBuf>,
    min_length: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ServerInfo {
    name: &'static str,
    version: &'static str,
}

#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct ResourceDefinition {
    uri: String,
    name: String,
    description: String,
    mime_type: String,
}

#[derive(Debug, Serialize)]
struct ResourceContents {
    uri: String,
    mime_type: String,
    text: String,
}

#[derive(Debug, Serialize)]
struct FixSuggestionResponse {
    key: String,
    source_locale: String,
    source_value: String,
    target_locales: Vec<String>,
    files_to_edit: Vec<PathBuf>,
    suggestion: FixSuggestion,
}

struct McpServer {
    workspace_root: PathBuf,
}

impl McpServer {
    fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone();

        let response = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params),
            "notifications/initialized" => Ok(Value::Null),
            "ping" => Ok(json!({ "pong": true })),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => self.handle_tool_call(request.params),
            "resources/list" => self.handle_resources_list(),
            "resources/read" => self.handle_resource_read(request.params),
            method => Err(anyhow!("Unsupported method: {method}")),
        };

        match response {
            Ok(result) => JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(result),
                error: None,
            },
            Err(error) => JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32000,
                    message: error.to_string(),
                    data: None,
                }),
            },
        }
    }

    fn handle_initialize(&self, _params: Option<Value>) -> Result<Value> {
        Ok(json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": ServerInfo {
                name: "i18nlens-mcp",
                version: env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "listChanged": false }
            }
        }))
    }

    fn handle_tool_call(&self, params: Option<Value>) -> Result<Value> {
        let params = params.unwrap_or(Value::Null);
        let tool_name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Missing tool name"))?;
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

        let result = match tool_name {
            "audit_i18n" => {
                let arguments: AuditParams = serde_json::from_value(arguments)?;
                self.audit_i18n(arguments)?
            }
            "get_missing_translations" => {
                let arguments: MissingTranslationsParams = serde_json::from_value(arguments)?;
                self.get_missing_translations(arguments)?
            }
            "suggest_translation_fixes" => {
                let arguments: SuggestTranslationFixesParams = serde_json::from_value(arguments)?;
                self.suggest_translation_fixes(arguments)?
            }
            "translate_missing_keys" => {
                let arguments: TranslateMissingKeysParams = serde_json::from_value(arguments)?;
                self.translate_missing_keys(arguments)?
            }
            "apply_translation_patch" => {
                let arguments: ApplyTranslationPatchParams = serde_json::from_value(arguments)?;
                self.apply_translation_patch(arguments)?
            }
            "validate_placeholders" => {
                let arguments: ValidatePlaceholdersParams = serde_json::from_value(arguments)?;
                self.validate_placeholders(arguments)?
            }
            "get_translation_context" => {
                let arguments: TranslationContextParams = serde_json::from_value(arguments)?;
                self.get_translation_context(arguments)?
            }
            "review_i18n_pr" => {
                let arguments: ReviewI18nPrParams = serde_json::from_value(arguments)?;
                self.review_i18n_pr(arguments)?
            }
            "extract_hardcoded_strings" => {
                let arguments: ExtractHardcodedStringsParams = serde_json::from_value(arguments)?;
                self.extract_hardcoded_strings(arguments)?
            }
            name => return Err(anyhow!("Unknown tool: {name}")),
        };

        Ok(json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&result)?
            }],
            "structuredContent": result,
            "isError": false
        }))
    }

    fn handle_resources_list(&self) -> Result<Value> {
        let resources = vec![
            ResourceDefinition {
                uri: "i18nlens://config".to_string(),
                name: "I18n Lens Config".to_string(),
                description: "Resolved i18n configuration for the current workspace".to_string(),
                mime_type: "application/json".to_string(),
            },
            ResourceDefinition {
                uri: "i18nlens://audit/latest".to_string(),
                name: "Latest Audit Report".to_string(),
                description: "Fresh audit report generated from the current workspace".to_string(),
                mime_type: "application/json".to_string(),
            },
            ResourceDefinition {
                uri: "i18nlens://translations/index".to_string(),
                name: "Translation Inventory".to_string(),
                description: "Loaded locales and translation key count".to_string(),
                mime_type: "application/json".to_string(),
            },
        ];

        Ok(json!({ "resources": resources }))
    }

    fn handle_resource_read(&self, params: Option<Value>) -> Result<Value> {
        let uri = params
            .as_ref()
            .and_then(|value| value.get("uri"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Missing resource uri"))?;

        let contents = match uri {
            "i18nlens://config" | "intl-lens://config" => {
                let config = I18nConfig::load_from_workspace(&self.workspace_root);
                vec![ResourceContents {
                    uri: uri.to_string(),
                    mime_type: "application/json".to_string(),
                    text: serde_json::to_string_pretty(&config)?,
                }]
            }
            "i18nlens://audit/latest" | "intl-lens://audit/latest" => {
                let report = self.build_report(&self.workspace_root)?;
                vec![ResourceContents {
                    uri: uri.to_string(),
                    mime_type: "application/json".to_string(),
                    text: serde_json::to_string_pretty(&report)?,
                }]
            }
            "i18nlens://translations/index" | "intl-lens://translations/index" => {
                let (_, store) = self.load_store(&self.workspace_root);
                let payload = json!({
                    "workspace": self.workspace_root,
                    "locales": store.get_locales(),
                    "total_keys": store.get_all_keys().len()
                });
                vec![ResourceContents {
                    uri: uri.to_string(),
                    mime_type: "application/json".to_string(),
                    text: serde_json::to_string_pretty(&payload)?,
                }]
            }
            _ => return Err(anyhow!("Unknown resource uri: {uri}")),
        };

        Ok(json!({ "contents": contents }))
    }

    fn audit_i18n(&self, params: AuditParams) -> Result<Value> {
        let workspace = self.resolve_workspace(params.workspace);
        let report = self.build_report(&workspace)?;

        let include_suggestions = params.include_suggestions;
        let scope = params.scope.unwrap_or_else(|| "workspace".to_string());

        let missing = if include_suggestions {
            serde_json::to_value(&report.missing)?
        } else {
            serde_json::to_value(strip_missing_suggestions(&report.missing))?
        };

        Ok(json!({
            "workspace": workspace,
            "scope": scope,
            "summary": report.summary,
            "missing": missing,
            "unused": report.unused,
            "placeholder_issues": report.placeholder_issues
        }))
    }

    fn get_missing_translations(&self, params: MissingTranslationsParams) -> Result<Value> {
        let workspace = self.resolve_workspace(params.workspace);
        let report = self.build_report(&workspace)?;

        let filtered: Vec<Value> = report
            .missing
            .into_iter()
            .filter_map(|item| {
                let missing_in = if params.locales.is_empty() {
                    item.missing_in.clone()
                } else {
                    item.missing_in
                        .iter()
                        .filter(|locale| params.locales.contains(*locale))
                        .cloned()
                        .collect()
                };

                if missing_in.is_empty() {
                    return None;
                }

                let mut value = json!({
                    "key": item.key,
                    "source_locale": item.source_locale,
                    "source_value": item.source_value,
                    "missing_in": missing_in,
                });

                if params.include_context {
                    value["used_in"] = serde_json::to_value(item.used_in).ok()?;
                    value["suggestion"] = serde_json::to_value(item.suggestion).ok()?;
                }

                Some(value)
            })
            .collect();

        Ok(json!({
            "workspace": workspace,
            "requested_locales": params.locales,
            "count": filtered.len(),
            "missing": filtered
        }))
    }

    fn suggest_translation_fixes(&self, params: SuggestTranslationFixesParams) -> Result<Value> {
        if params.key.trim().is_empty() {
            return Err(anyhow!("'key' is required"));
        }

        let workspace = self.resolve_workspace(params.workspace);
        let report = self.build_report(&workspace)?;
        let missing = report
            .missing
            .into_iter()
            .find(|item| item.key == params.key)
            .ok_or_else(|| anyhow!("No missing translation found for key '{}'", params.key))?;

        let target_locales = if params.target_locales.is_empty() {
            missing.missing_in.clone()
        } else {
            let filtered: Vec<String> = missing
                .missing_in
                .iter()
                .filter(|locale| params.target_locales.contains(*locale))
                .cloned()
                .collect();

            if filtered.is_empty() {
                return Err(anyhow!(
                    "Key '{}' is not missing in the requested locales",
                    params.key
                ));
            }

            filtered
        };

        let mut files_to_edit = Vec::new();
        for locale in &target_locales {
            if let Some(path) = find_locale_file(&workspace, locale) {
                files_to_edit.push(path);
            }
        }

        let suggestion = missing.suggestion.unwrap_or(FixSuggestion {
            action: "add_translation".to_string(),
            files_to_edit: files_to_edit.clone(),
            context: Some(format!("Translation for '{}'", params.key)),
        });

        let response = FixSuggestionResponse {
            key: params.key,
            source_locale: missing.source_locale,
            source_value: missing.source_value,
            target_locales,
            files_to_edit,
            suggestion,
        };

        Ok(serde_json::to_value(response)?)
    }

    fn translate_missing_keys(&self, params: TranslateMissingKeysParams) -> Result<Value> {
        if params.dry_run == Some(false) {
            return Err(anyhow!(
                "translate_missing_keys only supports dry_run=true. Use apply_translation_patch when write mode is available."
            ));
        }

        let workspace = self.resolve_workspace(params.workspace);
        let patch_plan = self.plan_translation_patches(&workspace, params.translations)?;

        Ok(json!({
            "workspace": workspace,
            "dry_run": true,
            "patches": patch_plan.patches.iter().map(|patch| {
                json!({
                    "key": patch.key,
                    "locale": patch.locale,
                    "file": patch.file,
                    "unified_diff": patch.unified_diff
                })
            }).collect::<Vec<_>>(),
            "skipped": patch_plan.skipped
        }))
    }

    fn apply_translation_patch(&self, params: ApplyTranslationPatchParams) -> Result<Value> {
        let dry_run = params.dry_run.unwrap_or(true);
        let workspace = self.resolve_workspace(params.workspace);
        let patch_plan = self.plan_translation_patches(&workspace, params.translations)?;

        if !dry_run {
            for patch in &patch_plan.patches {
                std::fs::write(&patch.file, &patch.after).with_context(|| {
                    format!("Failed to write locale file {}", patch.file.display())
                })?;
            }
        }

        Ok(json!({
            "workspace": workspace,
            "dry_run": dry_run,
            "applied": if dry_run { 0 } else { patch_plan.patches.len() },
            "patches": patch_plan.patches.iter().map(|patch| {
                json!({
                    "key": patch.key,
                    "locale": patch.locale,
                    "file": patch.file,
                    "unified_diff": patch.unified_diff
                })
            }).collect::<Vec<_>>(),
            "skipped": patch_plan.skipped
        }))
    }

    fn validate_placeholders(&self, params: ValidatePlaceholdersParams) -> Result<Value> {
        if params.key.trim().is_empty() {
            return Err(anyhow!("'key' is required"));
        }

        let workspace = self.resolve_workspace(params.workspace);
        let report = self.build_report(&workspace)?;
        let issues: Vec<PlaceholderIssue> = report
            .placeholder_issues
            .into_iter()
            .filter(|issue| issue.key == params.key)
            .collect();

        let valid = issues.is_empty();

        Ok(json!({
            "workspace": workspace,
            "key": params.key,
            "valid": valid,
            "issues": issues
        }))
    }

    fn get_translation_context(&self, params: TranslationContextParams) -> Result<Value> {
        if params.key.trim().is_empty() {
            return Err(anyhow!("'key' is required"));
        }

        let workspace = self.resolve_workspace(params.workspace);
        let (config, store) = self.load_store(&workspace);
        let mut audit = AuditResult::new(workspace.clone(), config.clone(), store);
        audit.scan_codebase();
        let report = audit.generate_report();

        let all_translations = audit.store.get_all_translations(&params.key);
        let mut translations: Vec<Value> = all_translations
            .into_iter()
            .map(|(locale, entry)| {
                json!({
                    "locale": locale,
                    "value": entry.value,
                    "file": entry.file_path
                })
            })
            .collect();
        translations.sort_by(|left, right| {
            left["locale"]
                .as_str()
                .unwrap_or_default()
                .cmp(right["locale"].as_str().unwrap_or_default())
        });

        let missing_in = report
            .missing
            .iter()
            .find(|item| item.key == params.key)
            .map(|item| item.missing_in.clone())
            .unwrap_or_default();
        let used_in = if params.include_usage {
            report
                .missing
                .iter()
                .find(|item| item.key == params.key)
                .map(|item| item.used_in.clone())
                .or_else(|| audit.used_keys.get(&params.key).cloned())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let files_to_edit: Vec<PathBuf> = missing_in
            .iter()
            .filter_map(|locale| find_locale_file(&workspace, locale))
            .collect();

        Ok(json!({
            "workspace": workspace,
            "key": params.key,
            "source_locale": config.source_locale,
            "source_value": audit.store.get_translation(&params.key, &config.source_locale),
            "translations": translations,
            "missing_in": missing_in,
            "used_in": used_in,
            "files_to_edit": files_to_edit
        }))
    }

    fn review_i18n_pr(&self, params: ReviewI18nPrParams) -> Result<Value> {
        let workspace = self.resolve_workspace(params.workspace);
        let report = self.build_report(&workspace)?;
        let fail_on = if params.fail_on.is_empty() {
            vec!["missing".to_string(), "placeholder".to_string()]
        } else {
            params.fail_on
        };
        let blocking = review_should_block(&report, &fail_on);
        let findings = review_findings(&report, &fail_on);
        let markdown = review_markdown(&report, blocking, &findings);

        Ok(json!({
            "workspace": workspace,
            "blocking": blocking,
            "fail_on": fail_on,
            "summary": report.summary,
            "findings": findings,
            "markdown": markdown
        }))
    }

    fn extract_hardcoded_strings(&self, params: ExtractHardcodedStringsParams) -> Result<Value> {
        let workspace = self.resolve_workspace(params.workspace);
        let min_length = params.min_length.unwrap_or(3);
        let limit = params.limit.unwrap_or(100);
        let roots = if params.paths.is_empty() {
            vec![workspace.clone()]
        } else {
            params
                .paths
                .into_iter()
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        workspace.join(path)
                    }
                })
                .collect()
        };

        let mut candidates = Vec::new();
        for root in roots {
            for file in source_files(&root) {
                let content = std::fs::read_to_string(&file)
                    .with_context(|| format!("Failed to read source file {}", file.display()))?;
                candidates.extend(find_hardcoded_strings(
                    &workspace,
                    &file,
                    &content,
                    min_length,
                    limit.saturating_sub(candidates.len()),
                ));
                if candidates.len() >= limit {
                    break;
                }
            }
            if candidates.len() >= limit {
                break;
            }
        }

        Ok(json!({
            "workspace": workspace,
            "count": candidates.len(),
            "candidates": candidates
        }))
    }

    fn resolve_workspace(&self, override_path: Option<PathBuf>) -> PathBuf {
        override_path.unwrap_or_else(|| self.workspace_root.clone())
    }

    fn build_report(&self, workspace: &Path) -> Result<AuditReport> {
        let (config, store) = self.load_store(workspace);
        let mut audit = AuditResult::new(workspace.to_path_buf(), config, store);
        audit.scan_codebase();
        Ok(audit.generate_report())
    }

    fn load_store(&self, workspace: &Path) -> (I18nConfig, TranslationStore) {
        let config = I18nConfig::load_from_workspace(workspace);
        let store = TranslationStore::new(workspace.to_path_buf())
            .with_namespace_enabled(config.namespace_enabled);
        store.scan_and_load(&config.locale_paths);
        (config, store)
    }

    fn plan_translation_patches(
        &self,
        workspace: &Path,
        translations: Vec<TranslationPatchInput>,
    ) -> Result<TranslationPatchPlan> {
        let report = self.build_report(workspace)?;
        let mut patches = Vec::new();
        let mut skipped = Vec::new();

        for translation in translations {
            if translation.key.trim().is_empty() || translation.locale.trim().is_empty() {
                skipped.push(json!({
                    "key": translation.key,
                    "locale": translation.locale,
                    "reason": "key and locale are required"
                }));
                continue;
            }

            let Some(missing) = report.missing.iter().find(|item| {
                item.key == translation.key && item.missing_in.contains(&translation.locale)
            }) else {
                skipped.push(json!({
                    "key": translation.key,
                    "locale": translation.locale,
                    "reason": "translation is not currently missing"
                }));
                continue;
            };

            let expected_placeholders = extract_placeholders(&missing.source_value);
            let actual_placeholders = extract_placeholders(&translation.value);
            if expected_placeholders != actual_placeholders {
                skipped.push(json!({
                    "key": translation.key,
                    "locale": translation.locale,
                    "reason": "placeholder mismatch",
                    "expected_placeholders": expected_placeholders,
                    "actual_placeholders": actual_placeholders
                }));
                continue;
            }

            let Some(file) = find_locale_file(workspace, &translation.locale) else {
                skipped.push(json!({
                    "key": translation.key,
                    "locale": translation.locale,
                    "reason": "target locale file was not found"
                }));
                continue;
            };

            let before = std::fs::read_to_string(&file)
                .with_context(|| format!("Failed to read locale file {}", file.display()))?;
            let Some(after) =
                add_translation_to_content(&file, &before, &translation.key, &translation.value)?
            else {
                skipped.push(json!({
                    "key": translation.key,
                    "locale": translation.locale,
                    "file": file,
                    "reason": "target file format is not supported for translation patches"
                }));
                continue;
            };

            let diff = unified_diff(workspace, &file, &before, &after);
            patches.push(PlannedTranslationPatch {
                key: translation.key,
                locale: translation.locale,
                file,
                after,
                unified_diff: diff,
            });
        }

        Ok(TranslationPatchPlan { patches, skipped })
    }
}

struct TranslationPatchPlan {
    patches: Vec<PlannedTranslationPatch>,
    skipped: Vec<Value>,
}

struct PlannedTranslationPatch {
    key: String,
    locale: String,
    file: PathBuf,
    after: String,
    unified_diff: String,
}

fn strip_missing_suggestions(items: &[MissingTranslation]) -> Vec<Value> {
    items
        .iter()
        .map(|item| {
            json!({
                "key": item.key,
                "source_value": item.source_value,
                "source_locale": item.source_locale,
                "missing_in": item.missing_in,
                "used_in": item.used_in
            })
        })
        .collect()
}

fn find_locale_file(workspace: &Path, locale: &str) -> Option<PathBuf> {
    let config = I18nConfig::load_from_workspace(workspace);

    for locale_path in config.locale_paths {
        let base = workspace.join(locale_path);
        if !base.exists() {
            continue;
        }

        for extension in ["json", "yaml", "yml", "arb", "php"] {
            let candidate = base.join(format!("{}.{}", locale, extension));
            if candidate.exists() {
                return Some(candidate);
            }
        }

        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("arb") {
                    continue;
                }

                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };

                if stem == locale || stem.ends_with(&format!("_{locale}")) {
                    return Some(path);
                }
            }
        }
    }

    None
}

fn add_translation_to_content(
    path: &Path,
    content: &str,
    key: &str,
    value: &str,
) -> Result<Option<String>> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => add_json_translation_to_content(content, key, value).map(Some),
        Some("yaml") | Some("yml") => {
            add_yaml_translation_to_content(content, key, value).map(Some)
        }
        Some("arb") => add_arb_translation_to_content(content, key, value).map(Some),
        Some("php") => add_php_translation_to_content(content, key, value).map(Some),
        _ => Ok(None),
    }
}

fn add_json_translation_to_content(content: &str, key: &str, value: &str) -> Result<String> {
    let mut json: serde_json::Value = serde_json::from_str(content)?;
    insert_json_key(&mut json, key, value);

    let mut output = serde_json::to_string_pretty(&json)?;
    output.push('\n');
    Ok(output)
}

fn add_yaml_translation_to_content(content: &str, key: &str, value: &str) -> Result<String> {
    let mut yaml: serde_yaml::Value = serde_yaml::from_str(content)?;
    insert_yaml_key(&mut yaml, key, value);
    Ok(serde_yaml::to_string(&yaml)?)
}

fn add_arb_translation_to_content(content: &str, key: &str, value: &str) -> Result<String> {
    let mut json: serde_json::Value = serde_json::from_str(content)?;
    if !json.is_object() {
        json = serde_json::json!({});
    }
    json[key] = serde_json::Value::String(value.to_string());

    let mut output = serde_json::to_string_pretty(&json)?;
    output.push('\n');
    Ok(output)
}

fn add_php_translation_to_content(content: &str, key: &str, value: &str) -> Result<String> {
    let insert_at = content
        .rfind("];")
        .ok_or_else(|| anyhow!("Failed to find closing PHP short array"))?;
    let escaped_key = escape_php_single_quoted(key);
    let escaped_value = escape_php_single_quoted(value);
    let indent = detect_php_root_indent(content);

    let mut output = String::new();
    output.push_str(&content[..insert_at]);
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&format!("{indent}'{escaped_key}' => '{escaped_value}',\n"));
    output.push_str(&content[insert_at..]);
    Ok(output)
}

fn insert_json_key(json: &mut serde_json::Value, key: &str, value: &str) {
    if !json.is_object() {
        *json = serde_json::json!({});
    }

    let parts: Vec<&str> = key.split('.').collect();
    let mut current = json;

    for part in &parts[..parts.len().saturating_sub(1)] {
        if !current.get(*part).is_some_and(serde_json::Value::is_object) {
            current[*part] = serde_json::json!({});
        }
        current = &mut current[*part];
    }

    if let Some(last) = parts.last() {
        current[*last] = serde_json::Value::String(value.to_string());
    }
}

fn insert_yaml_key(yaml: &mut serde_yaml::Value, key: &str, value: &str) {
    if !yaml.is_mapping() {
        *yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }

    let parts: Vec<&str> = key.split('.').collect();
    let mut current = yaml;

    for part in &parts[..parts.len().saturating_sub(1)] {
        let key = serde_yaml::Value::String((*part).to_string());
        if !current.get(&key).is_some_and(serde_yaml::Value::is_mapping) {
            current[key.clone()] = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }
        current = &mut current[key];
    }

    if let Some(last) = parts.last() {
        current[serde_yaml::Value::String((*last).to_string())] =
            serde_yaml::Value::String(value.to_string());
    }
}

fn escape_php_single_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn detect_php_root_indent(content: &str) -> String {
    content
        .lines()
        .find_map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('\'') || trimmed.starts_with('"') {
                Some(line[..line.len() - trimmed.len()].to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "    ".to_string())
}

fn extract_placeholders(value: &str) -> Vec<String> {
    let mut placeholders = Vec::new();
    for pattern in [
        r"\{\{\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\}\}",
        r"\{([a-zA-Z_][a-zA-Z0-9_]*)\}",
        r"%[sdif]",
    ] {
        let regex = Regex::new(pattern).expect("placeholder regex");
        for capture in regex.captures_iter(value) {
            let placeholder = capture
                .get(1)
                .or_else(|| capture.get(0))
                .map(|match_| match_.as_str().to_string());
            if let Some(placeholder) = placeholder {
                if !placeholders.contains(&placeholder) {
                    placeholders.push(placeholder);
                }
            }
        }
    }
    placeholders.sort();
    placeholders
}

fn unified_diff(workspace: &Path, path: &Path, before: &str, after: &str) -> String {
    let relative = path.strip_prefix(workspace).unwrap_or(path).display();
    let mut diff = String::new();
    diff.push_str(&format!("--- a/{relative}\n"));
    diff.push_str(&format!("+++ b/{relative}\n"));
    diff.push_str("@@\n");
    for line in before.lines() {
        diff.push_str(&format!("-{line}\n"));
    }
    for line in after.lines() {
        diff.push_str(&format!("+{line}\n"));
    }
    diff
}

fn review_should_block(report: &AuditReport, fail_on: &[String]) -> bool {
    fail_on.iter().any(|kind| match kind.as_str() {
        "missing" => report.summary.missing_translations > 0,
        "unused" => report.summary.unused_keys > 0,
        "placeholder" => report.summary.placeholder_mismatches > 0,
        _ => false,
    })
}

fn review_findings(report: &AuditReport, fail_on: &[String]) -> Vec<Value> {
    let mut findings = Vec::new();

    if fail_on.iter().any(|kind| kind == "missing") {
        for item in &report.missing {
            findings.push(json!({
                "kind": "missing",
                "severity": "error",
                "key": item.key,
                "locales": item.missing_in,
                "source_locale": item.source_locale,
                "source_value": item.source_value
            }));
        }
    }

    if fail_on.iter().any(|kind| kind == "placeholder") {
        for item in &report.placeholder_issues {
            findings.push(json!({
                "kind": "placeholder",
                "severity": "error",
                "key": item.key,
                "expected_placeholders": item.expected_placeholders,
                "locales": item.locale_values.keys().cloned().collect::<Vec<_>>()
            }));
        }
    }

    if fail_on.iter().any(|kind| kind == "unused") {
        for item in &report.unused {
            findings.push(json!({
                "kind": "unused",
                "severity": "warning",
                "key": item.key,
                "file": item.defined_in.file_path
            }));
        }
    }

    findings
}

fn review_markdown(report: &AuditReport, blocking: bool, findings: &[Value]) -> String {
    let mut markdown = String::new();
    markdown.push_str(if blocking {
        "## I18n Lens Review: Action Required\n\n"
    } else {
        "## I18n Lens Review: Passed\n\n"
    });
    markdown.push_str(&format!(
        "- Missing translations: {}\n",
        report.summary.missing_translations
    ));
    markdown.push_str(&format!("- Unused keys: {}\n", report.summary.unused_keys));
    markdown.push_str(&format!(
        "- Placeholder issues: {}\n",
        report.summary.placeholder_mismatches
    ));

    if !findings.is_empty() {
        markdown.push_str("\n### Findings\n\n");
        for finding in findings.iter().take(20) {
            markdown.push_str(&format!(
                "- `{}` `{}`\n",
                finding["kind"].as_str().unwrap_or("unknown"),
                finding["key"].as_str().unwrap_or("")
            ));
        }
    }

    markdown
}

fn source_files(root: &Path) -> Vec<PathBuf> {
    let walker = if root.is_file() {
        return vec![root.to_path_buf()];
    } else {
        WalkDir::new(root)
    };

    walker
        .into_iter()
        .filter_entry(|entry| !is_ignored_dir(entry.path()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file() && is_supported_source_file(entry.path()))
        .map(|entry| entry.path().to_path_buf())
        .collect()
}

fn is_ignored_dir(path: &Path) -> bool {
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

fn is_supported_source_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    file_name.ends_with(".ts")
        || file_name.ends_with(".tsx")
        || file_name.ends_with(".js")
        || file_name.ends_with(".jsx")
        || file_name.ends_with(".vue")
        || file_name.ends_with(".svelte")
        || file_name.ends_with(".php")
        || file_name.ends_with(".blade.php")
        || file_name.ends_with(".dart")
}

fn find_hardcoded_strings(
    workspace: &Path,
    file: &Path,
    content: &str,
    min_length: usize,
    remaining: usize,
) -> Vec<Value> {
    if remaining == 0 {
        return Vec::new();
    }

    let jsx_text = Regex::new(r">([^<>{}\n][^<>{}\n]*[A-Za-z][^<>{}\n]*)<").expect("jsx regex");
    let quoted =
        Regex::new(r#"[\"']([A-Za-z][^\"'\n{}]*\s+[^\"'\n{}]*)[\"']"#).expect("quoted regex");
    let mut candidates = Vec::new();

    for (line_index, line) in content.lines().enumerate() {
        for capture in jsx_text.captures_iter(line) {
            if let Some(value) = normalize_candidate(capture.get(1).map(|m| m.as_str()), min_length)
            {
                candidates.push(candidate_json(
                    workspace, file, line_index, &value, "jsx_text",
                ));
            }
            if candidates.len() >= remaining {
                return candidates;
            }
        }

        for capture in quoted.captures_iter(line) {
            if line.contains("t(") || line.contains("i18n.t(") {
                continue;
            }
            if let Some(value) = normalize_candidate(capture.get(1).map(|m| m.as_str()), min_length)
            {
                candidates.push(candidate_json(
                    workspace,
                    file,
                    line_index,
                    &value,
                    "string_literal",
                ));
            }
            if candidates.len() >= remaining {
                return candidates;
            }
        }
    }

    candidates
}

fn normalize_candidate(value: Option<&str>, min_length: usize) -> Option<String> {
    let value = value?.trim();
    if value.len() < min_length {
        return None;
    }
    if value.starts_with('.') || value.contains("://") || value.contains('/') {
        return None;
    }
    Some(value.to_string())
}

fn candidate_json(
    workspace: &Path,
    file: &Path,
    line_index: usize,
    value: &str,
    kind: &str,
) -> Value {
    json!({
        "file": file.strip_prefix(workspace).unwrap_or(file),
        "line": line_index + 1,
        "kind": kind,
        "text": value
    })
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "audit_i18n",
            description: "Run a full i18n audit for the workspace and return missing, unused, and placeholder issues.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace": { "type": "string", "description": "Workspace path. Defaults to current directory." },
                    "scope": { "type": "string", "description": "Audit scope label for clients." },
                    "include_suggestions": { "type": "boolean", "default": false }
                }
            }),
        },
        ToolDefinition {
            name: "get_missing_translations",
            description: "List missing translation keys, optionally filtered by locale and with source usage context.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace": { "type": "string" },
                    "locales": { "type": "array", "items": { "type": "string" }, "default": [] },
                    "include_context": { "type": "boolean", "default": false }
                }
            }),
        },
        ToolDefinition {
            name: "suggest_translation_fixes",
            description: "Return actionable file targets and source text for adding missing translations.",
            input_schema: json!({
                "type": "object",
                "required": ["key"],
                "properties": {
                    "workspace": { "type": "string" },
                    "key": { "type": "string" },
                    "target_locales": { "type": "array", "items": { "type": "string" }, "default": [] }
                }
            }),
        },
        ToolDefinition {
            name: "translate_missing_keys",
            description: "Return dry-run patches for caller-provided translations of missing keys.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace": { "type": "string" },
                    "dry_run": { "type": "boolean", "default": true },
                    "translations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["key", "locale", "value"],
                            "properties": {
                                "key": { "type": "string" },
                                "locale": { "type": "string" },
                                "value": { "type": "string" }
                            }
                        },
                        "default": []
                    }
                }
            }),
        },
        ToolDefinition {
            name: "apply_translation_patch",
            description: "Apply or dry-run caller-provided translations for missing keys.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace": { "type": "string" },
                    "dry_run": { "type": "boolean", "default": true },
                    "translations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["key", "locale", "value"],
                            "properties": {
                                "key": { "type": "string" },
                                "locale": { "type": "string" },
                                "value": { "type": "string" }
                            }
                        },
                        "default": []
                    }
                }
            }),
        },
        ToolDefinition {
            name: "validate_placeholders",
            description: "Check placeholder consistency for a specific translation key across locales.",
            input_schema: json!({
                "type": "object",
                "required": ["key"],
                "properties": {
                    "workspace": { "type": "string" },
                    "key": { "type": "string" }
                }
            }),
        },
        ToolDefinition {
            name: "get_translation_context",
            description: "Return translations, missing locales, usage context, and target files for one key.",
            input_schema: json!({
                "type": "object",
                "required": ["key"],
                "properties": {
                    "workspace": { "type": "string" },
                    "key": { "type": "string" },
                    "include_usage": { "type": "boolean", "default": false }
                }
            }),
        },
        ToolDefinition {
            name: "review_i18n_pr",
            description: "Return a structured PR-style i18n review from the current audit.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace": { "type": "string" },
                    "fail_on": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["missing", "unused", "placeholder"] },
                        "default": ["missing", "placeholder"]
                    }
                }
            }),
        },
        ToolDefinition {
            name: "extract_hardcoded_strings",
            description: "Return candidate hardcoded user-facing strings from source files.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" }, "default": [] },
                    "min_length": { "type": "integer", "default": 3 },
                    "limit": { "type": "integer", "default": 100 }
                }
            }),
        },
    ]
}

fn read_message(reader: &mut impl Read) -> Result<Option<JsonRpcRequest>> {
    let mut content_length = None;
    let mut header_buffer = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        let read = reader.read(&mut byte)?;
        if read == 0 {
            if header_buffer.is_empty() {
                return Ok(None);
            }
            return Err(anyhow!("Unexpected EOF while reading headers"));
        }

        header_buffer.push(byte[0]);

        if header_buffer.ends_with(b"\r\n\r\n") {
            break;
        }
    }

    let headers = String::from_utf8(header_buffer).context("Headers were not valid UTF-8")?;
    for line in headers.split("\r\n") {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                content_length = Some(value.trim().parse::<usize>()?);
            }
        }
    }

    let content_length = content_length.ok_or_else(|| anyhow!("Missing Content-Length header"))?;
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    let request: JsonRpcRequest = serde_json::from_slice(&body)?;
    if request.jsonrpc.as_deref().unwrap_or("2.0") != "2.0" {
        return Err(anyhow!("Only JSON-RPC 2.0 requests are supported"));
    }

    Ok(Some(request))
}

fn write_message(writer: &mut impl Write, response: &JsonRpcResponse) -> Result<()> {
    let body = serde_json::to_vec(response)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env().add_directive("intl_lens=info".parse()?))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    let workspace_root = std::env::current_dir().context("Failed to resolve current directory")?;
    let server = McpServer::new(workspace_root);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    while let Some(request) = read_message(&mut reader)? {
        let is_notification = request.id.is_none();
        let response = server.handle_request(request);
        if !is_notification {
            write_message(&mut writer, &response)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_tool_definitions() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 9);
        assert_eq!(tools[0].name, "audit_i18n");
    }

    #[test]
    fn parses_content_length_message() {
        let payload = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let raw = format!("Content-Length: {}\r\n\r\n{}", payload.len(), payload);
        let mut bytes = raw.as_bytes();
        let request = read_message(&mut bytes)
            .expect("request should parse")
            .expect("request should exist");

        assert_eq!(request.method, "ping");
        assert_eq!(request.id, Some(json!(1)));
    }

    #[test]
    fn strips_missing_suggestions_when_requested() {
        let stripped = strip_missing_suggestions(&[MissingTranslation {
            key: "common.save".to_string(),
            source_value: "Save".to_string(),
            source_locale: "en".to_string(),
            missing_in: vec!["vi".to_string()],
            used_in: vec![],
            suggestion: Some(FixSuggestion {
                action: "add_translation".to_string(),
                files_to_edit: vec![PathBuf::from("locales/vi.json")],
                context: None,
            }),
        }]);

        assert_eq!(stripped.len(), 1);
        assert!(stripped[0].get("suggestion").is_none());
    }

    #[test]
    fn writes_content_length_response() {
        let response = JsonRpcResponse {
            jsonrpc: "2.0",
            id: Some(json!(1)),
            result: Some(json!({ "pong": true })),
            error: None,
        };
        let mut output = Vec::new();
        write_message(&mut output, &response).expect("response should serialize");

        let text = String::from_utf8(output).expect("utf8 output");
        assert!(text.starts_with("Content-Length: "));
        assert!(text.contains("\r\n\r\n{\"jsonrpc\":\"2.0\""));
    }
}
