use zed_extension_api::http_client::{HttpMethod, HttpRequest, RedirectPolicy};
use zed_extension_api::{self as zed, LanguageServerId, Result, Worktree};

mod binary;

const GITHUB_REPO_URL: &str = "https://github.com/nguyenphutrong/i18nlens";

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
        let (platform, arch) = zed::current_platform();
        if let Some(path) = &self.cached_binary_path {
            if Self::prepare_binary(path, platform, arch).is_ok() {
                return Ok(path.clone());
            }
        }
        self.cached_binary_path = None;

        if let Some(path) = worktree.which("i18nlens") {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        if let Some(path) = worktree.which("intl-lens") {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        let binary_path = match Self::download_latest(language_server_id, platform, arch) {
            Ok(path) => path,
            Err(err) => {
                binary::newest_installed_binary(std::path::Path::new("."), platform, |path| {
                    Self::prepare_binary(path, platform, arch)
                })
                .ok_or(err)?
            }
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

        let (version, download_url_base) = Self::latest_release()?;

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

        let version_dir = format!("i18nlens-{version}");
        let binary_path = format!(
            "{version_dir}/i18nlens{}",
            match platform {
                zed::Os::Windows => ".exe",
                _ => "",
            }
        );

        if Self::prepare_binary(&binary_path, platform, arch).is_ok() {
            return Ok(binary_path);
        }
        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::Downloading,
        );

        let file_type = match platform {
            zed::Os::Windows => zed::DownloadedFileType::Zip,
            _ => zed::DownloadedFileType::GzipTar,
        };

        zed::download_file(
            &format!("{download_url_base}/{asset_name}"),
            &version_dir,
            file_type,
        )?;

        Self::prepare_binary(&binary_path, platform, arch)?;
        Ok(binary_path)
    }

    /// Resolves the latest release tag from the `Location` header of the
    /// `releases/latest` redirect on github.com, instead of calling
    /// api.github.com, which is limited to 60 anonymous requests per hour
    /// per IP and returns 403 for users behind shared IPs (VPNs, proxies,
    /// corporate NAT). Returns the tag and the release download URL base.
    fn latest_release() -> Result<(String, String)> {
        let mut url = format!("{GITHUB_REPO_URL}/releases/latest");

        // github.com answers with one redirect hop per repository rename,
        // plus the final redirect to the tag page.
        for _ in 0..4 {
            let response = HttpRequest::builder()
                .method(HttpMethod::Head)
                .url(&url)
                .redirect_policy(RedirectPolicy::NoFollow)
                .build()?
                .fetch()?;

            let location = response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("location"))
                .map(|(_, value)| value.clone())
                .ok_or("expected a redirect from the releases page (no releases published?)")?;

            if let Some((repo_url, version)) = location.split_once("/releases/tag/") {
                let download_url_base = format!("{repo_url}/releases/download/{version}");
                return Ok((version.to_string(), download_url_base));
            }

            url = location;
        }

        Err("too many redirects while resolving the latest release".into())
    }

    fn prepare_binary(path: &str, platform: zed::Os, arch: zed::Architecture) -> Result<()> {
        binary::prepare_binary(path, platform, arch, zed::make_file_executable)
    }
}

zed::register_extension!(IntlLensExtension);
