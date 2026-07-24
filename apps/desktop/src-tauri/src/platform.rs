use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(target_os = "windows")]
use std::env;

#[cfg(any(target_os = "windows", test))]
const WINDOWS_CODEX_PACKAGE_IDENTITIES: &[&str] =
    &["OpenAI.Codex", "OpenAI.CodexBeta", "OpenAI.ChatGPT-Desktop"];
#[cfg(target_os = "windows")]
const WINDOWS_CODEX_EXECUTABLES: &[&str] = &["ChatGPT.exe", "Codex.exe", "codex.exe"];

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
pub fn program_command(program: &Path, args: &[&str]) -> Command {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let is_script = program
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    let mut command = if is_script {
        let mut shell = Command::new("cmd.exe");
        let command_line = format!("\"\"{}\" {}\"", program.display(), args.join(" "));
        shell.args(["/D", "/S", "/C"]).arg(command_line);
        shell
    } else {
        let mut direct = Command::new(program);
        direct.args(args);
        direct
    };
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(not(target_os = "windows"))]
pub fn program_command(program: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    command
}

fn run_program(program: &Path, args: &[&str]) -> Option<Output> {
    program_command(program, args).output().ok()
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

#[cfg(any(target_os = "windows", test))]
fn numeric_version(value: &str) -> Option<Vec<u32>> {
    let parts = value
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    (parts.len() >= 2).then_some(parts)
}

#[cfg(any(target_os = "windows", test))]
fn windows_package_version(package_name: &str) -> Option<(Vec<u32>, String)> {
    for identity in WINDOWS_CODEX_PACKAGE_IDENTITIES {
        let prefix_len = identity.len();
        if !package_name
            .get(..prefix_len)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(identity))
            || package_name.as_bytes().get(prefix_len) != Some(&b'_')
        {
            continue;
        }
        let version = package_name.get(prefix_len + 1..)?.split('_').next()?;
        return Some((numeric_version(version)?, version.to_string()));
    }
    None
}

#[cfg(any(target_os = "windows", test))]
fn latest_windows_package_version<'a>(
    package_names: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    package_names
        .into_iter()
        .filter_map(windows_package_version)
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, version)| version)
}

#[cfg(target_os = "windows")]
fn windows_store_app_version_from_roots(roots: &[PathBuf]) -> Option<String> {
    let mut package_names = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        package_names.extend(entries.flatten().filter_map(|entry| {
            entry
                .path()
                .is_dir()
                .then(|| entry.file_name().to_string_lossy().to_string())
        }));
    }
    latest_windows_package_version(package_names.iter().map(String::as_str))
        .map(|version| format!("Codex app {version}"))
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
        home.join("Applications/Codex.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/OpenAI Codex.app/Contents/Resources/codex"),
        home.join("Applications/OpenAI Codex.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/OpenAI.Codex.app/Contents/Resources/codex"),
        home.join("Applications/OpenAI.Codex.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/ChatGPT Codex.app/Contents/Resources/codex"),
        home.join("Applications/ChatGPT Codex.app/Contents/Resources/codex"),
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
            localappdata.join("Programs/OpenAI/Codex"),
            localappdata.join("OpenAI/ChatGPT"),
            localappdata.join("OpenAI/Codex"),
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
    let home = dirs::home_dir().unwrap_or_default();
    for root in [PathBuf::from("/Applications"), home.join("Applications")] {
        for name in [
            "Codex.app",
            "OpenAI Codex.app",
            "OpenAI.Codex.app",
            "ChatGPT Codex.app",
            "ChatGPT.app",
        ] {
            let app = root.join(name);
            if !app.is_dir() {
                continue;
            }
            let app_name = if name == "ChatGPT.app" {
                "ChatGPT app"
            } else {
                "Codex app"
            };
            if let Some(version) = macos_info_plist_version(&app).or_else(|| {
                let app = app.to_str()?;
                let output =
                    run_program(Path::new("mdls"), &["-name", "kMDItemVersion", "-raw", app])?;
                output
                    .status
                    .success()
                    .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
            }) {
                if !version.is_empty() && version != "(null)" {
                    return Some(format!("{app_name} {version}"));
                }
            }
            return Some(format!("{app_name} installed"));
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn macos_info_plist_version(app: &Path) -> Option<String> {
    let plist = fs::read_to_string(app.join("Contents/Info.plist")).ok()?;
    plist_string_value(&plist, "CFBundleShortVersionString")
        .or_else(|| plist_string_value(&plist, "CFBundleVersion"))
}

#[cfg(any(target_os = "macos", test))]
fn plist_string_value(plist: &str, key: &str) -> Option<String> {
    let (_, after_key) = plist.split_once(&format!("<key>{key}</key>"))?;
    let (_, after_open) = after_key.split_once("<string>")?;
    let (value, _) = after_open.split_once("</string>")?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(not(target_os = "macos"))]
fn macos_app_version() -> Option<String> {
    None
}

#[cfg(target_os = "windows")]
fn windows_app_version() -> Option<String> {
    let mut roots = Vec::new();
    for variable in ["ProgramFiles", "ProgramW6432"] {
        if let Ok(program_files) = env::var(variable) {
            roots.push(PathBuf::from(program_files).join("WindowsApps"));
        }
    }
    roots.push(PathBuf::from(r"C:\Program Files\WindowsApps"));
    roots.sort();
    roots.dedup();
    if let Some(version) = windows_store_app_version_from_roots(&roots) {
        return Some(version);
    }

    let script = "Get-AppxPackage | Where-Object { $_.Name -in @('OpenAI.Codex','OpenAI.CodexBeta','OpenAI.ChatGPT-Desktop') } | ForEach-Object { $_.Version.ToString() }";
    if let Some(output) = run_program(
        Path::new("powershell.exe"),
        &["-NoProfile", "-NonInteractive", "-Command", script],
    ) {
        if output.status.success() {
            let versions = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .filter_map(|version| {
                    numeric_version(version).map(|parsed| (parsed, version.to_string()))
                })
                .max_by(|left, right| left.0.cmp(&right.0));
            if let Some((_, version)) = versions {
                return Some(format!("Codex app {version}"));
            }
        }
    }

    let local_appdata = env::var("LOCALAPPDATA").ok().map(PathBuf::from)?;
    for directory in [
        local_appdata.join("OpenAI/Codex/bin"),
        local_appdata.join("OpenAI/Codex"),
        local_appdata.join("Programs/OpenAI/Codex"),
        local_appdata.join("Programs/Codex"),
    ] {
        if WINDOWS_CODEX_EXECUTABLES.iter().any(|name| {
            directory.join(name).is_file() || directory.join("app").join(name).is_file()
        }) {
            return Some("Codex app installed".to_string());
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn windows_app_version() -> Option<String> {
    None
}

pub fn codex_executable_candidates() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut candidates = ["codex", "codex.exe", "codex.cmd"]
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    candidates.extend(windows_where_candidates());
    candidates.extend(platform_candidates(&home));
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for candidate in candidates {
        push_candidate(&mut unique, &mut seen, candidate);
    }
    unique
}

pub fn detect_codex_version() -> Option<String> {
    for candidate in codex_executable_candidates() {
        let is_bare_command = candidate.components().count() == 1;
        if is_bare_command || candidate.is_file() {
            if let Some(version) = command_version(&candidate) {
                return Some(version);
            }
        }
    }
    macos_app_version().or_else(windows_app_version)
}

#[cfg(test)]
mod tests {
    use super::{latest_windows_package_version, plist_string_value, version_line};

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

    #[test]
    fn windows_package_detection_accepts_supported_codex_packages() {
        assert_eq!(
            latest_windows_package_version([
                "OpenAI.Codex_1.2.3.4_x64__publisher",
                "OpenAI.CodexBeta_1.3.0.0_x64__publisher",
                "Other.App_99.0.0.0_x64__publisher",
            ]),
            Some("1.3.0.0".to_string())
        );
    }

    #[test]
    fn plist_parser_reads_codex_bundle_version() {
        let plist = r#"<plist><dict>
<key>CFBundleShortVersionString</key>
<string>1.2026.204</string>
</dict></plist>"#;
        assert_eq!(
            plist_string_value(plist, "CFBundleShortVersionString").as_deref(),
            Some("1.2026.204")
        );
    }
}
