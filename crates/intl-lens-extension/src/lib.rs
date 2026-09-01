use zed_extension_api::{self as zed, LanguageServerId, Result, Worktree};

struct IntlLensExtension {
    cached_binary_path: Option<String>,
}

impl zed::Extension for IntlLensExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let binary_path = self.get_server_binary_path(language_server_id, worktree)?;

        Ok(zed::Command {
            command: binary_path,
            args: vec![],
            env: vec![],
        })
    }
}

impl IntlLensExtension {
    fn get_server_binary_path(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<String> {
        if let Some(path) = &self.cached_binary_path {
            if std::fs::metadata(path).is_ok() {
                return Ok(path.clone());
            }
        }

        if let Some(path) = worktree.which("i18nlens") {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        if let Some(path) = worktree.which("intl-lens") {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        let (platform, arch) = zed::current_platform();

        let binary_path = match Self::download_latest(language_server_id, platform, arch) {
            Ok(path) => path,
            Err(err) => Self::newest_installed_binary(platform).ok_or(err)?,
        };

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }

    fn download_latest(
        language_server_id: &LanguageServerId,
        platform: zed::Os,
        arch: zed::Architecture,
    ) -> Result<String> {
        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let release = zed::latest_github_release(
            "nguyenphutrong/i18nlens",
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let asset_name = format!(
            "i18nlens-{}-{}.{}",
            match arch {
                zed::Architecture::Aarch64 => "aarch64",
                zed::Architecture::X8664 => "x86_64",
                zed::Architecture::X86 => "x86",
            },
            match platform {
                zed::Os::Mac => "apple-darwin",
                zed::Os::Linux => "unknown-linux-gnu",
                zed::Os::Windows => "pc-windows-msvc",
            },
            match platform {
                zed::Os::Windows => "zip",
                _ => "tar.gz",
            }
        );

        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("no asset found matching {asset_name}"))?;

        let version_dir = format!("i18nlens-{}", release.version);
        let binary_path = format!(
            "{version_dir}/i18nlens{}",
            match platform {
                zed::Os::Windows => ".exe",
                _ => "",
            }
        );

        if std::fs::metadata(&binary_path).is_err() {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );

            let file_type = match platform {
                zed::Os::Windows => zed::DownloadedFileType::Zip,
                _ => zed::DownloadedFileType::GzipTar,
            };

            zed::download_file(&asset.download_url, &version_dir, file_type)?;

            zed::make_file_executable(&binary_path)?;
        }

        Ok(binary_path)
    }

    /// Finds the newest already-downloaded server binary, so that a failed
    /// release check (e.g. an anonymous GitHub API rate limit 403 on a shared
    /// IP) doesn't prevent an installed server from starting.
    fn newest_installed_binary(platform: zed::Os) -> Option<String> {
        let suffix = match platform {
            zed::Os::Windows => ".exe",
            _ => "",
        };

        std::fs::read_dir(".")
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let dir_name = entry.file_name().into_string().ok()?;
                // Version dirs downloaded by extension versions before the
                // rename use the `intl-lens-` prefix and binary name.
                let server_name = ["i18nlens", "intl-lens"]
                    .into_iter()
                    .find(|name| dir_name.starts_with(&format!("{name}-")))?;
                let version = dir_name[server_name.len() + 1..]
                    .trim_start_matches('v')
                    .split('.')
                    .map(|part| part.parse::<u32>().ok())
                    .collect::<Option<Vec<u32>>>()?;

                let binary_path = format!("{dir_name}/{server_name}{suffix}");
                std::fs::metadata(&binary_path)
                    .is_ok()
                    .then_some((version, binary_path))
            })
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, binary_path)| binary_path)
    }
}

zed::register_extension!(IntlLensExtension);
