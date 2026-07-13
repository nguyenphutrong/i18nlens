use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use globset::{Glob, GlobSet, GlobSetBuilder};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::audit::{AuditReport, AuditResult, AuditSummary};
use crate::config::I18nConfig;
use crate::i18n::store::TranslationStore;
use crate::scanner::CodeScanner;

#[derive(Parser)]
#[command(name = "i18nlens")]
#[command(about = "CLI tool for i18n auditing and analysis")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Path to the project root
    #[arg(short, long, default_value = ".", global = true)]
    workspace: PathBuf,

    /// Output format
    #[arg(short, long, value_enum, default_value = "terminal", global = true)]
    format: OutputFormat,

    /// Output file (if not specified, prints to stdout)
    #[arg(short, long, global = true)]
    output: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Audit the entire project for i18n issues
    Audit(AuditArgs),
    /// Audit with CI-oriented defaults
    Ci(CiArgs),
    /// Check specific files for i18n key usage
    Check {
        /// Files to check
        files: Vec<PathBuf>,
    },
    /// Fix issues automatically (with approval)
    Fix {
        /// Dry run - show what would be changed without making changes
        #[arg(long)]
        dry_run: bool,
        /// Add missing translations to target locale files
        #[arg(long)]
        add_missing: bool,
        /// Sort translation keys in supported locale files
        #[arg(long)]
        sort_keys: bool,
        /// Convert flat dotted keys to nested key structure
        #[arg(long, conflicts_with = "to_flat")]
        to_nested: bool,
        /// Convert nested key structure to flat dotted keys
        #[arg(long, conflicts_with = "to_nested")]
        to_flat: bool,
        /// Value to use when adding missing translations. Defaults to source text.
        #[arg(long)]
        placeholder: Option<String>,
    },
}

#[derive(clap::Args)]
struct AuditArgs {
    /// Filter by missing locales (comma-separated)
    #[arg(long)]
    missing_in: Option<String>,

    /// Include AI-ready fix suggestions
    #[arg(long)]
    suggest_fixes: bool,

    /// Issue kinds that should fail the command: missing,unused,placeholder
    #[arg(long, value_parser = parse_fail_on, default_value = "missing,unused")]
    fail_on: FailOn,

    /// Ignore translation keys matching this regex
    #[arg(long)]
    ignore_key_pattern: Vec<String>,

    /// Ignore issues from files matching this glob, relative to workspace
    #[arg(long)]
    ignore_file: Vec<String>,

    /// Baseline file with accepted existing issues
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Write a baseline file from the current active issues and exit successfully
    #[arg(long)]
    write_baseline: Option<PathBuf>,

    /// Allow up to this many active unused keys before failing on unused
    #[arg(long)]
    max_unused: Option<usize>,
}

#[derive(clap::Args)]
struct CiArgs {
    /// Filter by missing locales (comma-separated)
    #[arg(long)]
    missing_in: Option<String>,

    /// Include AI-ready fix suggestions
    #[arg(long)]
    suggest_fixes: bool,

    /// Issue kinds that should fail the command: missing,unused,placeholder
    #[arg(long, value_parser = parse_fail_on, default_value = "missing,placeholder")]
    fail_on: FailOn,

    /// Ignore translation keys matching this regex
    #[arg(long)]
    ignore_key_pattern: Vec<String>,

    /// Ignore issues from files matching this glob, relative to workspace
    #[arg(long)]
    ignore_file: Vec<String>,

    /// Baseline file with accepted existing issues. Defaults to .i18nlens-baseline.json when present.
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Write a baseline file from the current active issues and exit successfully
    #[arg(long)]
    write_baseline: Option<PathBuf>,

    /// Allow up to this many active unused keys before failing on unused
    #[arg(long)]
    max_unused: Option<usize>,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum OutputFormat {
    /// Human-readable terminal output with colors
    Terminal,
    /// JSON format for programmatic consumption
    Json,
    /// Markdown report
    Markdown,
}

#[derive(Debug, Clone)]
struct FailOn {
    missing: bool,
    unused: bool,
    placeholder: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IssueKind {
    Missing,
    Unused,
    Placeholder,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
struct IssueIdentity {
    kind: IssueKind,
    key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    locale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Baseline {
    version: u8,
    issues: Vec<IssueIdentity>,
}

struct AuditOptions {
    missing_in: Option<String>,
    suggest_fixes: bool,
    fail_on: FailOn,
    ignore_key_pattern: Vec<String>,
    ignore_file: Vec<String>,
    baseline: Option<PathBuf>,
    write_baseline: Option<PathBuf>,
    max_unused: Option<usize>,
}

pub async fn run_from_env() -> anyhow::Result<i32> {
    run(std::env::args_os()).await
}

pub async fn run<I, T>(args: I) -> anyhow::Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);

    match cli.command {
        Commands::Audit(args) => {
            run_audit(
                &cli.workspace,
                cli.format,
                cli.output,
                AuditOptions::from(args),
            )
            .await
        }
        Commands::Ci(args) => {
            run_audit(
                &cli.workspace,
                cli.format,
                cli.output,
                AuditOptions::from_ci(args, &cli.workspace),
            )
            .await
        }
        Commands::Check { files } => run_check(&cli.workspace, files, cli.format, cli.output).await,
        Commands::Fix {
            dry_run,
            add_missing,
            sort_keys,
            to_nested,
            to_flat,
            placeholder,
        } => {
            run_fix(
                &cli.workspace,
                dry_run,
                add_missing,
                sort_keys,
                to_nested,
                to_flat,
                placeholder,
            )
            .await
        }
    }
}

impl From<AuditArgs> for AuditOptions {
    fn from(args: AuditArgs) -> Self {
        Self {
            missing_in: args.missing_in,
            suggest_fixes: args.suggest_fixes,
            fail_on: args.fail_on,
            ignore_key_pattern: args.ignore_key_pattern,
            ignore_file: args.ignore_file,
            baseline: args.baseline,
            write_baseline: args.write_baseline,
            max_unused: args.max_unused,
        }
    }
}

impl AuditOptions {
    fn from_ci(args: CiArgs, workspace: &Path) -> Self {
        let baseline = args.baseline.or_else(|| {
            let default = workspace.join(".i18nlens-baseline.json");
            let legacy_default = workspace.join(".intl-lens-baseline.json");
            default
                .exists()
                .then_some(default)
                .or_else(|| legacy_default.exists().then_some(legacy_default))
        });

        Self {
            missing_in: args.missing_in,
            suggest_fixes: args.suggest_fixes,
            fail_on: args.fail_on,
            ignore_key_pattern: args.ignore_key_pattern,
            ignore_file: args.ignore_file,
            baseline,
            write_baseline: args.write_baseline,
            max_unused: args.max_unused,
        }
    }
}

fn parse_fail_on(value: &str) -> Result<FailOn, String> {
    let mut fail_on = FailOn {
        missing: false,
        unused: false,
        placeholder: false,
    };

    for part in value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match part {
            "missing" => fail_on.missing = true,
            "unused" => fail_on.unused = true,
            "placeholder" => fail_on.placeholder = true,
            "none" => {}
            other => {
                return Err(format!(
                    "unknown issue kind '{other}', expected missing, unused, or placeholder"
                ));
            }
        }
    }

    Ok(fail_on)
}

async fn run_audit(
    workspace: &Path,
    format: OutputFormat,
    output: Option<PathBuf>,
    options: AuditOptions,
) -> anyhow::Result<i32> {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")?
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );

    pb.set_message("Loading configuration...");
    let config = I18nConfig::load_from_workspace(workspace);

    pb.set_message("Scanning translation files...");
    let store = TranslationStore::new(workspace.to_path_buf())
        .with_namespace_enabled(config.namespace_enabled);
    store.scan_and_load(&config.locale_paths);

    pb.set_message("Scanning codebase...");
    let mut result = AuditResult::new(workspace.to_path_buf(), config, store);
    result.scan_codebase();

    pb.set_message("Generating report...");
    let mut report = result.generate_report();
    pb.finish_and_clear();

    apply_audit_filters(&mut report, workspace, &options)?;

    if let Some(path) = options.write_baseline.as_ref() {
        write_baseline(path, workspace, &report)?;
        println!("✓ Baseline written to {}", path.display());
        return Ok(0);
    }

    let output_str = match format {
        OutputFormat::Terminal => format_terminal(&report, options.suggest_fixes),
        OutputFormat::Json => serde_json::to_string_pretty(&report)?,
        OutputFormat::Markdown => format_markdown(&report, options.suggest_fixes),
    };

    if let Some(output_path) = output {
        std::fs::write(&output_path, output_str)?;
        println!("✓ Report written to {}", output_path.display());
    } else {
        println!("{}", output_str);
    }

    Ok(evaluate_exit_code(
        &report,
        &options.fail_on,
        options.max_unused,
    ))
}

fn apply_audit_filters(
    report: &mut AuditReport,
    workspace: &Path,
    options: &AuditOptions,
) -> anyhow::Result<()> {
    if let Some(locales_str) = options.missing_in.as_ref() {
        let locales: HashSet<&str> = locales_str.split(',').map(str::trim).collect();
        report.missing.retain_mut(|item| {
            item.missing_in
                .retain(|locale| locales.contains(locale.as_str()));
            !item.missing_in.is_empty()
        });
    }

    let key_patterns = compile_key_patterns(&options.ignore_key_pattern)?;
    if !key_patterns.is_empty() {
        report
            .missing
            .retain(|item| !key_patterns.iter().any(|regex| regex.is_match(&item.key)));
        report
            .unused
            .retain(|item| !key_patterns.iter().any(|regex| regex.is_match(&item.key)));
        report
            .placeholder_issues
            .retain(|item| !key_patterns.iter().any(|regex| regex.is_match(&item.key)));
    }

    let file_globs = compile_file_globs(&options.ignore_file)?;
    if let Some(file_globs) = file_globs.as_ref() {
        report.missing.retain_mut(|item| {
            let had_usages = !item.used_in.is_empty();
            item.used_in
                .retain(|usage| !matches_glob(file_globs, workspace, &usage.file));
            !had_usages || !item.used_in.is_empty()
        });
        report
            .unused
            .retain(|item| !matches_glob(file_globs, workspace, &item.defined_in.file_path));
    }

    if let Some(path) = options.baseline.as_ref() {
        let baseline = read_baseline(path)?;
        apply_baseline(report, workspace, &baseline);
    }

    recalculate_summary(report);
    Ok(())
}

fn compile_key_patterns(patterns: &[String]) -> anyhow::Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|pattern| Regex::new(pattern).with_context(|| format!("Invalid regex: {pattern}")))
        .collect()
}

fn compile_file_globs(patterns: &[String]) -> anyhow::Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).with_context(|| format!("Invalid glob: {pattern}"))?);
    }

    Ok(Some(builder.build()?))
}

fn matches_glob(globs: &GlobSet, workspace: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(workspace).unwrap_or(path);
    globs.is_match(relative)
}

fn read_baseline(path: &Path) -> anyhow::Result<Baseline> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read baseline {}", path.display()))?;
    let baseline = serde_json::from_str::<Baseline>(&content)
        .with_context(|| format!("Failed to parse baseline {}", path.display()))?;
    if baseline.version != 1 {
        return Err(anyhow!(
            "Unsupported baseline version {}, expected 1",
            baseline.version
        ));
    }
    Ok(baseline)
}

fn write_baseline(path: &Path, workspace: &Path, report: &AuditReport) -> anyhow::Result<()> {
    let mut issues: Vec<IssueIdentity> = issue_identities(report, workspace).into_iter().collect();
    issues.sort();

    let baseline = Baseline { version: 1, issues };

    let content = serde_json::to_string_pretty(&baseline)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn apply_baseline(report: &mut AuditReport, workspace: &Path, baseline: &Baseline) {
    let issues: HashSet<IssueIdentity> = baseline.issues.iter().cloned().collect();

    report.missing.retain_mut(|item| {
        item.missing_in.retain(|locale| {
            !issues.contains(&IssueIdentity {
                kind: IssueKind::Missing,
                key: item.key.clone(),
                locale: Some(locale.clone()),
                file: None,
            })
        });
        !item.missing_in.is_empty()
    });

    report.unused.retain(|item| {
        !issues.contains(&IssueIdentity {
            kind: IssueKind::Unused,
            key: item.key.clone(),
            locale: None,
            file: Some(relative_path(workspace, &item.defined_in.file_path)),
        })
    });

    report.placeholder_issues.retain_mut(|item| {
        item.locale_values.retain(|locale, _| {
            !issues.contains(&IssueIdentity {
                kind: IssueKind::Placeholder,
                key: item.key.clone(),
                locale: Some(locale.clone()),
                file: None,
            })
        });
        !item.locale_values.is_empty()
    });
}

fn issue_identities(report: &AuditReport, workspace: &Path) -> HashSet<IssueIdentity> {
    let mut issues = HashSet::new();

    for item in &report.missing {
        for locale in &item.missing_in {
            issues.insert(IssueIdentity {
                kind: IssueKind::Missing,
                key: item.key.clone(),
                locale: Some(locale.clone()),
                file: None,
            });
        }
    }

    for item in &report.unused {
        issues.insert(IssueIdentity {
            kind: IssueKind::Unused,
            key: item.key.clone(),
            locale: None,
            file: Some(relative_path(workspace, &item.defined_in.file_path)),
        });
    }

    for item in &report.placeholder_issues {
        for locale in item.locale_values.keys() {
            issues.insert(IssueIdentity {
                kind: IssueKind::Placeholder,
                key: item.key.clone(),
                locale: Some(locale.clone()),
                file: None,
            });
        }
    }

    issues
}

fn relative_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn recalculate_summary(report: &mut AuditReport) {
    report.summary = AuditSummary {
        total_keys: report.summary.total_keys,
        total_locales: report.summary.total_locales,
        missing_translations: report.missing.len(),
        unused_keys: report.unused.len(),
        placeholder_mismatches: report.placeholder_issues.len(),
    };
}

fn evaluate_exit_code(report: &AuditReport, fail_on: &FailOn, max_unused: Option<usize>) -> i32 {
    if fail_on.missing && report.summary.missing_translations > 0 {
        return 1;
    }

    if fail_on.unused && report.summary.unused_keys > max_unused.unwrap_or(0) {
        return 1;
    }

    if fail_on.placeholder && report.summary.placeholder_mismatches > 0 {
        return 1;
    }

    0
}

async fn run_check(
    workspace: &Path,
    files: Vec<PathBuf>,
    format: OutputFormat,
    output: Option<PathBuf>,
) -> anyhow::Result<i32> {
    let config = I18nConfig::load_from_workspace(workspace);
    let scanner = CodeScanner::new(&config.function_patterns);

    let mut all_keys = Vec::new();

    for file in files {
        let content = std::fs::read_to_string(&file)?;
        let occurrences = scanner.scan_content(&content);
        for occ in occurrences {
            all_keys.push((file.clone(), occ));
        }
    }

    let store = TranslationStore::new(workspace.to_path_buf())
        .with_namespace_enabled(config.namespace_enabled);
    store.scan_and_load(&config.locale_paths);

    let mut missing = Vec::new();
    let mut found = Vec::new();

    for (file, occ) in all_keys {
        if store.key_exists(&occ.key) {
            found.push((file, occ));
        } else {
            missing.push((file, occ));
        }
    }

    match format {
        OutputFormat::Terminal => {
            println!("{}", "i18n Key Check Results".bold().underline());
            println!();

            if !missing.is_empty() {
                println!(
                    "{}",
                    format!("❌ Missing Keys ({}):", missing.len()).red().bold()
                );
                for (file, occ) in &missing {
                    println!(
                        "  {}:{} {}",
                        file.display().to_string().cyan(),
                        occ.line + 1,
                        occ.key.yellow()
                    );
                }
                println!();
            }

            if !found.is_empty() {
                println!(
                    "{}",
                    format!("✓ Found Keys ({})", found.len()).green().bold()
                );
                for (file, occ) in &found {
                    println!(
                        "  {}:{} {}",
                        file.display().to_string().dimmed(),
                        occ.line + 1,
                        occ.key.dimmed()
                    );
                }
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "missing": missing.iter().map(|(f, o)| serde_json::json!({
                    "file": f,
                    "line": o.line + 1,
                    "key": o.key,
                })).collect::<Vec<_>>(),
                "found": found.iter().map(|(f, o)| serde_json::json!({
                    "file": f,
                    "line": o.line + 1,
                    "key": o.key,
                })).collect::<Vec<_>>(),
            });
            let output_str = serde_json::to_string_pretty(&json)?;
            if let Some(output_path) = output {
                std::fs::write(&output_path, output_str)?;
            } else {
                println!("{}", output_str);
            }
        }
        OutputFormat::Markdown => {
            let mut md = String::new();
            md.push_str("# i18n Key Check Results\n\n");

            if !missing.is_empty() {
                md.push_str(&format!("## ❌ Missing Keys ({}):\n\n", missing.len()));
                for (file, occ) in &missing {
                    md.push_str(&format!(
                        "- `{}:{}` - `{}`\n",
                        file.display(),
                        occ.line + 1,
                        occ.key
                    ));
                }
                md.push('\n');
            }

            if !found.is_empty() {
                md.push_str(&format!("## ✓ Found Keys ({}):\n\n", found.len()));
                for (file, occ) in &found {
                    md.push_str(&format!(
                        "- `{}:{}` - `{}`\n",
                        file.display(),
                        occ.line + 1,
                        occ.key
                    ));
                }
            }

            if let Some(output_path) = output {
                std::fs::write(&output_path, md)?;
            } else {
                println!("{}", md);
            }
        }
    }

    Ok(if missing.is_empty() { 0 } else { 1 })
}

async fn run_fix(
    workspace: &Path,
    dry_run: bool,
    add_missing: bool,
    sort_keys: bool,
    to_nested: bool,
    to_flat: bool,
    placeholder: Option<String>,
) -> anyhow::Result<i32> {
    let config = I18nConfig::load_from_workspace(workspace);
    let store = TranslationStore::new(workspace.to_path_buf())
        .with_namespace_enabled(config.namespace_enabled);
    store.scan_and_load(&config.locale_paths);

    let mut result = AuditResult::new(workspace.to_path_buf(), config.clone(), store);
    result.scan_codebase();
    let report = result.generate_report();

    if dry_run {
        println!("{}", format_fix_dry_run(workspace, &config, &report));
        return Ok(0);
    }

    if add_missing {
        let summary =
            apply_missing_translations(workspace, &config, &report, placeholder.as_deref())?;
        println!("Added {} missing translations.", summary.added);
        if summary.skipped > 0 {
            println!(
                "Skipped {} missing translations without a supported JSON target file.",
                summary.skipped
            );
        }
        return Ok(0);
    }

    if to_nested || to_flat {
        let target_style = if to_nested {
            KeyConversionTarget::Nested
        } else {
            KeyConversionTarget::Flat
        };
        let summary = convert_translation_files(workspace, &config, target_style)?;
        println!("Converted {} translation files.", summary.converted);
        println!(
            "Skipped {} unsupported or unchanged files.",
            summary.skipped
        );
        if sort_keys {
            let sort_summary = sort_translation_files(workspace, &config)?;
            println!("Sorted {} translation files.", sort_summary.sorted);
            println!(
                "Skipped {} unsupported or unchanged files.",
                sort_summary.skipped
            );
        }
        return Ok(0);
    }

    if sort_keys {
        let summary = sort_translation_files(workspace, &config)?;
        println!("Sorted {} translation files.", summary.sorted);
        println!(
            "Skipped {} unsupported or unchanged files.",
            summary.skipped
        );
        return Ok(0);
    }

    println!(
        "Write mode requires an explicit fix option. Run `i18nlens fix --dry-run` to preview fixes."
    );
    println!(
        "Supported write options: `i18nlens fix --add-missing --placeholder _TODO_`, `i18nlens fix --sort-keys`, `i18nlens fix --to-nested`, `i18nlens fix --to-flat`."
    );
    Ok(1)
}

struct MissingWriteSummary {
    added: usize,
    skipped: usize,
}

struct SortSummary {
    sorted: usize,
    skipped: usize,
}

struct ConvertSummary {
    converted: usize,
    skipped: usize,
}

enum SortOutcome {
    Sorted,
    Skipped,
}

enum ConvertOutcome {
    Converted,
    Skipped,
}

#[derive(Clone, Copy)]
enum KeyConversionTarget {
    Nested,
    Flat,
}

fn convert_translation_files(
    workspace: &Path,
    config: &I18nConfig,
    target: KeyConversionTarget,
) -> anyhow::Result<ConvertSummary> {
    let mut converted = 0;
    let mut skipped = 0;

    for file in collect_translation_files(workspace, &config.locale_paths) {
        match convert_translation_file(&file, target)? {
            ConvertOutcome::Converted => converted += 1,
            ConvertOutcome::Skipped => skipped += 1,
        }
    }

    Ok(ConvertSummary { converted, skipped })
}

fn convert_translation_file(
    path: &Path,
    target: KeyConversionTarget,
) -> anyhow::Result<ConvertOutcome> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => convert_json_translation_file(path, target),
        Some("yaml") | Some("yml") => convert_yaml_translation_file(path, target),
        _ => Ok(ConvertOutcome::Skipped),
    }
}

fn convert_json_translation_file(
    path: &Path,
    target: KeyConversionTarget,
) -> anyhow::Result<ConvertOutcome> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read locale file {}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse JSON locale file {}", path.display()))?;
    let converted = match target {
        KeyConversionTarget::Nested => json_to_nested(json),
        KeyConversionTarget::Flat => json_to_flat(json),
    };
    let converted = converted.with_context(|| {
        format!(
            "Failed to convert JSON locale file {} without key conflicts",
            path.display()
        )
    })?;

    let mut output = serde_json::to_string_pretty(&converted)?;
    output.push('\n');
    if content == output {
        return Ok(ConvertOutcome::Skipped);
    }
    std::fs::write(path, output)
        .with_context(|| format!("Failed to write locale file {}", path.display()))?;
    Ok(ConvertOutcome::Converted)
}

fn convert_yaml_translation_file(
    path: &Path,
    target: KeyConversionTarget,
) -> anyhow::Result<ConvertOutcome> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read locale file {}", path.display()))?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse YAML locale file {}", path.display()))?;
    let converted = match target {
        KeyConversionTarget::Nested => yaml_to_nested(yaml),
        KeyConversionTarget::Flat => yaml_to_flat(yaml),
    };
    let converted = converted.with_context(|| {
        format!(
            "Failed to convert YAML locale file {} without key conflicts",
            path.display()
        )
    })?;

    let output = serde_yaml::to_string(&converted)?;
    if content == output {
        return Ok(ConvertOutcome::Skipped);
    }
    std::fs::write(path, output)
        .with_context(|| format!("Failed to write locale file {}", path.display()))?;
    Ok(ConvertOutcome::Converted)
}

fn sort_translation_files(workspace: &Path, config: &I18nConfig) -> anyhow::Result<SortSummary> {
    let mut sorted = 0;
    let mut skipped = 0;

    for file in collect_translation_files(workspace, &config.locale_paths) {
        match sort_translation_file(&file)? {
            SortOutcome::Sorted => sorted += 1,
            SortOutcome::Skipped => skipped += 1,
        }
    }

    Ok(SortSummary { sorted, skipped })
}

fn collect_translation_files(workspace: &Path, locale_paths: &[String]) -> Vec<PathBuf> {
    let mut files = Vec::new();

    for locale_path in locale_paths {
        let trimmed = locale_path.trim_end_matches(['/', '\\']);
        if trimmed.is_empty() {
            continue;
        }

        if has_glob_meta(trimmed) {
            collect_glob_translation_files(workspace, trimmed, &mut files);
            continue;
        }

        let full_path = workspace.join(trimmed);
        if full_path.is_file() {
            files.push(full_path);
        } else if full_path.exists() {
            collect_dir_translation_files(&full_path, &mut files);
        }
    }

    files.sort();
    files.dedup();
    files
}

fn collect_glob_translation_files(workspace: &Path, pattern: &str, files: &mut Vec<PathBuf>) {
    let Ok(glob) = Glob::new(pattern) else {
        return;
    };
    let matcher = glob.compile_matcher();

    for entry in WalkDir::new(workspace)
        .into_iter()
        .filter_entry(|entry| !is_ignored_workspace_dir(entry.path()))
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        let Ok(relative_path) = path.strip_prefix(workspace) else {
            continue;
        };

        if !matcher.is_match(relative_path) {
            continue;
        }

        if path.is_dir() {
            collect_dir_translation_files(path, files);
        } else if path.is_file() {
            files.push(path.to_path_buf());
        }
    }
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

fn collect_dir_translation_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in WalkDir::new(dir)
        .max_depth(3)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        if path.is_file() && is_translation_extension(path) {
            files.push(path.to_path_buf());
        }
    }
}

fn is_translation_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("json" | "yaml" | "yml" | "arb" | "php")
    )
}

fn sort_translation_file(path: &Path) -> anyhow::Result<SortOutcome> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => sort_json_translation_file(path),
        Some("yaml") | Some("yml") => sort_yaml_translation_file(path),
        Some("arb") => sort_arb_translation_file(path),
        _ => Ok(SortOutcome::Skipped),
    }
}

fn sort_json_translation_file(path: &Path) -> anyhow::Result<SortOutcome> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read locale file {}", path.display()))?;
    let mut json: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse JSON locale file {}", path.display()))?;
    sort_json_value(&mut json);

    let mut output = serde_json::to_string_pretty(&json)?;
    output.push('\n');
    write_if_changed(path, &content, output)
}

fn sort_arb_translation_file(path: &Path) -> anyhow::Result<SortOutcome> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read locale file {}", path.display()))?;
    let mut json: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse ARB locale file {}", path.display()))?;
    sort_json_value(&mut json);

    let mut output = serde_json::to_string_pretty(&json)?;
    output.push('\n');
    write_if_changed(path, &content, output)
}

fn sort_yaml_translation_file(path: &Path) -> anyhow::Result<SortOutcome> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read locale file {}", path.display()))?;
    let mut yaml: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse YAML locale file {}", path.display()))?;
    sort_yaml_value(&mut yaml);

    let output = serde_yaml::to_string(&yaml)?;
    write_if_changed(path, &content, output)
}

fn write_if_changed(path: &Path, before: &str, after: String) -> anyhow::Result<SortOutcome> {
    if before == after {
        return Ok(SortOutcome::Skipped);
    }

    std::fs::write(path, after)
        .with_context(|| format!("Failed to write locale file {}", path.display()))?;
    Ok(SortOutcome::Sorted)
}

fn sort_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for child in map.values_mut() {
                sort_json_value(child);
            }
            map.sort_keys();
        }
        serde_json::Value::Array(items) => {
            for item in items {
                sort_json_value(item);
            }
        }
        _ => {}
    }
}

fn sort_yaml_value(value: &mut serde_yaml::Value) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            let old_map = std::mem::take(map);
            let mut entries: Vec<(serde_yaml::Value, serde_yaml::Value)> =
                old_map.into_iter().collect();
            for (_, child) in &mut entries {
                sort_yaml_value(child);
            }
            entries.sort_by_key(|(left, _)| yaml_sort_key(left));
            *map = entries.into_iter().collect();
        }
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                sort_yaml_value(item);
            }
        }
        _ => {}
    }
}

fn yaml_sort_key(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::String(value) => value.clone(),
        other => serde_yaml::to_string(other).unwrap_or_default(),
    }
}

fn json_to_nested(value: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let mut nested = serde_json::json!({});
    for (key, value) in flatten_json_entries("", value) {
        insert_json_value(&mut nested, &key, value)?;
    }
    Ok(nested)
}

fn json_to_flat(value: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let entries = flatten_json_entries("", value);
    let mut flat = serde_json::Map::new();
    for (key, value) in entries {
        if flat.contains_key(&key) {
            return Err(anyhow!("conflicting key `{key}`"));
        }
        flat.insert(key, value);
    }
    Ok(serde_json::Value::Object(flat))
}

fn flatten_json_entries(
    prefix: &str,
    value: serde_json::Value,
) -> Vec<(String, serde_json::Value)> {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries = Vec::new();
            for (key, child) in map {
                let next_key = join_key(prefix, &key);
                if child.is_object() {
                    entries.extend(flatten_json_entries(&next_key, child));
                } else {
                    entries.push((next_key, child));
                }
            }
            entries
        }
        other if !prefix.is_empty() => vec![(prefix.to_string(), other)],
        other => vec![(String::new(), other)],
    }
}

fn insert_json_value(
    json: &mut serde_json::Value,
    key: &str,
    value: serde_json::Value,
) -> anyhow::Result<()> {
    if !json.is_object() {
        *json = serde_json::json!({});
    }

    let parts: Vec<&str> = key.split('.').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        *json = value;
        return Ok(());
    }

    let mut current = json;
    for part in &parts[..parts.len().saturating_sub(1)] {
        if current.get(*part).is_some_and(|child| !child.is_object()) {
            return Err(anyhow!("conflicting key `{key}`"));
        }
        if !current.get(*part).is_some_and(serde_json::Value::is_object) {
            current[*part] = serde_json::json!({});
        }
        current = &mut current[*part];
    }

    if let Some(last) = parts.last() {
        if current.get(*last).is_some_and(serde_json::Value::is_object) && !value.is_object() {
            return Err(anyhow!("conflicting key `{key}`"));
        }
        current[*last] = value;
    }
    Ok(())
}

fn yaml_to_nested(value: serde_yaml::Value) -> anyhow::Result<serde_yaml::Value> {
    let mut nested = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    for (key, value) in flatten_yaml_entries("", value) {
        insert_yaml_value(&mut nested, &key, value)?;
    }
    Ok(nested)
}

fn yaml_to_flat(value: serde_yaml::Value) -> anyhow::Result<serde_yaml::Value> {
    let entries = flatten_yaml_entries("", value);
    let mut flat = serde_yaml::Mapping::new();
    for (key, value) in entries {
        let key = serde_yaml::Value::String(key);
        if flat.contains_key(&key) {
            return Err(anyhow!("conflicting key `{}`", yaml_sort_key(&key)));
        }
        flat.insert(key, value);
    }
    Ok(serde_yaml::Value::Mapping(flat))
}

fn flatten_yaml_entries(
    prefix: &str,
    value: serde_yaml::Value,
) -> Vec<(String, serde_yaml::Value)> {
    match value {
        serde_yaml::Value::Mapping(map) => {
            let mut entries = Vec::new();
            for (key, child) in map {
                let key = yaml_sort_key(&key);
                let next_key = join_key(prefix, &key);
                if child.is_mapping() {
                    entries.extend(flatten_yaml_entries(&next_key, child));
                } else {
                    entries.push((next_key, child));
                }
            }
            entries
        }
        other if !prefix.is_empty() => vec![(prefix.to_string(), other)],
        other => vec![(String::new(), other)],
    }
}

fn insert_yaml_value(
    yaml: &mut serde_yaml::Value,
    key: &str,
    value: serde_yaml::Value,
) -> anyhow::Result<()> {
    if !yaml.is_mapping() {
        *yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }

    let parts: Vec<&str> = key.split('.').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        *yaml = value;
        return Ok(());
    }

    let mut current = yaml;
    for part in &parts[..parts.len().saturating_sub(1)] {
        let key = serde_yaml::Value::String((*part).to_string());
        if current.get(&key).is_some_and(|child| !child.is_mapping()) {
            return Err(anyhow!("conflicting key `{}`", parts.join(".")));
        }
        if !current.get(&key).is_some_and(serde_yaml::Value::is_mapping) {
            current[key.clone()] = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }
        current = &mut current[key];
    }

    if let Some(last) = parts.last() {
        let key = serde_yaml::Value::String((*last).to_string());
        if current.get(&key).is_some_and(serde_yaml::Value::is_mapping) && !value.is_mapping() {
            return Err(anyhow!("conflicting key `{}`", parts.join(".")));
        }
        current[key] = value;
    }
    Ok(())
}

fn join_key(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn apply_missing_translations(
    workspace: &Path,
    config: &I18nConfig,
    report: &AuditReport,
    placeholder: Option<&str>,
) -> anyhow::Result<MissingWriteSummary> {
    let mut added = 0;
    let mut skipped = 0;

    for item in &report.missing {
        for locale in &item.missing_in {
            let Some(file) = find_locale_file(workspace, config, locale) else {
                skipped += 1;
                continue;
            };

            let value = placeholder.unwrap_or(&item.source_value);
            if add_translation_to_file(&file, &item.key, value)? {
                added += 1;
            } else {
                skipped += 1;
            }
        }
    }

    Ok(MissingWriteSummary { added, skipped })
}

fn add_translation_to_file(path: &Path, key: &str, value: &str) -> anyhow::Result<bool> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => add_json_translation(path, key, value).map(|_| true),
        Some("yaml") | Some("yml") => add_yaml_translation(path, key, value).map(|_| true),
        Some("arb") => add_arb_translation(path, key, value).map(|_| true),
        Some("php") => add_php_translation(path, key, value).map(|_| true),
        _ => Ok(false),
    }
}

fn add_json_translation(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read locale file {}", path.display()))?;
    let mut json: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse JSON locale file {}", path.display()))?;

    insert_json_key(&mut json, key, value);

    let mut output = serde_json::to_string_pretty(&json)?;
    output.push('\n');
    std::fs::write(path, output)
        .with_context(|| format!("Failed to write locale file {}", path.display()))?;
    Ok(())
}

fn add_yaml_translation(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read locale file {}", path.display()))?;
    let mut yaml: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse YAML locale file {}", path.display()))?;

    insert_yaml_key(&mut yaml, key, value);

    let output = serde_yaml::to_string(&yaml)?;
    std::fs::write(path, output)
        .with_context(|| format!("Failed to write locale file {}", path.display()))?;
    Ok(())
}

fn add_arb_translation(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read locale file {}", path.display()))?;
    let mut json: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse ARB locale file {}", path.display()))?;

    if !json.is_object() {
        json = serde_json::json!({});
    }
    json[key] = serde_json::Value::String(value.to_string());

    let mut output = serde_json::to_string_pretty(&json)?;
    output.push('\n');
    std::fs::write(path, output)
        .with_context(|| format!("Failed to write locale file {}", path.display()))?;
    Ok(())
}

fn add_php_translation(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read locale file {}", path.display()))?;
    let insert_at = content.rfind("];").ok_or_else(|| {
        anyhow!(
            "Failed to find closing PHP short array in locale file {}",
            path.display()
        )
    })?;

    let escaped_key = escape_php_single_quoted(key);
    let escaped_value = escape_php_single_quoted(value);
    let indent = detect_php_root_indent(&content);
    let mut output = String::new();
    output.push_str(&content[..insert_at]);
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&format!("{indent}'{escaped_key}' => '{escaped_value}',\n"));
    output.push_str(&content[insert_at..]);

    std::fs::write(path, output)
        .with_context(|| format!("Failed to write locale file {}", path.display()))?;
    Ok(())
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

fn format_fix_dry_run(workspace: &Path, config: &I18nConfig, report: &AuditReport) -> String {
    let mut output = String::new();
    output.push_str("i18n Fix Dry Run\n\n");
    output.push_str(&format!(
        "Missing translations: {}\n",
        report.summary.missing_translations
    ));
    output.push_str(&format!("Unused keys: {}\n", report.summary.unused_keys));
    output.push_str(&format!(
        "Placeholder issues: {}\n\n",
        report.summary.placeholder_mismatches
    ));

    if report.missing.is_empty() && report.unused.is_empty() && report.placeholder_issues.is_empty()
    {
        output.push_str("No fixes to suggest.\n");
        return output;
    }

    if !report.missing.is_empty() {
        output.push_str("Missing translations\n");
        for item in &report.missing {
            output.push_str(&format!("- {}\n", item.key));
            output.push_str(&format!(
                "  action: {}\n",
                fix_action(item.suggestion.as_ref())
            ));
            output.push_str(&format!(
                "  source: {} = {}\n",
                item.source_locale, item.source_value
            ));
            output.push_str(&format!("  missing in: {}\n", item.missing_in.join(", ")));
            output.push_str("  files:\n");
            for file in fix_files(item.suggestion.as_ref()) {
                output.push_str(&format!("    - {}\n", relative_path(workspace, &file)));
            }
        }
        output.push('\n');
    }

    if !report.unused.is_empty() {
        output.push_str("Unused keys\n");
        for item in &report.unused {
            output.push_str(&format!("- {}\n", item.key));
            output.push_str(&format!(
                "  action: {}\n",
                fix_action(item.suggestion.as_ref())
            ));
            output.push_str(&format!(
                "  defined in: {}\n",
                relative_path(workspace, &item.defined_in.file_path)
            ));
        }
        output.push('\n');
    }

    if !report.placeholder_issues.is_empty() {
        output.push_str("Placeholder issues\n");
        for item in &report.placeholder_issues {
            let mut locales: Vec<&String> = item.locale_values.keys().collect();
            locales.sort();
            output.push_str(&format!("- {}\n", item.key));
            output.push_str("  action: review_placeholder_mismatch\n");
            output.push_str(&format!(
                "  expected placeholders: {}\n",
                item.expected_placeholders.join(", ")
            ));
            output.push_str(&format!(
                "  mismatched locales: {}\n",
                locales
                    .iter()
                    .map(|locale| locale.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            output.push_str("  files:\n");
            for locale in locales {
                if let Some(file) = find_locale_file(workspace, config, locale) {
                    output.push_str(&format!("    - {}\n", relative_path(workspace, &file)));
                }
            }
        }
    }

    output
}

fn fix_action(suggestion: Option<&crate::audit::FixSuggestion>) -> &str {
    suggestion
        .map(|suggestion| suggestion.action.as_str())
        .unwrap_or("review")
}

fn fix_files(suggestion: Option<&crate::audit::FixSuggestion>) -> Vec<PathBuf> {
    suggestion
        .map(|suggestion| suggestion.files_to_edit.clone())
        .unwrap_or_default()
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

fn find_locale_file(workspace: &Path, config: &I18nConfig, locale: &str) -> Option<PathBuf> {
    for locale_path in &config.locale_paths {
        let base = workspace.join(locale_path);
        for extension in ["json", "yaml", "yml", "arb", "php"] {
            let candidate = base.join(format!("{locale}.{extension}"));
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

fn format_terminal(report: &AuditReport, suggest_fixes: bool) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "{}\n\n",
        "╔══════════════════════════════════════╗".blue().bold()
    ));
    output.push_str(&format!(
        "{}\n",
        "║      i18n Audit Report               ║".blue().bold()
    ));
    output.push_str(&format!(
        "{}\n\n",
        "╚══════════════════════════════════════╝".blue().bold()
    ));

    output.push_str(&"Summary\n".bold().underline().to_string());
    output.push_str(&format!(
        "  Total Keys:        {}\n",
        report.summary.total_keys.to_string().cyan()
    ));
    output.push_str(&format!(
        "  Total Locales:     {}\n",
        report.summary.total_locales.to_string().cyan()
    ));

    let missing_str = if report.summary.missing_translations > 0 {
        report.summary.missing_translations.to_string().red().bold()
    } else {
        report.summary.missing_translations.to_string().green()
    };
    output.push_str(&format!("  Missing Translations: {}\n", missing_str));

    let unused_str = if report.summary.unused_keys > 0 {
        report.summary.unused_keys.to_string().yellow().bold()
    } else {
        report.summary.unused_keys.to_string().green()
    };
    output.push_str(&format!("  Unused Keys:       {}\n", unused_str));

    if report.summary.placeholder_mismatches > 0 {
        output.push_str(&format!(
            "  Placeholder Issues: {}\n",
            report
                .summary
                .placeholder_mismatches
                .to_string()
                .red()
                .bold()
        ));
    }
    output.push('\n');

    if !report.missing.is_empty() {
        output.push_str(&format!(
            "{}",
            "Missing Translations\n".red().bold().underline()
        ));
        for item in &report.missing {
            output.push_str(&format!("  {} {}\n", "•".red(), item.key.yellow()));
            output.push_str(&format!(
                "    Source ({}): {}\n",
                item.source_locale,
                item.source_value.dimmed()
            ));
            output.push_str(&format!(
                "    Missing in: {}\n",
                item.missing_in.join(", ").red()
            ));

            if !item.used_in.is_empty() {
                output.push_str("    Used in:\n");
                for usage in &item.used_in {
                    output.push_str(&format!(
                        "      - {}:{}\n",
                        usage.file.display().to_string().dimmed(),
                        usage.line + 1
                    ));
                }
            }

            if suggest_fixes {
                if let Some(sugg) = item.suggestion.as_ref() {
                    output.push_str(&format!(
                        "    {} {}\n",
                        "→".green(),
                        "Suggestion:".green().bold()
                    ));
                    output.push_str(&format!("      Action: {}\n", sugg.action.green()));
                    if !sugg.files_to_edit.is_empty() {
                        output.push_str("      Files to edit:\n");
                        for f in &sugg.files_to_edit {
                            output.push_str(&format!(
                                "        - {}\n",
                                f.display().to_string().green()
                            ));
                        }
                    }
                }
            }
            output.push('\n');
        }
    }

    if !report.unused.is_empty() {
        output.push_str(&"Unused Keys\n".yellow().bold().underline().to_string());
        for item in &report.unused {
            output.push_str(&format!("  {} {}\n", "•".yellow(), item.key.dimmed()));
            output.push_str(&format!(
                "    Defined in: {}:{}\n",
                item.defined_in.file_path.display().to_string().dimmed(),
                item.defined_in.line
            ));
            output.push('\n');
        }
    }

    if !report.placeholder_issues.is_empty() {
        output.push_str(&format!(
            "{}",
            "Placeholder Issues\n".red().bold().underline()
        ));
        for item in &report.placeholder_issues {
            output.push_str(&format!("  {} {}\n", "•".red(), item.key.yellow()));
            output.push_str(&format!(
                "    Expected placeholders: {}\n",
                item.expected_placeholders.join(", ").cyan()
            ));
            output.push_str("    Mismatched locales:\n");
            for (locale, value) in &item.locale_values {
                output.push_str(&format!("      {}: {}\n", locale.red(), value));
            }
            output.push('\n');
        }
    }

    if report.missing.is_empty() && report.unused.is_empty() && report.placeholder_issues.is_empty()
    {
        output.push_str(&format!("{}\n", "✓ All i18n checks passed!".green().bold()));
    }

    output
}

fn format_markdown(report: &AuditReport, suggest_fixes: bool) -> String {
    let mut md = String::new();

    md.push_str("# i18n Audit Report\n\n");

    md.push_str("## Summary\n\n");
    md.push_str("| Metric | Count |\n");
    md.push_str("|--------|-------|\n");
    md.push_str(&format!("| Total Keys | {} |\n", report.summary.total_keys));
    md.push_str(&format!(
        "| Total Locales | {} |\n",
        report.summary.total_locales
    ));

    let missing_badge = if report.summary.missing_translations > 0 {
        format!("**{}** ⚠️", report.summary.missing_translations)
    } else {
        format!("{} ✓", report.summary.missing_translations)
    };
    md.push_str(&format!("| Missing Translations | {} |\n", missing_badge));

    let unused_badge = if report.summary.unused_keys > 0 {
        format!("**{}** ⚠️", report.summary.unused_keys)
    } else {
        format!("{} ✓", report.summary.unused_keys)
    };
    md.push_str(&format!("| Unused Keys | {} |\n", unused_badge));

    if report.summary.placeholder_mismatches > 0 {
        md.push_str(&format!(
            "| Placeholder Issues | **{}** ⚠️ |\n",
            report.summary.placeholder_mismatches
        ));
    }
    md.push('\n');

    if !report.missing.is_empty() {
        md.push_str("## Missing Translations\n\n");
        for item in &report.missing {
            md.push_str(&format!("### `{}`\n\n", item.key));
            md.push_str(&format!(
                "- **Source ({}):** {}\n",
                item.source_locale, item.source_value
            ));
            md.push_str(&format!(
                "- **Missing in:** `{}`\n",
                item.missing_in.join("`, `")
            ));

            if !item.used_in.is_empty() {
                md.push_str("- **Used in:**\n");
                for usage in &item.used_in {
                    md.push_str(&format!(
                        "  - `{}:{}`\n",
                        usage.file.display(),
                        usage.line + 1
                    ));
                }
            }

            if suggest_fixes {
                if let Some(sugg) = item.suggestion.as_ref() {
                    md.push_str("\n**Suggestion:**\n");
                    md.push_str(&format!("- Action: `{}`\n", sugg.action));
                    if !sugg.files_to_edit.is_empty() {
                        md.push_str("- Files to edit:\n");
                        for f in &sugg.files_to_edit {
                            md.push_str(&format!("  - `{}`\n", f.display()));
                        }
                    }
                }
            }
            md.push('\n');
        }
    }

    if !report.unused.is_empty() {
        md.push_str("## Unused Keys\n\n");
        for item in &report.unused {
            md.push_str(&format!(
                "- `{}` - defined in `{}:{}`\n",
                item.key,
                item.defined_in.file_path.display(),
                item.defined_in.line
            ));
        }
        md.push('\n');
    }

    if !report.placeholder_issues.is_empty() {
        md.push_str("## Placeholder Issues\n\n");
        for item in &report.placeholder_issues {
            md.push_str(&format!("### `{}`\n\n", item.key));
            md.push_str(&format!(
                "- **Expected placeholders:** `{}`\n",
                item.expected_placeholders.join("`, `")
            ));
            md.push_str("- **Mismatched locales:**\n");
            for (locale, value) in &item.locale_values {
                md.push_str(&format!("  - `{}`: `{}`\n", locale, value));
            }
            md.push('\n');
        }
    }

    if report.missing.is_empty() && report.unused.is_empty() && report.placeholder_issues.is_empty()
    {
        md.push_str("## ✓ All i18n checks passed!\n");
    }

    md
}
