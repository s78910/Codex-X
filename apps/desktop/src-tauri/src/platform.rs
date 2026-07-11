use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(target_os = "windows")]
use std::env;

fn version_line(stdout: &str, stderr: &str, success: bool) -> Option<String> {
    let lines = stdout.lines().chain(stderr.lines()).map(str::trim);
    let preferred = lines.clone().find(|line| {
        let lower = line.to_ascii_lowercase();
        !line.is_empty()
            && !lower.starts_with("warning:")
            && (lower.contains("codex-cli")
                || lower.contains("@openai/codex")
                || lower.starts_with("codex "))
            && line.chars().any(|ch| ch.is_ascii_digit())
    });
    if preferred.is_some() {
        return preferred.map(ToString::to_string);
    }
    if !success {
        return None;
    }
    lines
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            !line.is_empty()
                && !lower.starts_with("warning:")
                && !lower.starts_with("error:")
                && line.chars().any(|ch| ch.is_ascii_digit())
        })
        .map(ToString::to_string)
        .next()
}

fn version_from_output(output: Output) -> Option<String> {
    version_line(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        output.status.success(),
    )
}

#[cfg(target_os = "windows")]
fn run_program(program: &Path, args: &[&str]) -> Option<Output> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let is_script = program
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    let mut command = if is_script {
        let mut shell = Command::new("cmd.exe");
        let command_line = format!("\"{}\" {}", program.display(), args.join(" "));
        shell.args(["/D", "/S", "/C"]).arg(command_line);
        shell
    } else {
        let mut direct = Command::new(program);
        direct.args(args);
        direct
    };
    command.creation_flags(CREATE_NO_WINDOW).output().ok()
}

#[cfg(not(target_os = "windows"))]
fn run_program(program: &Path, args: &[&str]) -> Option<Output> {
    Command::new(program).args(args).output().ok()
}

fn command_version(program: &Path) -> Option<String> {
    run_program(program, &["--version"])
        .and_then(version_from_output)
        .or_else(|| run_program(program, &["-V"]).and_then(version_from_output))
}

fn candidate_key(path: &Path) -> String {
    let value = path.to_string_lossy().to_string();
    if cfg!(target_os = "windows") {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn push_candidate(candidates: &mut Vec<PathBuf>, seen: &mut HashSet<String>, path: PathBuf) {
    if seen.insert(candidate_key(&path)) {
        candidates.push(path);
    }
}

fn collect_named_files(root: &Path, names: &[&str], depth: usize, output: &mut Vec<PathBuf>) {
    if depth == 0 || !root.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_named_files(&path, names, depth - 1, output);
        } else if path.is_file()
            && path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| {
                    names
                        .iter()
                        .any(|candidate| name.eq_ignore_ascii_case(candidate))
                })
        {
            output.push(path);
        }
    }
}

fn extension_codex_candidates(home: &Path) -> Vec<PathBuf> {
    let roots = [
        home.join(".cursor").join("extensions"),
        home.join(".vscode").join("extensions"),
        home.join(".vscode-insiders").join("extensions"),
        home.join(".windsurf").join("extensions"),
    ];
    let mut candidates = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        let mut extension_dirs = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .is_some_and(|name| {
                            let lower = name.to_ascii_lowercase();
                            lower.starts_with("openai.chatgpt-")
                                || lower.starts_with("openai.codex-")
                        })
            })
            .collect::<Vec<_>>();
        extension_dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        for extension_dir in extension_dirs {
            collect_named_files(
                &extension_dir,
                &["codex", "codex.exe", "codex.cmd"],
                5,
                &mut candidates,
            );
        }
    }
    candidates
}

#[cfg(target_os = "macos")]
fn platform_candidates(home: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
        home.join("Applications/ChatGPT.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/OpenAI Codex.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/ChatGPT Codex.app/Contents/Resources/codex"),
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
        home.join(".local/bin/codex"),
        home.join(".npm-global/bin/codex"),
        home.join("Library/pnpm/codex"),
    ];
    candidates.extend(extension_codex_candidates(home));
    candidates
}

#[cfg(target_os = "windows")]
fn platform_candidates(home: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(appdata) = env::var("APPDATA") {
        let appdata = PathBuf::from(appdata);
        candidates.push(appdata.join("npm").join("codex.cmd"));
        candidates.push(appdata.join("npm").join("codex.exe"));
        for target in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
            candidates.push(
                appdata
                    .join("npm/node_modules/@openai/codex/vendor")
                    .join(target)
                    .join("codex/codex.exe"),
            );
        }
    }
    if let Ok(localappdata) = env::var("LOCALAPPDATA") {
        let localappdata = PathBuf::from(localappdata);
        candidates.push(localappdata.join("Microsoft/WindowsApps/codex.exe"));
        candidates.push(localappdata.join("Microsoft/WindowsApps/codex.cmd"));
        for root in [
            localappdata.join("Programs/ChatGPT"),
            localappdata.join("Programs/Codex"),
            localappdata.join("OpenAI/ChatGPT"),
        ] {
            collect_named_files(&root, &["codex.exe", "codex.cmd"], 7, &mut candidates);
        }
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(program_files) = env::var(variable) {
            for app in ["ChatGPT", "Codex"] {
                collect_named_files(
                    &PathBuf::from(&program_files).join(app),
                    &["codex.exe", "codex.cmd"],
                    7,
                    &mut candidates,
                );
            }
        }
    }
    candidates.extend(extension_codex_candidates(home));
    candidates
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn platform_candidates(home: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/usr/local/bin/codex"),
        PathBuf::from("/usr/bin/codex"),
        PathBuf::from("/snap/bin/codex"),
        home.join(".local/bin/codex"),
        home.join(".npm-global/bin/codex"),
        home.join(".local/share/pnpm/codex"),
    ];
    candidates.extend(extension_codex_candidates(home));
    candidates
}

#[cfg(target_os = "windows")]
fn windows_where_candidates() -> Vec<PathBuf> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let mut command = Command::new("where.exe");
    let Ok(output) = command
        .creation_flags(CREATE_NO_WINDOW)
        .arg("codex")
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn windows_where_candidates() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(target_os = "macos")]
fn macos_app_version() -> Option<String> {
    for app in [
        "/Applications/ChatGPT.app",
        "/Applications/Codex.app",
        "/Applications/OpenAI Codex.app",
        "/Applications/ChatGPT Codex.app",
    ] {
        let Some(output) =
            run_program(Path::new("mdls"), &["-name", "kMDItemVersion", "-raw", app])
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !version.is_empty() && version != "(null)" {
            let app_name = if app.ends_with("ChatGPT.app") {
                "ChatGPT app"
            } else {
                "Codex app"
            };
            return Some(format!("{app_name} {version}"));
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn macos_app_version() -> Option<String> {
    None
}

pub fn detect_codex_version() -> Option<String> {
    for command in ["codex", "codex.exe", "codex.cmd"] {
        if let Some(version) = command_version(Path::new(command)) {
            return Some(version);
        }
    }

    let home = dirs::home_dir().unwrap_or_default();
    let mut candidates = windows_where_candidates();
    candidates.extend(platform_candidates(&home));
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for candidate in candidates {
        push_candidate(&mut unique, &mut seen, candidate);
    }
    for candidate in unique {
        if candidate.is_file() {
            if let Some(version) = command_version(&candidate) {
                return Some(version);
            }
        }
    }
    macos_app_version()
}

#[cfg(test)]
mod tests {
    use super::version_line;

    #[test]
    fn version_parser_prefers_codex_line_over_warning() {
        assert_eq!(
            version_line(
                "codex-cli 0.144.0-alpha.4\n",
                "WARNING: could not create PATH aliases\n",
                true,
            )
            .as_deref(),
            Some("codex-cli 0.144.0-alpha.4")
        );
    }

    #[test]
    fn version_parser_accepts_successful_plain_version() {
        assert_eq!(
            version_line("0.42.0\n", "", true).as_deref(),
            Some("0.42.0")
        );
    }

    #[test]
    fn version_parser_rejects_failed_error_output() {
        assert_eq!(
            version_line("", "error: command not found 127\n", false),
            None
        );
    }
}
