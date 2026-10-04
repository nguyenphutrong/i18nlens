# I18n Lens

I18n Lens is an i18n intelligence layer for codebases.

It runs in editors through LSP, in CI through a CLI, and in AI coding workflows through an MCP server. The Zed extension is the first editor integration, not the whole product.

[Features](#features) | [Install](#install) | [CLI](#cli) | [MCP](#mcp) | [Configure](#configuration) | [Roadmap](ROADMAP.md)

## Features

I18n Lens helps you answer the questions that usually require opening translation files by hand:

- What does this key mean?
- Is this key defined?
- Which locales are missing a value?
- Which translation keys are no longer used?
- Do translated placeholders still match the source locale?

Current surfaces:

| Surface | What it does |
|---------|--------------|
| LSP / Zed | Inline translation hints, hover previews, missing-key diagnostics, autocomplete, go to definition, auto reload |
| CLI | Project audits, file checks, terminal / JSON / Markdown output, CI-friendly exit codes |
| MCP | Agent tools for audit data, missing translations, fix suggestions, and placeholder validation |
| Rust library | Reusable scanner, parser, config, audit, and translation store modules |

## Install

### One-Off CLI

After the npm package is published, you can run I18n Lens without a global install:

```bash
npx @i18nlens/cli audit
bunx @i18nlens/cli fix --to-nested
```

### Zed Extension

1. Open Zed.
2. Go to Extensions with `cmd+shift+x`.
3. Search for `I18n Lens`.
4. Install the extension.

The extension launches the `i18nlens` language server.

The extension checks for updates through GitHub's release page. Before reusing a downloaded server, it checks the executable format, architecture, and file ranges, then sets execute permissions. If an update check or download fails, it tries installed versions from newest to oldest, including `intl-lens` installs. It skips incomplete files or versions whose permissions cannot be set. If none can be prepared, it returns the original update or download error.

### Build from Source

```bash
git clone https://github.com/nguyenphutrong/i18nlens.git
cd i18nlens
cargo build --release
```

This builds:

- `target/release/i18nlens`
- `target/release/i18nlens-mcp`
- compatibility aliases: `target/release/intl-lens`, `target/release/intl-lens-cli`, `target/release/intl-lens-mcp`

Put the binaries on your `PATH` if you want to run them from other projects.

```bash
ln -sf "$(pwd)/target/release/i18nlens" ~/.local/bin/i18nlens
ln -sf "$(pwd)/target/release/i18nlens-mcp" ~/.local/bin/i18nlens-mcp
```

`intl-lens`, `intl-lens-cli`, and `intl-lens-mcp` are still built as compatibility aliases, but the public commands are `i18nlens` and `i18nlens-mcp`.

## Editor Usage

When you write code like this:

```tsx
<button>{t("common.actions.submit")}</button>
```

I18n Lens can show the source translation inline, display all locale values on hover, warn when the key is missing, and jump to the translation definition.

Manual Zed configuration example:

```jsonc
{
  "lsp": {
    "i18nlens": {
      "binary": { "path": "i18nlens" }
    }
  },
  "languages": {
    "TSX": {
      "language_servers": ["typescript-language-server", "i18nlens", "..."]
    },
    "TypeScript": {
      "language_servers": ["typescript-language-server", "i18nlens", "..."]
    }
  }
}
```

## CLI

Run a full audit:

```bash
i18nlens audit
```

Write machine-readable output for CI or another tool:

```bash
i18nlens audit --format json --output i18n-report.json
```

Write a Markdown report:

```bash
i18nlens audit --format markdown --output i18n-report.md
```

Check specific files:

```bash
i18nlens check src/components/Checkout.tsx src/pages/Home.tsx
```

Include AI-ready fix suggestions:

```bash
i18nlens audit --suggest-fixes --format json
```

Preview and apply missing-key fixes:

```bash
i18nlens fix --dry-run
i18nlens fix --add-missing --placeholder "_TODO_"
i18nlens fix --sort-keys
i18nlens fix --to-nested --sort-keys
i18nlens fix --to-flat --sort-keys
```

`fix --add-missing` currently writes JSON, YAML, PHP, and ARB locale files. `fix --sort-keys` currently sorts JSON, YAML, and ARB locale files. `fix --to-nested` and `fix --to-flat` currently convert JSON and YAML locale files and can be combined with `--sort-keys`. Other write paths are tracked in [ROADMAP.md](ROADMAP.md).

`audit` and `check` return a non-zero exit code when I18n Lens finds missing or unused keys. `ci` uses stricter CI defaults: it fails on missing translations and placeholder mismatches, and it auto-loads `.i18nlens-baseline.json` when that file exists.

CI policy examples:

```bash
i18nlens ci --fail-on missing,placeholder --max-unused 20
i18nlens audit --fail-on missing,unused,placeholder
i18nlens audit --ignore-key-pattern '^legacy\.'
i18nlens ci --ignore-file 'src/generated/**'
```

Baseline flow for projects with existing i18n debt:

```bash
i18nlens audit --write-baseline .i18nlens-baseline.json
i18nlens ci
```

### GitHub Actions Example

```yaml
name: i18n

on:
  pull_request:

jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: nguyenphutrong/i18nlens@v0.1.6
        with:
          fail-on: missing,placeholder
          format: markdown
          output: i18n-report.md
```

The action installs the matching release binary and runs `i18nlens ci`.

### GitLab CI Example

```yaml
include:
  - local: examples/gitlab-ci.yml
```

Or copy [examples/gitlab-ci.yml](examples/gitlab-ci.yml) into your pipeline and adjust `I18NLENS_VERSION`.

## MCP

Start the MCP server from the project you want to inspect:

```bash
i18nlens-mcp
```

Available tools:

| Tool | Purpose |
|------|---------|
| `audit_i18n` | Return missing translations, unused keys, placeholder issues, and summary data |
| `get_missing_translations` | Filter missing keys by locale and optionally include source usage context |
| `suggest_translation_fixes` | Return file targets and source text for a missing key |
| `translate_missing_keys` | Return dry-run diffs for caller-provided translations of missing keys |
| `apply_translation_patch` | Apply or dry-run caller-provided translations; defaults to dry-run |
| `validate_placeholders` | Check placeholder consistency for one key |
| `get_translation_context` | Return locale values, missing locales, usage context, and target files for one key |
| `review_i18n_pr` | Return a structured PR-style i18n review and Markdown comment |
| `extract_hardcoded_strings` | Return candidate hardcoded user-facing strings from source files |

Available resources:

| Resource | Purpose |
|----------|---------|
| `i18nlens://config` | Resolved i18n config |
| `i18nlens://audit/latest` | Fresh audit report |
| `i18nlens://translations/index` | Loaded locales and key count |

Example agent workflow:

```text
Run i18nlens audit, list keys missing in Vietnamese, provide translated values, get dry-run diffs, then validate placeholders.
```

## Configuration

I18n Lens looks for configuration in this order:

1. `.i18nlens.json`
2. `i18nlens.config.json`
3. `.intl-lens.json`
4. `intl-lens.config.json`
5. `.zed/i18n.json`

Example:

```json
{
  "localePaths": ["src/locales", "public/locales"],
  "sourceLocale": "en",
  "keyStyle": "auto",
  "displayMode": "inlayHints",
  "namespaceEnabled": false
}
```

Options:

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `localePaths` | `string[]` | Common locale directories | Translation directories to scan |
| `sourceLocale` | `string` | `en` | Primary locale used as the source text |
| `keyStyle` | `nested`, `flat`, `auto` | `auto` | Translation key structure |
| `displayMode` | `inlayHints`, `codeLens` | `inlayHints` | LSP display mode |
| `namespaceEnabled` | `boolean` | `false` | Enables namespace-aware behavior |
| `functionPatterns` | `string[]` | Built-in framework patterns | Custom regex patterns for key detection |

Custom pattern example:

```json
{
  "functionPatterns": [
    "t\\s*\\(\\s*[\"']([^\"']+)[\"']",
    "translate\\s*\\(\\s*[\"']([^\"']+)[\"']"
  ]
}
```

## Supported Frameworks

| Framework | Patterns |
|-----------|----------|
| react-i18next | `t("key")`, `useTranslation()`, `<Trans i18nKey="key">` |
| i18next | `t("key")`, `i18n.t("key")` |
| vue-i18n | `$t("key")`, `$tc("key")`, `$te("key")` |
| react-intl | `formatMessage({ id: "key" })` |
| ngx-translate | `translateService.instant("key")`, `translateService.get("key")`, `| translate` |
| Transloco | `translocoService.translate("key")`, `selectTranslate("key")`, `| transloco` |
| Laravel | `__("key")`, `trans("key")`, `Lang::get("key")`, `@lang("key")` |
| Flutter gen_l10n | `AppLocalizations.of(context)!.key` |
| easy_localization | `'key'.tr()`, `tr("key")`, `context.tr("key")` |
| flutter_i18n | `FlutterI18n.translate(context, "key")`, `I18nText("key")` |
| GetX | `'key'.tr`, `'key'.trParams({})` |
| svelte-i18n | `$_("key")`, `$t("key")`, `$format("key")` |
| sveltekit-i18n | `$t("key")`, `t("key")` |

## Supported Source Files

- TypeScript / TSX
- JavaScript / JSX
- HTML
- Angular templates
- PHP
- Blade
- Dart / Flutter
- Vue
- Svelte

## Supported Translation Formats

| Format | Extensions |
|--------|------------|
| JSON | `.json` |
| YAML | `.yaml`, `.yml` |
| PHP | `.php` |
| ARB | `.arb` |

Directory-per-locale:

```text
locales/
  en/
    common.json
  vi/
    common.json
```

Flat files:

```text
locales/
  en.json
  vi.json
```

Flutter ARB:

```text
lib/
  l10n/
    app_en.arb
    app_vi.arb
```

## Development

```bash
cargo test
cargo build
cargo build --release
RUST_LOG=debug ./target/release/i18nlens
```

## Contributing

Good first areas:

- CLI tests for audit and check workflows
- MCP client compatibility tests
- Auto-fix dry-run implementation
- More translation file formats
- Better key detection with AST or tree-sitter

See [ROADMAP.md](ROADMAP.md) for the current product direction.

## License

MIT (c) [Trong Nguyen](https://github.com/nguyenphutrong)
