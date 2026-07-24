#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use crate::error::CodexxError;
use crate::error::Result;
use serde::Serialize;
use std::path::Path;

mod assets;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_injector;
#[cfg(target_os = "macos")]
mod macos_system;
mod node_runtime;
#[cfg(any(target_os = "windows", feature = "windows-runtime-check"))]
#[cfg_attr(feature = "windows-runtime-check", allow(dead_code))]
mod windows;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinRuntimeStatus {
    pub(crate) supported: bool,
    pub(crate) active: bool,
    pub(crate) phase: String,
    pub(crate) port: Option<u16>,
    pub(crate) theme_id: Option<String>,
    pub(crate) message: String,
}

impl SkinRuntimeStatus {
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    fn unsupported() -> Self {
        Self {
            supported: false,
            active: false,
            phase: "unsupported".to_string(),
            port: None,
            theme_id: None,
            message: "当前版本的 Codex 实机换肤仅支持 macOS 和 Windows".to_string(),
        }
    }
}

pub(crate) enum SkinRuntimeAction {
    Applied(String),
    RestartRequired(String),
    Paused(String),
    Restored(String),
}

pub(crate) fn skin_runtime_status() -> SkinRuntimeStatus {
    #[cfg(target_os = "macos")]
    {
        macos::runtime_status()
    }
    #[cfg(target_os = "windows")]
    {
        windows::runtime_status()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        SkinRuntimeStatus::unsupported()
    }
}

pub(crate) fn apply_skin_runtime(
    theme_dir: &Path,
    theme_id: &str,
    restart_existing: bool,
) -> Result<SkinRuntimeAction> {
    #[cfg(target_os = "macos")]
    {
        macos::apply_theme(theme_dir, theme_id, restart_existing)
    }
    #[cfg(target_os = "windows")]
    {
        windows::apply_theme(theme_dir, theme_id, restart_existing)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (theme_dir, theme_id, restart_existing);
        Err(CodexxError::Config(
            "当前版本的 Codex 实机换肤仅支持 macOS 和 Windows".to_string(),
        ))
    }
}

pub(crate) fn pause_skin_runtime() -> Result<SkinRuntimeAction> {
    #[cfg(target_os = "macos")]
    {
        macos::pause_theme()
    }
    #[cfg(target_os = "windows")]
    {
        windows::pause_theme()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err(CodexxError::Config(
            "当前版本的 Codex 实机换肤仅支持 macOS 和 Windows".to_string(),
        ))
    }
}

pub(crate) fn restore_skin_runtime(restart_existing: bool) -> Result<SkinRuntimeAction> {
    #[cfg(target_os = "macos")]
    {
        macos::restore_official(restart_existing)
    }
    #[cfg(target_os = "windows")]
    {
        windows::restore_official(restart_existing)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = restart_existing;
        Err(CodexxError::Config(
            "当前版本的 Codex 实机换肤仅支持 macOS 和 Windows".to_string(),
        ))
    }
}
