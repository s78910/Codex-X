use super::assets::{
    ensure_runtime_assets, ensure_runtime_directory, RuntimeAssets, RUNTIME_VERSION,
    UPSTREAM_COMMIT,
};
use super::macos_injector::{
    launch_watcher, remove_live_skin, run_injector_once, stage_theme_snapshot,
};
use super::macos_system::{discover_codex_runtime, CodexRuntime};
use super::{SkinRuntimeAction, SkinRuntimeStatus};
use crate::error::{CodexxError, Result};
use crate::file_io::{atomic_write, io_err};
use crate::now_rfc3339;
use crate::skins::skins_root;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_CDP_PORT: u16 = 9341;
const MAX_CDP_PORT_OFFSET: u16 = 100;
const RUNTIME_STATE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStateFile {
    schema_version: u32,
    runtime_version: String,
    upstream_commit: String,
    session: String,
    port: u16,
    injector_pid: u32,
    injector_started_at: String,
    injector_path: String,
    node_path: String,
    node_version: String,
    codex_bundle: String,
    codex_executable: String,
    codex_version: String,
    codex_team_id: String,
    theme_id: String,
    theme_dir: String,
    created_at: String,
    updated_at: String,
}

fn runtime_root() -> Result<PathBuf> {
    Ok(skins_root()?.join("runtime"))
}

fn state_path() -> Result<PathBuf> {
    Ok(runtime_root()?.join("state.json"))
}

pub(super) fn snapshots_root() -> Result<PathBuf> {
    Ok(runtime_root()?.join("themes"))
}

fn logs_root() -> Result<PathBuf> {
    Ok(runtime_root()?.join("logs"))
}

fn read_runtime_state() -> Result<Option<RuntimeStateFile>> {
    let path = state_path()?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_err(&path, error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 256 * 1024 {
        return Err(CodexxError::Config(
            "皮肤运行状态文件不是安全的普通文件，已停止自动处理".to_string(),
        ));
    }
    let bytes = fs::read(&path).map_err(|source| io_err(&path, source))?;
    let state: RuntimeStateFile = serde_json::from_slice(&bytes)
        .map_err(|error| CodexxError::Config(format!("解析皮肤运行状态失败: {error}")))?;
    if state.schema_version != RUNTIME_STATE_SCHEMA || !(1024..=65535).contains(&state.port) {
        return Err(CodexxError::Config(
            "皮肤运行状态版本或端口无效，已停止自动处理".to_string(),
        ));
    }
    Ok(Some(state))
}

fn write_runtime_state(state: &RuntimeStateFile) -> Result<()> {
    let path = state_path()?;
    let text = serde_json::to_string_pretty(state)
        .map_err(|error| CodexxError::Config(format!("序列化皮肤运行状态失败: {error}")))?;
    atomic_write(&path, format!("{text}\n").as_bytes())?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(|source| io_err(&path, source))
}

fn remove_runtime_state() -> Result<()> {
    let path = state_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_err(&path, error)),
    }
}

fn process_table() -> Vec<(u32, String)> {
    let Ok(output) = Command::new("/bin/ps")
        .args(["-axo", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let split = trimmed.find(char::is_whitespace)?;
            let pid = trimmed[..split].parse::<u32>().ok()?;
            Some((pid, trimmed[split..].trim_start().to_string()))
        })
        .collect()
}

pub(super) fn process_command(pid: u32) -> Option<String> {
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    let command = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!command.is_empty()).then_some(command)
}

pub(super) fn process_started_at(pid: u32) -> Option<String> {
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!value.is_empty()).then_some(value)
}

pub(super) fn process_alive(pid: u32) -> bool {
    pid > 0
        && Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
}

fn codex_main_pids(runtime: &CodexRuntime) -> Vec<u32> {
    let prefix = runtime.executable.to_string_lossy();
    process_table()
        .into_iter()
        .filter_map(|(pid, command)| command.starts_with(prefix.as_ref()).then_some(pid))
        .collect()
}

fn codex_is_running(runtime: &CodexRuntime) -> bool {
    !codex_main_pids(runtime).is_empty()
}

fn debug_port_from_command(command: &str) -> Option<u16> {
    command.split_whitespace().find_map(|argument| {
        argument
            .strip_prefix("--remote-debugging-port=")?
            .parse::<u16>()
            .ok()
            .filter(|port| *port >= 1024)
    })
}

fn process_parent(pid: u32) -> Option<u32> {
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn pid_is_codex_descendant(runtime: &CodexRuntime, mut pid: u32) -> bool {
    let executable = runtime.executable.to_string_lossy();
    for _ in 0..32 {
        if process_command(pid).is_some_and(|command| command.starts_with(executable.as_ref())) {
            return true;
        }
        let Some(parent) = process_parent(pid) else {
            return false;
        };
        if parent <= 1 || parent == pid {
            return false;
        }
        pid = parent;
    }
    false
}

fn listener_pids(port: u16) -> Vec<u32> {
    let Ok(output) = Command::new("/usr/sbin/lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

fn port_belongs_to_codex(runtime: &CodexRuntime, port: u16) -> bool {
    let pids = listener_pids(port);
    !pids.is_empty()
        && pids
            .into_iter()
            .all(|pid| pid_is_codex_descendant(runtime, pid))
}

fn cdp_http_ready(port: u16) -> bool {
    let Ok(client) = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    client
        .get(format!("http://127.0.0.1:{port}/json/version"))
        .send()
        .is_ok_and(|response| response.status().is_success())
}

fn verified_cdp_endpoint(runtime: &CodexRuntime, port: u16) -> bool {
    port_belongs_to_codex(runtime, port) && cdp_http_ready(port)
}

fn running_cdp_port(runtime: &CodexRuntime) -> Option<u16> {
    codex_main_pids(runtime)
        .into_iter()
        .filter_map(process_command)
        .filter_map(|command| debug_port_from_command(&command))
        .find(|port| verified_cdp_endpoint(runtime, *port))
}

fn select_available_port(preferred: u16) -> Result<u16> {
    let last = preferred.saturating_add(MAX_CDP_PORT_OFFSET);
    (preferred..=last)
        .find(|port| listener_pids(*port).is_empty())
        .ok_or_else(|| {
            CodexxError::Config(format!("未找到可用的本机 CDP 端口: {preferred}-{last}"))
        })
}

fn wait_for_cdp(runtime: &CodexRuntime, port: u16, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if verified_cdp_endpoint(runtime, port) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(350));
    }
    Err(CodexxError::Config(format!(
        "Codex 未在 127.0.0.1:{port} 打开经过验证的调试端口"
    )))
}

fn external_skin_watcher_pid(own_state: Option<&RuntimeStateFile>) -> Option<u32> {
    process_table().into_iter().find_map(|(pid, command)| {
        let is_external = command.contains("codex-dream-skin-studio")
            && command.contains("injector.mjs")
            && command.contains("--watch");
        (is_external && own_state.is_none_or(|state| state.injector_pid != pid)).then_some(pid)
    })
}

fn canonical_managed_snapshot(path: &Path) -> Option<PathBuf> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let root = fs::canonicalize(snapshots_root().ok()?).ok()?;
    let candidate = fs::canonicalize(path).ok()?;
    candidate.starts_with(&root).then_some(candidate)
}

fn is_managed_injector(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return false;
    }
    let Ok(root) = runtime_root()
        .and_then(|path| fs::canonicalize(&path).map_err(|source| io_err(&path, source)))
    else {
        return false;
    };
    fs::canonicalize(path).is_ok_and(|candidate| {
        candidate.starts_with(root)
            && candidate.file_name().and_then(|name| name.to_str()) == Some("injector.mjs")
    })
}

fn state_process_matches(state: &RuntimeStateFile, runtime: &CodexRuntime) -> bool {
    if state.injector_pid == 0
        || state.node_path != runtime.node.to_string_lossy()
        || !is_managed_injector(Path::new(&state.injector_path))
    {
        return false;
    }
    if canonical_managed_snapshot(Path::new(&state.theme_dir)).is_none() {
        return false;
    }
    let Some(command) = process_command(state.injector_pid) else {
        return false;
    };
    let expected = format!(
        "{} --watch --port {} --theme-dir {}",
        state.injector_path, state.port, state.theme_dir
    );
    command.starts_with(runtime.node.to_string_lossy().as_ref())
        && command.contains(&expected)
        && process_started_at(state.injector_pid).as_deref()
            == Some(state.injector_started_at.as_str())
}

fn stop_recorded_injector(state: &RuntimeStateFile, runtime: &CodexRuntime) -> Result<()> {
    if state.injector_pid == 0 || !process_alive(state.injector_pid) {
        return Ok(());
    }
    if !state_process_matches(state, runtime) {
        return Err(CodexxError::Config(format!(
            "记录的皮肤进程 PID {} 身份不匹配，已拒绝终止",
            state.injector_pid
        )));
    }
    let pid = state.injector_pid.to_string();
    let _ = Command::new("/bin/kill").args(["-TERM", &pid]).status();
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline && state_process_matches(state, runtime) {
        thread::sleep(Duration::from_millis(150));
    }
    if state_process_matches(state, runtime) {
        let _ = Command::new("/bin/kill").args(["-KILL", &pid]).status();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && state_process_matches(state, runtime) {
        thread::sleep(Duration::from_millis(100));
    }
    if state_process_matches(state, runtime) {
        return Err(CodexxError::Config(format!(
            "无法停止皮肤注入器 PID {}",
            state.injector_pid
        )));
    }
    Ok(())
}

fn stop_codex(runtime: &CodexRuntime) -> Result<()> {
    let _ = Command::new("/usr/bin/osascript")
        .args(["-e", "tell application id \"com.openai.codex\" to quit"])
        .status();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && codex_is_running(runtime) {
        thread::sleep(Duration::from_millis(250));
    }
    if !codex_is_running(runtime) {
        return Ok(());
    }
    for pid in codex_main_pids(runtime) {
        let _ = Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && codex_is_running(runtime) {
        thread::sleep(Duration::from_millis(250));
    }
    if codex_is_running(runtime) {
        for pid in codex_main_pids(runtime) {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
        thread::sleep(Duration::from_millis(500));
    }
    if codex_is_running(runtime) {
        Err(CodexxError::Config(
            "无法安全关闭 Codex，换肤操作已中止".to_string(),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn log_files(prefix: &str) -> Result<(File, File)> {
    use std::os::unix::fs::PermissionsExt;

    let root = logs_root()?;
    ensure_runtime_directory(&root)?;
    let stdout_path = root.join(format!("{prefix}.log"));
    let stderr_path = root.join(format!("{prefix}-error.log"));
    let stdout = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&stdout_path)
        .map_err(|source| io_err(&stdout_path, source))?;
    let stderr = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&stderr_path)
        .map_err(|source| io_err(&stderr_path, source))?;
    fs::set_permissions(&stdout_path, fs::Permissions::from_mode(0o600))
        .map_err(|source| io_err(&stdout_path, source))?;
    fs::set_permissions(&stderr_path, fs::Permissions::from_mode(0o600))
        .map_err(|source| io_err(&stderr_path, source))?;
    Ok((stdout, stderr))
}

fn launch_codex_with_cdp(runtime: &CodexRuntime, port: u16) -> Result<()> {
    let (stdout, stderr) = log_files("codex-launch")?;
    let status = Command::new("/usr/bin/open")
        .arg("-na")
        .arg(&runtime.bundle)
        .arg("--args")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--remote-debugging-port={port}"))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .map_err(|source| io_err(&runtime.bundle, source))?;
    if !status.success() {
        return Err(CodexxError::Config(
            "使用本机调试端口启动 Codex 失败".to_string(),
        ));
    }
    if wait_for_cdp(runtime, port, Duration::from_secs(12)).is_ok() {
        return Ok(());
    }

    if codex_is_running(runtime) {
        stop_codex(runtime)?;
    }
    let (stdout, stderr) = log_files("codex-launch-direct")?;
    let mut child = Command::new(&runtime.executable)
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--remote-debugging-port={port}"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|source| io_err(&runtime.executable, source))?;
    thread::Builder::new()
        .name("codex-x-codex-launch-reaper".to_string())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|error| CodexxError::Config(format!("启动 Codex 进程监控失败: {error}")))?;
    wait_for_cdp(runtime, port, Duration::from_secs(45))
}

fn launch_codex_normally(runtime: &CodexRuntime) -> Result<()> {
    let status = Command::new("/usr/bin/open")
        .arg("-na")
        .arg(&runtime.bundle)
        .status()
        .map_err(|source| io_err(&runtime.bundle, source))?;
    if status.success() {
        Ok(())
    } else {
        Err(CodexxError::Config(
            "恢复官方外观后重新启动 Codex 失败".to_string(),
        ))
    }
}

fn active_state(
    runtime: &CodexRuntime,
    assets: &RuntimeAssets,
    port: u16,
    theme_id: &str,
    theme_dir: &Path,
    pid: u32,
    started_at: String,
) -> RuntimeStateFile {
    let timestamp = now_rfc3339();
    RuntimeStateFile {
        schema_version: RUNTIME_STATE_SCHEMA,
        runtime_version: RUNTIME_VERSION.to_string(),
        upstream_commit: UPSTREAM_COMMIT.to_string(),
        session: "active".to_string(),
        port,
        injector_pid: pid,
        injector_started_at: started_at,
        injector_path: assets.injector.to_string_lossy().to_string(),
        node_path: runtime.node.to_string_lossy().to_string(),
        node_version: runtime.node_version.clone(),
        codex_bundle: runtime.bundle.to_string_lossy().to_string(),
        codex_executable: runtime.executable.to_string_lossy().to_string(),
        codex_version: runtime.version.clone(),
        codex_team_id: runtime.team_id.clone(),
        theme_id: theme_id.to_string(),
        theme_dir: theme_dir.to_string_lossy().to_string(),
        created_at: timestamp.clone(),
        updated_at: timestamp,
    }
}

fn cleanup_snapshot(path: &str, keep: &Path) {
    let Some(path) = canonical_managed_snapshot(Path::new(path)) else {
        return;
    };
    let keep = canonical_managed_snapshot(keep);
    if keep.as_ref() != Some(&path) {
        let _ = fs::remove_dir_all(&path);
    }
}

fn restore_previous_after_failure(
    runtime: &CodexRuntime,
    assets: &RuntimeAssets,
    previous: Option<&RuntimeStateFile>,
) {
    let Some(previous) = previous else {
        return;
    };
    let theme_dir = Path::new(&previous.theme_dir);
    if previous.session != "active"
        || !theme_dir.is_dir()
        || !verified_cdp_endpoint(runtime, previous.port)
        || run_injector_once(runtime, assets, previous.port, theme_dir).is_err()
    {
        return;
    }
    if let Ok((pid, started_at)) = launch_watcher(runtime, assets, previous.port, theme_dir) {
        let restored = active_state(
            runtime,
            assets,
            previous.port,
            &previous.theme_id,
            theme_dir,
            pid,
            started_at,
        );
        let _ = write_runtime_state(&restored);
    }
}

pub(super) fn apply_theme(
    source: &Path,
    theme_id: &str,
    restart_existing: bool,
) -> Result<SkinRuntimeAction> {
    let runtime = discover_codex_runtime()?;
    let assets = ensure_runtime_assets()?;
    let previous = read_runtime_state()?;
    if let Some(pid) = external_skin_watcher_pid(previous.as_ref()) {
        return Err(CodexxError::Config(format!(
            "检测到外部 Codex Dream Skin 注入器 PID {pid}，请先在原工具中暂停或恢复官方外观"
        )));
    }

    let running = codex_is_running(&runtime);
    let verified_port = previous
        .as_ref()
        .map(|state| state.port)
        .filter(|port| verified_cdp_endpoint(&runtime, *port))
        .or_else(|| running_cdp_port(&runtime));
    if running && verified_port.is_none() && !restart_existing {
        return Ok(SkinRuntimeAction::RestartRequired(
            "Codex 需要重启一次才能打开仅限本机的换肤调试端口".to_string(),
        ));
    }

    let snapshot = stage_theme_snapshot(&runtime, &assets, source, theme_id)?;
    let apply_result = (|| -> Result<RuntimeStateFile> {
        if let Some(state) = previous.as_ref() {
            stop_recorded_injector(state, &runtime)?;
        }
        let port = if let Some(port) = verified_port {
            port
        } else {
            if running {
                stop_codex(&runtime)?;
            }
            let preferred = previous
                .as_ref()
                .map_or(DEFAULT_CDP_PORT, |state| state.port);
            let port = select_available_port(preferred)?;
            launch_codex_with_cdp(&runtime, port)?;
            port
        };
        run_injector_once(&runtime, &assets, port, &snapshot)?;
        let (pid, started_at) = launch_watcher(&runtime, &assets, port, &snapshot)?;
        let state = active_state(
            &runtime, &assets, port, theme_id, &snapshot, pid, started_at,
        );
        if let Err(error) = write_runtime_state(&state) {
            let _ = stop_recorded_injector(&state, &runtime);
            return Err(error);
        }
        Ok(state)
    })();

    match apply_result {
        Ok(_) => {
            if let Some(previous) = previous.as_ref() {
                cleanup_snapshot(&previous.theme_dir, &snapshot);
            }
            Ok(SkinRuntimeAction::Applied("已应用 Codex 皮肤".to_string()))
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&snapshot);
            restore_previous_after_failure(&runtime, &assets, previous.as_ref());
            Err(error)
        }
    }
}

pub(super) fn pause_theme() -> Result<SkinRuntimeAction> {
    let assets = ensure_runtime_assets()?;
    let Some(mut state) = read_runtime_state()? else {
        return Ok(SkinRuntimeAction::Paused(
            "Codex 皮肤当前没有运行".to_string(),
        ));
    };
    let runtime = discover_codex_runtime()?;
    if let Some(pid) = external_skin_watcher_pid(Some(&state)) {
        return Err(CodexxError::Config(format!(
            "检测到外部 Codex Dream Skin 注入器 PID {pid}，未处理该进程"
        )));
    }
    stop_recorded_injector(&state, &runtime)?;
    if verified_cdp_endpoint(&runtime, state.port) {
        if let Err(error) =
            remove_live_skin(&runtime, &assets, state.port, Path::new(&state.theme_dir))
        {
            restore_previous_after_failure(&runtime, &assets, Some(&state));
            return Err(error);
        }
    }
    state.session = "paused".to_string();
    state.injector_pid = 0;
    state.injector_started_at.clear();
    state.updated_at = now_rfc3339();
    write_runtime_state(&state)?;
    Ok(SkinRuntimeAction::Paused(
        "已关闭 Codex 皮肤；Codex 保持打开".to_string(),
    ))
}

pub(super) fn restore_official(restart_existing: bool) -> Result<SkinRuntimeAction> {
    let assets = ensure_runtime_assets()?;
    let Some(state) = read_runtime_state()? else {
        return Ok(SkinRuntimeAction::Restored(
            "Codex 当前已使用官方外观".to_string(),
        ));
    };
    let runtime = discover_codex_runtime()?;
    if let Some(pid) = external_skin_watcher_pid(Some(&state)) {
        return Err(CodexxError::Config(format!(
            "检测到外部 Codex Dream Skin 注入器 PID {pid}，未处理该进程"
        )));
    }
    let running = codex_is_running(&runtime);
    let debug_ready = verified_cdp_endpoint(&runtime, state.port);
    if running && (debug_ready || state.session == "active") && !restart_existing {
        return Ok(SkinRuntimeAction::RestartRequired(
            "完整恢复需要重启 Codex，以关闭本机调试端口并清除当前渲染状态".to_string(),
        ));
    }
    stop_recorded_injector(&state, &runtime)?;
    if debug_ready {
        remove_live_skin(&runtime, &assets, state.port, Path::new(&state.theme_dir))?;
    }
    if running && (debug_ready || state.session == "active") {
        stop_codex(&runtime)?;
        launch_codex_normally(&runtime)?;
    }
    remove_runtime_state()?;
    cleanup_snapshot(&state.theme_dir, Path::new(""));
    Ok(SkinRuntimeAction::Restored(
        "已恢复 Codex 官方外观并关闭 Codex-X 管理的换肤运行时".to_string(),
    ))
}

pub(super) fn runtime_status() -> SkinRuntimeStatus {
    if let Err(error) = ensure_runtime_assets() {
        return SkinRuntimeStatus {
            supported: true,
            active: false,
            phase: "error".to_string(),
            port: None,
            theme_id: None,
            message: error.to_string(),
        };
    }
    let state = match read_runtime_state() {
        Ok(Some(state)) => state,
        Ok(None) => {
            return SkinRuntimeStatus {
                supported: true,
                active: false,
                phase: "inactive".to_string(),
                port: None,
                theme_id: None,
                message: "尚未应用 Codex 皮肤".to_string(),
            }
        }
        Err(error) => {
            return SkinRuntimeStatus {
                supported: true,
                active: false,
                phase: "error".to_string(),
                port: None,
                theme_id: None,
                message: error.to_string(),
            }
        }
    };
    if state.session == "paused" {
        return SkinRuntimeStatus {
            supported: true,
            active: false,
            phase: "paused".to_string(),
            port: Some(state.port),
            theme_id: Some(state.theme_id),
            message: "皮肤已暂停，Codex 保持打开".to_string(),
        };
    }
    let runtime = match discover_codex_runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            return SkinRuntimeStatus {
                supported: true,
                active: false,
                phase: "unavailable".to_string(),
                port: Some(state.port),
                theme_id: Some(state.theme_id),
                message: error.to_string(),
            }
        }
    };
    let active =
        state_process_matches(&state, &runtime) && verified_cdp_endpoint(&runtime, state.port);
    SkinRuntimeStatus {
        supported: true,
        active,
        phase: if active { "active" } else { "stale" }.to_string(),
        port: Some(state.port),
        theme_id: Some(state.theme_id),
        message: if active {
            "Codex 皮肤运行中".to_string()
        } else {
            "皮肤运行状态已失效，可重新应用或恢复官方外观".to_string()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_port_parser_accepts_only_bounded_numeric_arguments() {
        assert_eq!(
            debug_port_from_command(
                "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT --remote-debugging-port=9341"
            ),
            Some(9341)
        );
        assert_eq!(
            debug_port_from_command("Codex --remote-debugging-port=80"),
            None
        );
        assert_eq!(
            debug_port_from_command("Codex --remote-debugging-port=not-a-port"),
            None
        );
        assert_eq!(debug_port_from_command("Codex"), None);
    }
}
