use super::node_runtime::command_detail;
use crate::error::{CodexxError, Result};
use crate::file_io::io_err;
use std::path::{Path, PathBuf};
use std::process::Command;

const EXPECTED_BUNDLE_ID: &str = "com.openai.codex";
const EXPECTED_TEAM_ID: &str = "2DC432GLL2";

#[derive(Debug, Clone)]
pub(super) struct CodexRuntime {
    pub(super) bundle: PathBuf,
    pub(super) executable: PathBuf,
    pub(super) version: String,
    pub(super) node: PathBuf,
    pub(super) node_version: String,
    pub(super) team_id: String,
}

fn output_text(program: &Path, args: &[&str], label: &str) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|source| io_err(program, source))?;
    if !output.status.success() {
        let detail = command_detail(&output);
        return Err(CodexxError::Config(if detail.is_empty() {
            format!("{label}失败")
        } else {
            format!("{label}失败: {detail}")
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn plist_value(bundle: &Path, key: &str) -> Result<String> {
    let plist = bundle.join("Contents/Info.plist");
    output_text(
        Path::new("/usr/bin/plutil"),
        &["-extract", key, "raw", "-o", "-", &plist.to_string_lossy()],
        "读取 Codex 应用信息",
    )
}

fn bundle_candidates() -> Result<Vec<PathBuf>> {
    let home = crate::paths::home_dir()?;
    let mut candidates = vec![
        PathBuf::from("/Applications/ChatGPT.app"),
        home.join("Applications/ChatGPT.app"),
        PathBuf::from("/Applications/Codex.app"),
        home.join("Applications/Codex.app"),
    ];
    if let Ok(output) = Command::new("/usr/bin/mdfind")
        .arg(format!(
            "kMDItemCFBundleIdentifier == \"{EXPECTED_BUNDLE_ID}\""
        ))
        .output()
    {
        if output.status.success() {
            candidates.extend(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(PathBuf::from),
            );
        }
    }
    candidates.dedup();
    Ok(candidates)
}

fn codesign_team_id(path: &Path) -> Result<String> {
    let output = Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=4"])
        .arg(path)
        .output()
        .map_err(|source| io_err(path, source))?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    combined
        .lines()
        .find_map(|line| line.trim().strip_prefix("TeamIdentifier="))
        .map(str::to_string)
        .ok_or_else(|| CodexxError::Config("无法读取 Codex 签名 Team ID".to_string()))
}

fn verify_signature(path: &Path, deep: bool) -> Result<()> {
    let mut command = Command::new("/usr/bin/codesign");
    command.arg("--verify");
    if deep {
        command.arg("--deep");
    }
    let output = command
        .arg("--strict")
        .arg(path)
        .output()
        .map_err(|source| io_err(path, source))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CodexxError::Config(format!(
            "官方 Codex 代码签名校验失败: {}",
            path.display()
        )))
    }
}

pub(super) fn discover_codex_runtime() -> Result<CodexRuntime> {
    let mut bundle = None;
    for candidate in bundle_candidates()? {
        if !candidate.join("Contents/Info.plist").is_file() {
            continue;
        }
        if plist_value(&candidate, "CFBundleIdentifier")
            .ok()
            .as_deref()
            == Some(EXPECTED_BUNDLE_ID)
        {
            bundle = Some(candidate);
            break;
        }
    }
    let bundle = bundle.ok_or_else(|| {
        CodexxError::Config("未找到官方 Codex Desktop (com.openai.codex)".to_string())
    })?;
    let executable_name = plist_value(&bundle, "CFBundleExecutable")?;
    if executable_name.is_empty() || executable_name.contains('/') || executable_name.contains('\\')
    {
        return Err(CodexxError::Config(
            "Codex 应用声明了无效的可执行文件名".to_string(),
        ));
    }
    let executable = bundle.join("Contents/MacOS").join(executable_name);
    let node = bundle.join("Contents/Resources/cua_node/bin/node");
    if !executable.is_file() || !node.is_file() {
        return Err(CodexxError::Config(
            "Codex 可执行文件或内置 Node.js 不存在，请更新或重新安装官方 Codex".to_string(),
        ));
    }
    verify_signature(&bundle, true)?;
    verify_signature(&node, false)?;
    let team_id = codesign_team_id(&bundle)?;
    let node_team_id = codesign_team_id(&node)?;
    if team_id != EXPECTED_TEAM_ID || node_team_id != team_id {
        return Err(CodexxError::Config(format!(
            "Codex 签名身份不匹配，拒绝启动换肤运行时: {team_id}"
        )));
    }
    let node_version = output_text(&node, &["--version"], "读取 Codex Node.js 版本")?;
    let node_major = node_version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| CodexxError::Config(format!("无法解析 Node.js 版本: {node_version}")))?;
    if node_major < 20 {
        return Err(CodexxError::Config(format!(
            "Codex 内置 Node.js 版本过低: {node_version}，需要 20 或更高版本"
        )));
    }
    let machine_arch = output_text(Path::new("/usr/bin/uname"), &["-m"], "读取系统架构")?;
    let node_file = output_text(
        Path::new("/usr/bin/file"),
        &[&node.to_string_lossy()],
        "读取 Codex Node.js 架构",
    )?;
    if !node_file.contains(&machine_arch) {
        return Err(CodexxError::Config(format!(
            "Codex 内置 Node.js 与当前 Mac 架构不匹配: {machine_arch}"
        )));
    }
    Ok(CodexRuntime {
        version: plist_value(&bundle, "CFBundleShortVersionString")?,
        bundle,
        executable,
        node,
        node_version,
        team_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_detail_is_bounded() {
        let output = Command::new("/usr/bin/printf")
            .arg("%0900d")
            .arg("1")
            .output()
            .expect("create fixture output");
        assert!(command_detail(&output).chars().count() <= 800);
    }
}
