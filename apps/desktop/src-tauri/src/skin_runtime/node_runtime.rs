use super::assets::{ensure_runtime_directory, RuntimeAssets};
use crate::error::{CodexxError, Result};
use crate::file_io::io_err;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(25);
const THEME_STAGE_TIMEOUT: Duration = Duration::from_secs(15);
const PAYLOAD_CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const INJECTOR_ONCE_TIMEOUT: Duration = Duration::from_secs(35);
const INJECTOR_REMOVE_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) fn command_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    if detail.chars().count() <= 800 {
        detail
    } else {
        detail
            .chars()
            .rev()
            .take(800)
            .collect::<String>()
            .chars()
            .rev()
            .collect()
    }
}

pub(super) fn wait_for_output(
    mut child: std::process::Child,
    program: &Path,
    label: &str,
    timeout: Duration,
) -> Result<std::process::Output> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().map_err(|source| io_err(program, source))? {
            Some(_) => {
                return child
                    .wait_with_output()
                    .map_err(|source| io_err(program, source))
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let output = child
                    .wait_with_output()
                    .map_err(|source| io_err(program, source))?;
                let detail = command_detail(&output);
                let suffix = if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                };
                return Err(CodexxError::Config(format!(
                    "{label}超时（{} 秒）{suffix}",
                    timeout.as_secs()
                )));
            }
            None => thread::sleep(COMMAND_POLL_INTERVAL),
        }
    }
}

pub(super) fn run_node(
    node: &Path,
    script: &Path,
    args: &[String],
    label: &str,
    timeout: Duration,
) -> Result<String> {
    let child = Command::new(node)
        .arg(script)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| io_err(script, source))?;
    let output = wait_for_output(child, script, label, timeout)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let detail = command_detail(&output);
        Err(CodexxError::Config(if detail.is_empty() {
            format!("{label}失败")
        } else {
            format!("{label}失败: {detail}")
        }))
    }
}

pub(super) fn stage_theme_snapshot(
    node: &Path,
    assets: &RuntimeAssets,
    snapshots_root: &Path,
    source: &Path,
    theme_id: &str,
) -> Result<PathBuf> {
    ensure_runtime_directory(snapshots_root)?;
    let snapshot = snapshots_root.join(format!(
        "{}-{}-{}",
        theme_id,
        std::process::id(),
        chrono::Local::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
    ));
    ensure_runtime_directory(&snapshot)?;
    let result = (|| -> Result<()> {
        run_node(
            node,
            &assets.stage_theme,
            &[
                source.to_string_lossy().to_string(),
                snapshot.to_string_lossy().to_string(),
            ],
            "暂存主题",
            THEME_STAGE_TIMEOUT,
        )?;
        run_node(
            node,
            &assets.injector,
            &[
                "--check-payload".to_string(),
                "--theme-dir".to_string(),
                snapshot.to_string_lossy().to_string(),
            ],
            "校验主题注入负载",
            PAYLOAD_CHECK_TIMEOUT,
        )?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&snapshot);
    }
    result.map(|_| snapshot)
}

pub(super) fn run_injector_once(
    node: &Path,
    assets: &RuntimeAssets,
    port: u16,
    theme_dir: &Path,
) -> Result<()> {
    run_node(
        node,
        &assets.injector,
        &[
            "--once".to_string(),
            "--port".to_string(),
            port.to_string(),
            "--theme-dir".to_string(),
            theme_dir.to_string_lossy().to_string(),
            "--timeout-ms".to_string(),
            "20000".to_string(),
        ],
        "注入并验证 Codex 皮肤",
        INJECTOR_ONCE_TIMEOUT,
    )?;
    Ok(())
}

pub(super) fn remove_live_skin(
    node: &Path,
    assets: &RuntimeAssets,
    port: u16,
    theme_dir: &Path,
) -> Result<()> {
    run_node(
        node,
        &assets.injector,
        &[
            "--remove".to_string(),
            "--port".to_string(),
            port.to_string(),
            "--theme-dir".to_string(),
            theme_dir.to_string_lossy().to_string(),
            "--timeout-ms".to_string(),
            "10000".to_string(),
        ],
        "移除 Codex 皮肤",
        INJECTOR_REMOVE_TIMEOUT,
    )?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn wait_for_output_collects_a_completed_child() {
        let child = Command::new("/bin/echo")
            .arg("skin-ready")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn echo fixture");
        let output = wait_for_output(
            child,
            Path::new("/bin/echo"),
            "测试皮肤命令",
            Duration::from_secs(1),
        )
        .expect("collect output");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "skin-ready");
    }

    #[test]
    fn wait_for_output_terminates_a_stuck_child() {
        let child = Command::new("/bin/sleep")
            .arg("5")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep fixture");
        let started = Instant::now();
        let error = wait_for_output(
            child,
            Path::new("/bin/sleep"),
            "测试皮肤命令",
            Duration::from_millis(80),
        )
        .expect_err("timeout stuck child");
        assert!(error.to_string().contains("超时"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
