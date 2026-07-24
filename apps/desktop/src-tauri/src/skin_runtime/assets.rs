use crate::error::{CodexxError, Result};
use crate::file_io::{atomic_write, ensure_directory};
use crate::skins::skins_root;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) const RUNTIME_VERSION: &str = "1.2.2-codexx.3";
pub(super) const UPSTREAM_COMMIT: &str = "5fd8af532efbaa87d2d0092297fd2d45cd56574e";

struct EmbeddedRuntimeAsset {
    relative_path: &'static str,
    bytes: &'static [u8],
}

fn embedded_assets() -> &'static [EmbeddedRuntimeAsset] {
    &[
        EmbeddedRuntimeAsset {
            relative_path: "scripts/injector.mjs",
            bytes: include_bytes!("../../resources/skin-runtime/scripts/injector.mjs"),
        },
        EmbeddedRuntimeAsset {
            relative_path: "scripts/image-metadata.mjs",
            bytes: include_bytes!("../../resources/skin-runtime/scripts/image-metadata.mjs"),
        },
        EmbeddedRuntimeAsset {
            relative_path: "scripts/stage-theme.mjs",
            bytes: include_bytes!("../../resources/skin-runtime/scripts/stage-theme.mjs"),
        },
        EmbeddedRuntimeAsset {
            relative_path: "assets/dream-skin.css",
            bytes: include_bytes!("../../resources/skin-runtime/assets/dream-skin.css"),
        },
        EmbeddedRuntimeAsset {
            relative_path: "assets/renderer-inject.js",
            bytes: include_bytes!("../../resources/skin-runtime/assets/renderer-inject.js"),
        },
        EmbeddedRuntimeAsset {
            relative_path: "windows/codexx-windows.ps1",
            bytes: include_bytes!("../../resources/skin-runtime/windows/codexx-windows.ps1"),
        },
        EmbeddedRuntimeAsset {
            relative_path: "windows/common-windows.ps1",
            bytes: include_bytes!("../../resources/skin-runtime/windows/common-windows.ps1"),
        },
        EmbeddedRuntimeAsset {
            relative_path: "windows/config-utf8.ps1",
            bytes: include_bytes!("../../resources/skin-runtime/windows/config-utf8.ps1"),
        },
        EmbeddedRuntimeAsset {
            relative_path: "LICENSE",
            bytes: include_bytes!("../../resources/skin-runtime/LICENSE"),
        },
        EmbeddedRuntimeAsset {
            relative_path: "NOTICE.md",
            bytes: include_bytes!("../../resources/skin-runtime/NOTICE.md"),
        },
    ]
}

fn bytes_match(path: &Path, expected: &[u8]) -> bool {
    if !fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
    {
        return false;
    }
    let Ok(actual) = fs::read(path) else {
        return false;
    };
    Sha256::digest(actual) == Sha256::digest(expected)
}

pub(super) fn ensure_runtime_directory(path: &Path) -> Result<()> {
    ensure_directory(path)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|source| crate::file_io::io_err(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CodexxError::Config(format!(
            "皮肤运行时目录不能是符号链接或普通文件: {}",
            path.display()
        )));
    }
    set_private_permissions(path, true)
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, directory: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if directory { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|source| crate::file_io::io_err(path, source))
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _directory: bool) -> Result<()> {
    Ok(())
}

pub(super) struct RuntimeAssets {
    pub(super) injector: PathBuf,
    pub(super) stage_theme: PathBuf,
    #[cfg(any(target_os = "windows", feature = "windows-runtime-check"))]
    #[cfg_attr(feature = "windows-runtime-check", allow(dead_code))]
    pub(super) windows_adapter: PathBuf,
}

pub(super) fn ensure_runtime_assets() -> Result<RuntimeAssets> {
    let base = skins_root()?.join("runtime");
    ensure_runtime_directory(&base)?;
    let root = base.join(RUNTIME_VERSION);
    ensure_runtime_directory(&root)?;
    for asset in embedded_assets() {
        let path = root.join(asset.relative_path);
        let parent = path.parent().ok_or_else(|| {
            CodexxError::Config(format!("内置皮肤资源路径无效: {}", path.display()))
        })?;
        ensure_runtime_directory(parent)?;
        if !bytes_match(&path, asset.bytes) {
            atomic_write(&path, asset.bytes)?;
        }
        set_private_permissions(&path, false)?;
    }
    Ok(RuntimeAssets {
        injector: root.join("scripts/injector.mjs"),
        stage_theme: root.join("scripts/stage-theme.mjs"),
        #[cfg(any(target_os = "windows", feature = "windows-runtime-check"))]
        windows_adapter: root.join("windows/codexx-windows.ps1"),
    })
}
