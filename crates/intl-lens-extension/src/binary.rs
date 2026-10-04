use std::fs;
use std::path::Path;

use object::{Object, ObjectSection, ObjectSegment};
use zed_extension_api::{Architecture, Os, Result};

pub(super) fn prepare_binary(
    path: &str,
    platform: Os,
    architecture: Architecture,
    make_executable: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|err| err.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("{path} is not a non-empty server binary"));
    }

    // Read headers and tables through a seekable cache, not the whole executable.
    let file = fs::File::open(path).map_err(|err| err.to_string())?;
    let data = object::read::ReadCache::new(file);
    let binary =
        object::File::parse(&data).map_err(|err| format!("invalid binary {path}: {err}"))?;
    let format = match platform {
        Os::Mac => object::BinaryFormat::MachO,
        Os::Linux => object::BinaryFormat::Elf,
        Os::Windows => object::BinaryFormat::Pe,
    };
    let architecture = match architecture {
        Architecture::Aarch64 => object::Architecture::Aarch64,
        Architecture::X8664 => object::Architecture::X86_64,
        Architecture::X86 => object::Architecture::I386,
    };
    if binary.format() != format
        || binary.architecture() != architecture
        || !matches!(
            binary.kind(),
            object::ObjectKind::Executable | object::ObjectKind::Dynamic
        )
    {
        return Err(format!(
            "{path} is not a server executable for this platform"
        ));
    }

    // The generic Mach-O iterators discard load-command parsing errors.
    let commands = match &binary {
        object::File::MachO32(file) => Some(file.macho_load_commands()),
        object::File::MachO64(file) => Some(file.macho_load_commands()),
        _ => None,
    };
    if let Some(commands) = commands {
        let mut commands = commands.map_err(|err| format!("invalid binary {path}: {err}"))?;
        while commands
            .next()
            .map_err(|err| format!("invalid binary {path}: {err}"))?
            .is_some()
        {}
    }

    let in_bounds = |(offset, size): (u64, u64)| {
        offset
            .checked_add(size)
            .is_some_and(|end| end <= metadata.len())
    };
    let mut has_payload = false;
    for segment in binary.segments() {
        let range = segment.file_range();
        if !in_bounds(range) {
            return Err(format!("{path} is a truncated server binary"));
        }
        has_payload |= range.1 != 0;
    }
    if !has_payload
        || binary
            .sections()
            .filter_map(|section| section.file_range())
            .any(|range| !in_bounds(range))
    {
        return Err(format!("{path} has no complete executable payload"));
    }

    make_executable(path)
}

pub(super) fn newest_installed_binary(
    directory: &Path,
    platform: Os,
    mut prepare: impl FnMut(&str) -> Result<()>,
) -> Option<String> {
    let mut candidates = fs::read_dir(directory)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let name = entry.file_name();
            let name = name.to_str()?;
            let (server, version) = name
                .strip_prefix("i18nlens-")
                .map(|version| ("i18nlens", version))
                .or_else(|| {
                    name.strip_prefix("intl-lens-")
                        .map(|version| ("intl-lens", version))
                })?;
            let mut parts = version.trim_start_matches('v').split('.');
            let version = [
                parts.next()?.parse::<u32>().ok()?,
                parts.next()?.parse::<u32>().ok()?,
                parts.next()?.parse::<u32>().ok()?,
            ];
            if parts.next().is_some() {
                return None;
            }
            let binary_name = match (server, platform) {
                ("i18nlens", Os::Windows) => "i18nlens.exe",
                ("intl-lens", Os::Windows) => "intl-lens.exe",
                _ => server,
            };
            Some((version, entry.path().join(binary_name)))
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    candidates.into_iter().find_map(|(_, path)| {
        let path = path.to_str()?;
        prepare(path).ok().map(|()| path.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn native_platform() -> (Os, Architecture) {
        let os = match std::env::consts::OS {
            "macos" => Os::Mac,
            "linux" => Os::Linux,
            "windows" => Os::Windows,
            other => panic!("unsupported test OS: {other}"),
        };
        let arch = match std::env::consts::ARCH {
            "aarch64" => Architecture::Aarch64,
            "x86_64" => Architecture::X8664,
            "x86" => Architecture::X86,
            other => panic!("unsupported test architecture: {other}"),
        };
        (os, arch)
    }

    fn installed_binary(root: &Path, server: &str, version: &str) -> String {
        let directory = root.join(format!("{server}-v{version}"));
        fs::create_dir_all(&directory).unwrap();
        let name = if cfg!(windows) {
            format!("{server}.exe")
        } else {
            server.to_owned()
        };
        let path = directory.join(name);
        fs::copy(std::env::current_exe().unwrap(), &path).unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn make_executable(path: &str) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path)
                .map_err(|err| err.to_string())?
                .permissions();
            permissions.set_mode(permissions.mode() | 0o111);
            fs::set_permissions(path, permissions).map_err(|err| err.to_string())?;
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    }

    fn prepare(path: &str) -> Result<()> {
        let (os, arch) = native_platform();
        prepare_binary(path, os, arch, make_executable)
    }

    fn select(root: &Path) -> Option<String> {
        newest_installed_binary(root, native_platform().0, prepare)
    }

    #[test]
    fn truncated_newer_binary_does_not_mask_installed_server() {
        let root = TempDir::new().unwrap();
        let older = installed_binary(root.path(), "i18nlens", "0.1.10");
        let newer = installed_binary(root.path(), "i18nlens", "0.1.11");
        fs::OpenOptions::new()
            .write(true)
            .open(newer)
            .unwrap()
            .set_len(512)
            .unwrap();
        assert_eq!(select(root.path()), Some(older));
    }

    #[test]
    fn intact_headers_do_not_make_a_truncated_payload_usable() {
        let root = TempDir::new().unwrap();
        let older = installed_binary(root.path(), "i18nlens", "0.1.10");
        let newer = installed_binary(root.path(), "i18nlens", "0.1.11");
        let length = fs::metadata(&newer).unwrap().len();
        fs::OpenOptions::new()
            .write(true)
            .open(newer)
            .unwrap()
            .set_len(length / 2)
            .unwrap();
        assert_eq!(select(root.path()), Some(older));
    }

    #[test]
    fn directories_and_empty_files_are_not_server_binaries() {
        let root = TempDir::new().unwrap();
        let older = installed_binary(root.path(), "i18nlens", "0.1.10");
        let directory = installed_binary(root.path(), "i18nlens", "0.1.11");
        fs::remove_file(&directory).unwrap();
        fs::create_dir(directory).unwrap();
        let empty = installed_binary(root.path(), "i18nlens", "0.1.12");
        fs::write(empty, []).unwrap();
        assert_eq!(select(root.path()), Some(older));
    }

    #[cfg(unix)]
    #[test]
    fn missing_execute_permissions_are_repaired_before_selection() {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new().unwrap();
        let path = installed_binary(root.path(), "i18nlens", "0.1.11");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(select(root.path()), Some(path.clone()));
        let output = std::process::Command::new(path)
            .arg("--list")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "selected binary cannot run: {:?}",
            output.status
        );
    }

    #[test]
    fn preparation_failure_uses_the_older_legacy_binary() {
        let root = TempDir::new().unwrap();
        let older = installed_binary(root.path(), "intl-lens", "0.1.9");
        let newer = installed_binary(root.path(), "i18nlens", "0.1.10");
        let selected = newest_installed_binary(root.path(), native_platform().0, |path| {
            let (os, arch) = native_platform();
            prepare_binary(path, os, arch, |path| {
                if path == newer {
                    Err("permission denied".into())
                } else {
                    make_executable(path)
                }
            })
        });
        assert_eq!(selected, Some(older));
    }

    #[test]
    fn chooses_numeric_newest_version_across_both_names() {
        let root = TempDir::new().unwrap();
        installed_binary(root.path(), "intl-lens", "0.1.9");
        let newer = installed_binary(root.path(), "i18nlens", "0.1.10");
        assert_eq!(select(root.path()), Some(newer));
    }

    #[test]
    fn wrong_platform_or_architecture_is_rejected() {
        let root = TempDir::new().unwrap();
        let path = installed_binary(root.path(), "i18nlens", "0.1.11");
        let (os, arch) = native_platform();
        let other_os = if os == Os::Mac { Os::Linux } else { Os::Mac };
        let other_arch = if arch == Architecture::Aarch64 {
            Architecture::X8664
        } else {
            Architecture::Aarch64
        };
        assert!(prepare_binary(&path, other_os, arch, make_executable).is_err());
        assert!(prepare_binary(&path, os, other_arch, make_executable).is_err());
    }

    #[test]
    fn no_usable_installation_returns_none() {
        let root = TempDir::new().unwrap();
        let path = installed_binary(root.path(), "i18nlens", "0.1.11");
        fs::write(path, b"unfinished download").unwrap();
        assert_eq!(select(root.path()), None);
    }
}
