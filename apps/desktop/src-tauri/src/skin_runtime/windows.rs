use super::assets::{ensure_runtime_assets, ensure_runtime_directory, RuntimeAssets};
use super::node_runtime;
use super::{SkinRuntimeAction, SkinRuntimeStatus};
use crate::error::{CodexxError, Result};
use crate::file_io::{atomic_write, io_err};
use crate::now_rfc3339;
use crate::skins::skins_root;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DEFAULT_CDP_PORT: u16 = 9341;
const RUNTIME_STATE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStateFile {
    schema_version: u32,
    platform: String,
    session: String,
    port: u16,
    browser_id: String,
    injector_pid: u32,
    injector_started_at: String,
    injector_path: String,
    node_path: String,
    node_version: String,
    codex_package_root: String,
    codex_executable: String,
    codex_version: String,
    codex_package_full_name: String,
    codex_package_family_name: String,
    codex_app_user_model_id: String,
    theme_id: String,
    theme_dir: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexRuntime {
    package_root: PathBuf,
    executable: PathBuf,
    version: String,
    package_full_name: String,
    package_family_name: String,
    app_user_model_id: String,
    running: bool,
    debug_port: Option<u16>,
    browser_id: Option<String>,
    watcher_pids: Vec<u32>,
}

#[derive(Debug, Deserialize)]
struct PortResult {
    port: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchResult {
    port: u16,
    browser_id: String,
    strategy: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerifyPortResult {
    verified: bool,
    browser_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcessInfo {
    alive: bool,
    path: Option<String>,
    command_line: Option<String>,
    started_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ActiveResult {
    active: bool,
}

#[derive(Debug, Clone)]
struct NodeRuntime {
    path: PathBuf,
    version: String,
}

struct ActiveStateContext<'a> {
    runtime: &'a CodexRuntime,
    node: &'a NodeRuntime,
    assets: &'a RuntimeAssets,
    port: u16,
    browser_id: String,
    theme_id: &'a str,
    theme_dir: &'a Path,
    injector_pid: u32,
    injector_started_at: String,
}

fn runtime_root() -> Result<PathBuf> {
    Ok(skins_root()?.join("runtime"))
}

fn state_path() -> Result<PathBuf> {
    Ok(runtime_root()?.join("state.json"))
}

fn snapshots_root() -> Result<PathBuf> {
    Ok(runtime_root()?.join("themes"))
}

fn logs_root() -> Result<PathBuf> {
    Ok(runtime_root()?.join("logs"))
}

fn powershell_path() -> PathBuf {
    env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| {
            root.join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        })
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("powershell.exe"))
}

fn configure_background_command(command: &mut Command) {
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(target_os = "windows"))]
    let _ = command;
}

fn parse_last_json<T: DeserializeOwned>(stdout: &[u8], label: &str) -> Result<T> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find_map(|line| serde_json::from_str(line).ok())
        .ok_or_else(|| CodexxError::Config(format!("{label}没有返回有效结果")))
}

fn run_adapter<T: DeserializeOwned>(
    assets: &RuntimeAssets,
    action: &str,
    args: &[String],
    timeout: Duration,
) -> Result<T> {
    let powershell = powershell_path();
    let mut command = Command::new(&powershell);
    command
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(&assets.windows_adapter)
        .args(["-Action", action])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_background_command(&mut command);
    let child = command
        .spawn()
        .map_err(|source| io_err(&powershell, source))?;
    let output = node_runtime::wait_for_output(child, &powershell, action, timeout)?;
    if !output.status.success() {
        let detail = node_runtime::command_detail(&output);
        return Err(CodexxError::Config(if detail.is_empty() {
            format!("Windows 皮肤操作失败: {action}")
        } else {
            format!("Windows 皮肤操作失败: {detail}")
        }));
    }
    parse_last_json(&output.stdout, action)
}

fn inspect_codex(assets: &RuntimeAssets) -> Result<CodexRuntime> {
    run_adapter(assets, "inspect", &[], Duration::from_secs(15))
}

fn select_port(assets: &RuntimeAssets, preferred: u16) -> Result<u16> {
    let result: PortResult = run_adapter(
        assets,
        "selectPort",
        &["-Port".to_string(), preferred.to_string()],
        Duration::from_secs(10),
    )?;
    Ok(result.port)
}

fn verify_port(assets: &RuntimeAssets, port: u16) -> Result<VerifyPortResult> {
    run_adapter(
        assets,
        "verifyPort",
        &["-Port".to_string(), port.to_string()],
        Duration::from_secs(10),
    )
}

fn stop_codex(assets: &RuntimeAssets) -> Result<()> {
    let _: serde_json::Value = run_adapter(assets, "stopCodex", &[], Duration::from_secs(25))?;
    Ok(())
}

fn launch_codex_normally(assets: &RuntimeAssets) -> Result<()> {
    let _: serde_json::Value = run_adapter(assets, "launchNormal", &[], Duration::from_secs(15))?;
    Ok(())
}

fn launch_codex_with_cdp(assets: &RuntimeAssets, port: u16) -> Result<LaunchResult> {
    let result: LaunchResult = run_adapter(
        assets,
        "launch",
        &["-Port".to_string(), port.to_string()],
        Duration::from_secs(60),
    )?;
    if result.port != port || result.browser_id.is_empty() {
        return Err(CodexxError::Config(
            "Windows Codex 启动器返回了无效的调试端口身份".to_string(),
        ));
    }
    Ok(result)
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
    if state.schema_version != RUNTIME_STATE_SCHEMA
        || state.platform != "windows"
        || !(1024..=65535).contains(&state.port)
    {
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
    atomic_write(&path, format!("{text}\n").as_bytes())
}

fn remove_runtime_state() -> Result<()> {
    let path = state_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_err(&path, error)),
    }
}

fn node_version(path: &Path) -> Result<String> {
    let mut command = Command::new(path);
    command.arg("--version");
    configure_background_command(&mut command);
    let output = command.output().map_err(|source| io_err(path, source))?;
    if !output.status.success() {
        return Err(CodexxError::Config(format!(
            "无法运行 Windows 皮肤 Node.js: {}",
            path.display()
        )));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let major = version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| CodexxError::Config(format!("无法解析 Node.js 版本: {version}")))?;
    if major < 22 {
        return Err(CodexxError::Config(format!(
            "Windows 皮肤运行时需要 Node.js 22 或更高版本，当前为 {version}"
        )));
    }
    Ok(version)
}

fn resolve_node_candidate(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = fs::canonicalize(path) {
        return Some(canonical);
    }
    if path.components().count() != 1 {
        return None;
    }
    let mut command = Command::new("where.exe");
    command.arg(path);
    configure_background_command(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .find_map(|candidate| fs::canonicalize(candidate).ok())
}

fn discover_node_runtime() -> Result<NodeRuntime> {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("CODEX_X_SKIN_NODE") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(executable) = env::current_exe() {
        if let Some(parent) = executable.parent() {
            candidates.push(parent.join("skin-runtime").join("node").join("node.exe"));
        }
    }
    candidates.push(PathBuf::from("node.exe"));
    for candidate in candidates {
        if let Some(path) = resolve_node_candidate(&candidate) {
            if let Ok(version) = node_version(&path) {
                return Ok(NodeRuntime { path, version });
            }
        }
    }
    Err(CodexxError::Config(
        "Windows 皮肤运行时缺少内置 Node.js；请重新安装 Codex-X".to_string(),
    ))
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

fn cleanup_snapshot(path: &str, keep: &Path) {
    let Some(path) = canonical_managed_snapshot(Path::new(path)) else {
        return;
    };
    let keep = canonical_managed_snapshot(keep);
    if keep.as_ref() != Some(&path) {
        let _ = fs::remove_dir_all(path);
    }
}

fn process_info(assets: &RuntimeAssets, pid: u32) -> Result<ProcessInfo> {
    run_adapter(
        assets,
        "processInfo",
        &["-TargetPid".to_string(), pid.to_string()],
        Duration::from_secs(10),
    )
}

fn injector_active(assets: &RuntimeAssets) -> Result<bool> {
    let result: ActiveResult = run_adapter(
        assets,
        "injectorStatus",
        &[
            "-StatePath".to_string(),
            state_path()?.to_string_lossy().to_string(),
        ],
        Duration::from_secs(10),
    )?;
    Ok(result.active)
}

fn write_adapter_state(state: &RuntimeStateFile) -> Result<PathBuf> {
    let root = runtime_root()?;
    ensure_runtime_directory(&root)?;
    let path = root.join(format!(
        ".process-state-{}-{}.json",
        std::process::id(),
        state.injector_pid
    ));
    let text = serde_json::to_string(state)
        .map_err(|error| CodexxError::Config(format!("序列化皮肤进程状态失败: {error}")))?;
    atomic_write(&path, text.as_bytes())?;
    Ok(path)
}

fn stop_recorded_injector(assets: &RuntimeAssets, state: &RuntimeStateFile) -> Result<()> {
    if state.injector_pid == 0 {
        return Ok(());
    }
    let info = process_info(assets, state.injector_pid)?;
    if !info.alive {
        return Ok(());
    }
    let adapter_state = write_adapter_state(state)?;
    let result = run_adapter::<serde_json::Value>(
        assets,
        "stopInjector",
        &[
            "-StatePath".to_string(),
            adapter_state.to_string_lossy().to_string(),
        ],
        Duration::from_secs(15),
    );
    let _ = fs::remove_file(&adapter_state);
    result.map(|_| ())
}

fn log_files(prefix: &str) -> Result<(File, File)> {
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
    Ok((stdout, stderr))
}

fn launch_watcher(
    node: &NodeRuntime,
    assets: &RuntimeAssets,
    port: u16,
    theme_dir: &Path,
) -> Result<(u32, String)> {
    let (stdout, stderr) = log_files("injector")?;
    let mut command = Command::new(&node.path);
    command
        .arg(&assets.injector)
        .args(["--watch", "--port"])
        .arg(port.to_string())
        .arg("--theme-dir")
        .arg(theme_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    configure_background_command(&mut command);
    let mut child = command
        .spawn()
        .map_err(|source| io_err(&assets.injector, source))?;
    let pid = child.id();
    thread::sleep(Duration::from_millis(250));
    if child
        .try_wait()
        .map_err(|source| io_err(&assets.injector, source))?
        .is_some()
    {
        return Err(CodexxError::Config(
            "皮肤注入器启动后立即退出，请检查运行日志".to_string(),
        ));
    }
    let info = process_info(assets, pid)?;
    let started_at = info
        .started_at
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CodexxError::Config("无法记录皮肤注入器启动时间".to_string()))?;
    let expected_node = node.path.to_string_lossy();
    let path_matches = info.path.as_deref().is_some_and(|path| {
        Path::new(path)
            .canonicalize()
            .is_ok_and(|actual| actual == node.path)
    });
    let command_matches = info.command_line.as_deref().is_some_and(|command| {
        command.contains(assets.injector.to_string_lossy().as_ref())
            && command.contains("--watch")
            && command.contains(&format!("--port {port}"))
    });
    if !info.alive
        || (!path_matches
            && !expected_node.eq_ignore_ascii_case(info.path.as_deref().unwrap_or("")))
        || !command_matches
    {
        let _ = child.kill();
        return Err(CodexxError::Config(
            "Windows 皮肤注入器进程身份校验失败".to_string(),
        ));
    }
    drop(child);
    Ok((pid, started_at))
}

fn active_state(context: ActiveStateContext<'_>) -> RuntimeStateFile {
    let timestamp = now_rfc3339();
    RuntimeStateFile {
        schema_version: RUNTIME_STATE_SCHEMA,
        platform: "windows".to_string(),
        session: "active".to_string(),
        port: context.port,
        browser_id: context.browser_id,
        injector_pid: context.injector_pid,
        injector_started_at: context.injector_started_at,
        injector_path: context.assets.injector.to_string_lossy().to_string(),
        node_path: context.node.path.to_string_lossy().to_string(),
        node_version: context.node.version.clone(),
        codex_package_root: context.runtime.package_root.to_string_lossy().to_string(),
        codex_executable: context.runtime.executable.to_string_lossy().to_string(),
        codex_version: context.runtime.version.clone(),
        codex_package_full_name: context.runtime.package_full_name.clone(),
        codex_package_family_name: context.runtime.package_family_name.clone(),
        codex_app_user_model_id: context.runtime.app_user_model_id.clone(),
        theme_id: context.theme_id.to_string(),
        theme_dir: context.theme_dir.to_string_lossy().to_string(),
        created_at: timestamp.clone(),
        updated_at: timestamp,
    }
}

fn restore_previous_after_failure(
    runtime: &CodexRuntime,
    node: &NodeRuntime,
    assets: &RuntimeAssets,
    previous: Option<&RuntimeStateFile>,
) {
    let Some(previous) = previous else { return };
    let theme_dir = Path::new(&previous.theme_dir);
    let verified = verify_port(assets, previous.port).is_ok_and(|result| result.verified);
    if previous.session != "active"
        || !theme_dir.is_dir()
        || !verified
        || node_runtime::run_injector_once(&node.path, assets, previous.port, theme_dir).is_err()
    {
        return;
    }
    if let Ok((pid, started_at)) = launch_watcher(node, assets, previous.port, theme_dir) {
        let restored = active_state(ActiveStateContext {
            runtime,
            node,
            assets,
            port: previous.port,
            browser_id: previous.browser_id.clone(),
            theme_id: &previous.theme_id,
            theme_dir,
            injector_pid: pid,
            injector_started_at: started_at,
        });
        let _ = write_runtime_state(&restored);
    }
}

pub(super) fn apply_theme(
    source: &Path,
    theme_id: &str,
    restart_existing: bool,
) -> Result<SkinRuntimeAction> {
    let assets = ensure_runtime_assets()?;
    let runtime = inspect_codex(&assets)?;
    let node = discover_node_runtime()?;
    let previous = read_runtime_state()?;
    if let Some(pid) = runtime.watcher_pids.iter().copied().find(|pid| {
        previous
            .as_ref()
            .is_none_or(|state| state.injector_pid != *pid)
    }) {
        return Err(CodexxError::Config(format!(
            "检测到外部 Codex 换肤注入器 PID {pid}，请先在原工具中关闭皮肤"
        )));
    }
    if runtime.running && runtime.debug_port.is_none() && !restart_existing {
        return Ok(SkinRuntimeAction::RestartRequired(
            "Codex 需要重启一次才能打开仅限本机的换肤调试端口".to_string(),
        ));
    }
    let snapshot = node_runtime::stage_theme_snapshot(
        &node.path,
        &assets,
        &snapshots_root()?,
        source,
        theme_id,
    )?;
    let mut launched_new = false;
    let apply_result = (|| -> Result<RuntimeStateFile> {
        if let Some(state) = previous.as_ref() {
            stop_recorded_injector(&assets, state)?;
        }
        let (port, browser_id) = if let Some(port) = runtime.debug_port {
            let browser_id = runtime.browser_id.clone().ok_or_else(|| {
                CodexxError::Config("无法验证当前 Windows Codex 调试会话".to_string())
            })?;
            (port, browser_id)
        } else {
            if runtime.running {
                stop_codex(&assets)?;
            }
            let preferred = previous
                .as_ref()
                .map_or(DEFAULT_CDP_PORT, |state| state.port);
            let port = select_port(&assets, preferred)?;
            let launch = launch_codex_with_cdp(&assets, port)?;
            launched_new = true;
            let _strategy = launch.strategy;
            (port, launch.browser_id)
        };
        node_runtime::run_injector_once(&node.path, &assets, port, &snapshot)?;
        let (pid, started_at) = launch_watcher(&node, &assets, port, &snapshot)?;
        let state = active_state(ActiveStateContext {
            runtime: &runtime,
            node: &node,
            assets: &assets,
            port,
            browser_id,
            theme_id,
            theme_dir: &snapshot,
            injector_pid: pid,
            injector_started_at: started_at,
        });
        if let Err(error) = write_runtime_state(&state) {
            let _ = stop_recorded_injector(&assets, &state);
            return Err(error);
        }
        Ok(state)
    })();
    match apply_result {
        Ok(_) => {
            if let Some(previous) = previous.as_ref() {
                cleanup_snapshot(&previous.theme_dir, &snapshot);
            }
            Ok(SkinRuntimeAction::Applied(
                "已应用 Windows Codex 皮肤".to_string(),
            ))
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&snapshot);
            restore_previous_after_failure(&runtime, &node, &assets, previous.as_ref());
            if launched_new && previous.is_none() {
                let _ = stop_codex(&assets);
                let _ = launch_codex_normally(&assets);
            }
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
    let node = discover_node_runtime()?;
    stop_recorded_injector(&assets, &state)?;
    if verify_port(&assets, state.port).is_ok_and(|result| result.verified) {
        node_runtime::remove_live_skin(
            &node.path,
            &assets,
            state.port,
            Path::new(&state.theme_dir),
        )?;
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
    let node = discover_node_runtime()?;
    let runtime = inspect_codex(&assets)?;
    let debug_ready = verify_port(&assets, state.port).is_ok_and(|result| result.verified);
    if runtime.running && (debug_ready || state.session == "active") && !restart_existing {
        return Ok(SkinRuntimeAction::RestartRequired(
            "完整恢复需要重启 Codex，以关闭本机调试端口并清除当前渲染状态".to_string(),
        ));
    }
    stop_recorded_injector(&assets, &state)?;
    if debug_ready {
        node_runtime::remove_live_skin(
            &node.path,
            &assets,
            state.port,
            Path::new(&state.theme_dir),
        )?;
    }
    if runtime.running && (debug_ready || state.session == "active") {
        stop_codex(&assets)?;
        launch_codex_normally(&assets)?;
    }
    remove_runtime_state()?;
    cleanup_snapshot(&state.theme_dir, Path::new(""));
    Ok(SkinRuntimeAction::Restored(
        "已恢复 Windows Codex 官方外观并关闭换肤运行时".to_string(),
    ))
}

pub(super) fn runtime_status() -> SkinRuntimeStatus {
    let assets = match ensure_runtime_assets() {
        Ok(assets) => assets,
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
            message: "皮肤已关闭，Codex 保持打开".to_string(),
        };
    }
    if let Err(error) = inspect_codex(&assets) {
        return SkinRuntimeStatus {
            supported: true,
            active: false,
            phase: "unavailable".to_string(),
            port: Some(state.port),
            theme_id: Some(state.theme_id),
            message: error.to_string(),
        };
    }
    let active = injector_active(&assets).unwrap_or(false)
        && verify_port(&assets, state.port).is_ok_and(|result| {
            result.verified && result.browser_id.as_deref() == Some(state.browser_id.as_str())
        });
    SkinRuntimeStatus {
        supported: true,
        active,
        phase: if active { "active" } else { "stale" }.to_string(),
        port: Some(state.port),
        theme_id: Some(state.theme_id),
        message: if active {
            "Windows Codex 皮肤运行中".to_string()
        } else {
            "皮肤运行状态已失效，可重新应用主题".to_string()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> RuntimeStateFile {
        RuntimeStateFile {
            schema_version: 1,
            platform: "windows".to_string(),
            session: "active".to_string(),
            port: 9341,
            browser_id: "browser-1".to_string(),
            injector_pid: 42,
            injector_started_at: "2026-07-24T00:00:00.0000000Z".to_string(),
            injector_path: r"C:\Users\test\.codexx\runtime\injector.mjs".to_string(),
            node_path: r"C:\Program Files\Codex-X\skin-runtime\node\node.exe".to_string(),
            node_version: "v22.23.1".to_string(),
            codex_package_root: r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0.0_x64"
                .to_string(),
            codex_executable:
                r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0.0_x64\app\ChatGPT.exe".to_string(),
            codex_version: "1.0.0.0".to_string(),
            codex_package_full_name: "OpenAI.Codex_1.0.0.0_x64__test".to_string(),
            codex_package_family_name: "OpenAI.Codex_test".to_string(),
            codex_app_user_model_id: "OpenAI.Codex_test!App".to_string(),
            theme_id: "theme".to_string(),
            theme_dir: r"C:\Users\test\.codexx\runtime\themes\theme".to_string(),
            created_at: "2026-07-24T00:00:00Z".to_string(),
            updated_at: "2026-07-24T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn parses_last_json_line_after_powershell_warnings() {
        let result: PortResult = parse_last_json(
            b"WARNING: package activation fallback\r\n{\"port\":9342}\r\n",
            "test",
        )
        .expect("parse adapter JSON");
        assert_eq!(result.port, 9342);
    }

    #[test]
    fn process_state_uses_powershell_contract_field_names() {
        let value = serde_json::to_value(test_state()).expect("serialize state");
        assert_eq!(value["platform"], "windows");
        assert_eq!(value["injectorPid"], 42);
        assert_eq!(
            value["injectorPath"],
            r"C:\Users\test\.codexx\runtime\injector.mjs"
        );
        assert_eq!(
            value["nodePath"],
            r"C:\Program Files\Codex-X\skin-runtime\node\node.exe"
        );
        assert_eq!(value["browserId"], "browser-1");
    }

    #[test]
    fn adapter_keeps_store_identity_and_loopback_guards() {
        let source = include_str!("../../resources/skin-runtime/windows/codexx-windows.ps1");
        assert!(source.contains("Get-DreamSkinCodexInstall"));
        assert!(source.contains("Start-DreamSkinCodexForDebugging"));
        assert!(source.contains("--remote-debugging-address=127.0.0.1"));
        assert!(source.contains("Get-DreamSkinVerifiedCdpIdentity"));
        assert!(!source.contains("takeown"));
    }
}
