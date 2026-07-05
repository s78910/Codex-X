use std::process::Command;

pub fn command_version(program: &str, args: &[&str]) -> Option<String> {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
pub fn macos_codex_app_version() -> Option<String> {
    let candidates = [
        "/Applications/Codex.app",
        "/Applications/OpenAI Codex.app",
        "/Applications/ChatGPT Codex.app",
    ];
    for app in candidates {
        let output = Command::new("mdls")
            .args(["-name", "kMDItemVersion", "-raw", app])
            .output()
            .ok()?;
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !text.is_empty() && text != "(null)" {
                return Some(text);
            }
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
pub fn macos_codex_app_version() -> Option<String> {
    None
}

pub fn detect_codex_version() -> Option<String> {
    command_version("codex", &["--version"])
        .or_else(|| command_version("codex", &["-V"]))
        .or_else(macos_codex_app_version)
}
