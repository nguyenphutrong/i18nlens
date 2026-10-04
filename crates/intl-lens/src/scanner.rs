use std::path::{Path, PathBuf};

use crate::i18n::key_finder::KeyFinder;

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub content: String,
    pub found_keys: Vec<CodeKeyOccurrence>,
}

#[derive(Debug, Clone)]
pub struct CodeKeyOccurrence {
    pub key: String,
    pub line: usize,
    pub start_char: usize,
    pub end_char: usize,
    pub code_snippet: String,
}

pub struct CodeScanner {
    key_finder: KeyFinder,
}

impl CodeScanner {
    pub fn new(patterns: &[String]) -> Self {
        Self {
            key_finder: KeyFinder::new(patterns),
        }
    }

    pub fn scan_directory(&self, root: &Path) -> Vec<ScannedFile> {
        let mut scanned_files = Vec::new();

        // Supported file extensions
        let extensions = [
            "ts",
            "tsx",
            "js",
            "jsx",
            "vue",
            "php",
            "blade.php",
            "dart",
            "svelte",
        ];

        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // Check if file has supported extension
            if let Some(ext) = path.extension() {
                let ext_str = ext.to_string_lossy().to_string();
                if !extensions.iter().any(|e| ext_str.contains(e)) {
                    continue;
                }
            } else {
                continue;
            }

            // Skip node_modules, .git, etc.
            let path_str = path.to_string_lossy();
            if path_str.contains("node_modules")
                || path_str.contains(".git")
                || path_str.contains("target")
                || path_str.contains("dist")
                || path_str.contains("build")
            {
                continue;
            }

            if let Ok(content) = std::fs::read_to_string(path) {
                let found_keys = self.scan_content(&content);
                if !found_keys.is_empty() {
                    scanned_files.push(ScannedFile {
                        path: path.to_path_buf(),
                        content,
                        found_keys,
                    });
                }
            }
        }

        scanned_files
    }

    pub fn scan_content(&self, content: &str) -> Vec<CodeKeyOccurrence> {
        let found = self.key_finder.find_keys(content);

        found
            .into_iter()
            .map(|k| {
                // Extract code snippet around the key
                let line_start = content
                    .lines()
                    .take(k.line)
                    .map(|l| l.len() + 1)
                    .sum::<usize>();
                let line_end = content
                    .lines()
                    .take(k.line + 1)
                    .map(|l| l.len() + 1)
                    .sum::<usize>();
                let snippet_start = line_start.saturating_sub(1);
                let snippet_end = (line_end - 1).min(content.len());
                let code_snippet = content[snippet_start..snippet_end].to_string();

                CodeKeyOccurrence {
                    key: k.key,
                    line: k.line,
                    start_char: k.start_char,
                    end_char: k.end_char,
                    code_snippet,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::I18nConfig;

    fn scanner() -> CodeScanner {
        CodeScanner::new(&I18nConfig::default().function_patterns)
    }

    #[test]
    fn scans_svelte_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Greeting.svelte");
        std::fs::write(
            &file,
            "<script>\n  import { _ } from 'svelte-i18n';\n</script>\n\n<h1>{$_('home.title')}</h1>\n",
        )
        .unwrap();

        let scanned = scanner().scan_directory(dir.path());

        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].path, file);
        assert_eq!(scanned[0].found_keys.len(), 1);
        assert_eq!(scanned[0].found_keys[0].key, "home.title");
        assert_eq!(scanned[0].found_keys[0].line, 4);
    }

    #[test]
    fn skips_unsupported_extensions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "{$_('home.title')}").unwrap();

        let scanned = scanner().scan_directory(dir.path());

        assert!(scanned.is_empty());
    }
}
