use super::assets::RuntimeAssets;
use super::macos::{log_files, process_alive, process_started_at, snapshots_root};
use super::macos_system::CodexRuntime;
use super::node_runtime;
use crate::error::{CodexxError, Result};
use crate::file_io::io_err;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

pub(super) fn stage_theme_snapshot(
    runtime: &CodexRuntime,
    assets: &RuntimeAssets,
    source: &Path,
    theme_id: &str,
) -> Result<PathBuf> {
    node_runtime::stage_theme_snapshot(&runtime.node, assets, &snapshots_root()?, source, theme_id)
}

pub(super) fn run_injector_once(
    runtime: &CodexRuntime,
    assets: &RuntimeAssets,
    port: u16,
    theme_dir: &Path,
) -> Result<()> {
    node_runtime::run_injector_once(&runtime.node, assets, port, theme_dir)
}

pub(super) fn remove_live_skin(
    runtime: &CodexRuntime,
    assets: &RuntimeAssets,
    port: u16,
    theme_dir: &Path,
) -> Result<()> {
    node_runtime::remove_live_skin(&runtime.node, assets, port, theme_dir)
}

pub(super) fn launch_watcher(
    runtime: &CodexRuntime,
    assets: &RuntimeAssets,
    port: u16,
    theme_dir: &Path,
) -> Result<(u32, String)> {
    let (stdout, stderr) = log_files("injector")?;
    let mut child = Command::new(&runtime.node)
        .arg(&assets.injector)
        .args(["--watch", "--port"])
        .arg(port.to_string())
        .arg("--theme-dir")
        .arg(theme_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|source| io_err(&assets.injector, source))?;
    let pid = child.id();
    thread::Builder::new()
        .name("codex-x-skin-injector-reaper".to_string())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|error| CodexxError::Config(format!("启动皮肤进程监控失败: {error}")))?;
    thread::sleep(Duration::from_millis(180));
    if !process_alive(pid) {
        return Err(CodexxError::Config(
            "皮肤注入器启动后立即退出，请检查运行日志".to_string(),
        ));
    }
    let started_at = process_started_at(pid)
        .ok_or_else(|| CodexxError::Config("无法记录皮肤注入器启动时间".to_string()))?;
    Ok((pid, started_at))
}
