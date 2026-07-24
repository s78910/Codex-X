use crate::error::{CodexxError, Result};
use crate::file_io::{atomic_write, ensure_directory, io_err};
use crate::skin_presets::{builtin_skin_assets, retired_builtin_skin_ids, BUILTIN_SKIN_ID};
use crate::skin_runtime::{
    apply_skin_runtime, pause_skin_runtime, restore_skin_runtime, skin_runtime_status,
    SkinRuntimeAction, SkinRuntimeStatus,
};
use crate::{now_rfc3339, sanitize_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zip::write::SimpleFileOptions;

const MAX_THEME_ZIP_BYTES: usize = 24 * 1024 * 1024;
const MAX_THEME_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_THEME_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_THEME_ARCHIVE_ENTRIES: usize = 64;
const MAX_THEME_ID_BYTES: usize = 80;
const MIN_SURFACE_OPACITY: f64 = 0.35;
const IMAGE_THEME_SURFACE_OPACITY: f64 = 0.62;
const STATE_FILE: &str = "state.json";
const LEGACY_PLACEHOLDER_SHA256: &str =
    "ebf4f635a17d10d6eb46ba680b70142419aa3220f228001a036d311a22ee9d2a";
static SKIN_OPERATION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinThemeColors {
    pub(crate) background: String,
    pub(crate) panel: String,
    pub(crate) panel_alt: String,
    pub(crate) accent: String,
    pub(crate) accent_alt: String,
    pub(crate) secondary: String,
    pub(crate) highlight: String,
    pub(crate) text: String,
    pub(crate) muted: String,
    pub(crate) line: String,
}

impl Default for SkinThemeColors {
    fn default() -> Self {
        Self {
            background: "#071116".to_string(),
            panel: "#0b1a20".to_string(),
            panel_alt: "#10272c".to_string(),
            accent: "#38bdf8".to_string(),
            accent_alt: "#7dd3fc".to_string(),
            secondary: "#22c55e".to_string(),
            highlight: "#6366f1".to_string(),
            text: "#e9fff1".to_string(),
            muted: "#9ebdb3".to_string(),
            line: "rgba(56, 189, 248, .30)".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinThemeArt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) focus_x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) focus_y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) safe_area: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinThemeManifest {
    pub(crate) schema_version: u32,
    #[serde(default)]
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) brand_subtitle: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) tagline: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) project_prefix: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) project_label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) status_text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) quote: String,
    pub(crate) image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) appearance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) surface_opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) art: Option<SkinThemeArt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) colors: Option<SkinThemeColors>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinThemeSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) tagline: String,
    pub(crate) quote: String,
    pub(crate) image: String,
    pub(crate) image_path: String,
    pub(crate) source: String,
    pub(crate) enabled: bool,
    pub(crate) directory: String,
    pub(crate) adaptive: bool,
    pub(crate) surface_opacity: f64,
    pub(crate) art: SkinThemeArt,
    pub(crate) colors: SkinThemeColors,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinCenterState {
    pub(crate) skins_dir: String,
    pub(crate) current_theme_id: Option<String>,
    pub(crate) current_theme_path: Option<String>,
    pub(crate) themes: Vec<SkinThemeSummary>,
    pub(crate) runtime: SkinRuntimeStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinActionResult {
    pub(crate) message: String,
    pub(crate) state: SkinCenterState,
    pub(crate) restart_required: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinExportResult {
    pub(crate) path: String,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SkinStateFile {
    current_theme_id: Option<String>,
    updated_at: Option<String>,
}

pub(crate) fn skins_root() -> Result<PathBuf> {
    Ok(crate::paths::app_home()?.join("codex-x-skins"))
}

fn themes_root() -> Result<PathBuf> {
    Ok(skins_root()?.join("themes"))
}

fn current_root() -> Result<PathBuf> {
    Ok(skins_root()?.join("current"))
}

fn exports_root() -> Result<PathBuf> {
    Ok(skins_root()?.join("exports"))
}

fn state_path() -> Result<PathBuf> {
    Ok(skins_root()?.join(STATE_FILE))
}

fn normalize_export_destination(value: &str) -> Result<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CodexxError::Config("请选择皮肤主题的导出位置".to_string()));
    }
    let mut path = PathBuf::from(trimmed);
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(CodexxError::Config("皮肤导出路径无效".to_string()));
    }
    match path.extension().and_then(|extension| extension.to_str()) {
        None => {
            path.set_extension("zip");
        }
        Some(extension) if extension.eq_ignore_ascii_case("zip") => {}
        Some(_) => {
            return Err(CodexxError::Config(
                "皮肤主题必须导出为 .zip 文件".to_string(),
            ))
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| CodexxError::Config("皮肤导出目录无效".to_string()))?;
    let parent_metadata = fs::metadata(parent).map_err(|error| io_err(parent, error))?;
    if !parent_metadata.is_dir() {
        return Err(CodexxError::Config(format!(
            "皮肤导出目录不存在：{}",
            parent.display()
        )));
    }
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(CodexxError::Config(
            "皮肤导出目标不能是文件链接".to_string(),
        )),
        Ok(metadata) if !metadata.is_file() => {
            Err(CodexxError::Config("皮肤导出目标不是普通文件".to_string()))
        }
        Ok(_) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(io_err(&path, error)),
    }
}

fn normalize_theme_id(value: &str) -> Result<String> {
    let id = sanitize_id(value);
    let id = if id == "provider" {
        "skin-provider".to_string()
    } else {
        id
    };
    if id.len() > MAX_THEME_ID_BYTES {
        return Err(CodexxError::Config(format!(
            "主题 ID 不能超过 {MAX_THEME_ID_BYTES} 个 ASCII 字符"
        )));
    }
    Ok(id)
}

fn validate_image_name(name: &str) -> Result<()> {
    let clean = name.trim();
    if clean.is_empty()
        || clean.len() > 255
        || clean.contains('/')
        || clean.contains('\\')
        || clean == "."
        || clean == ".."
        || clean.chars().any(|ch| {
            ch.is_control() || matches!(ch, '\u{007f}'..='\u{009f}' | '\u{2028}' | '\u{2029}')
        })
    {
        return Err(CodexxError::Config(
            "主题图片必须位于主题包根目录".to_string(),
        ));
    }
    let lower = clean.to_ascii_lowercase();
    if ![".png", ".jpg", ".jpeg", ".webp"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        return Err(CodexxError::Config(
            "主题图片仅支持 PNG/JPEG/WebP".to_string(),
        ));
    }
    Ok(())
}

fn uploaded_image_extension(file_name: &str, bytes: &[u8]) -> Result<&'static str> {
    validate_image_name(file_name)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_THEME_IMAGE_BYTES {
        return Err(CodexxError::Config(
            "主题图片不能为空且不能超过 16MB".to_string(),
        ));
    }
    let lower = file_name.trim().to_ascii_lowercase();
    let extension = if lower.ends_with(".png") {
        "png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "jpg"
    } else {
        "webp"
    };
    let signature_matches = match extension {
        "png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "jpg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => false,
    };
    if !signature_matches {
        return Err(CodexxError::Config(
            "图片内容与文件格式不匹配，请选择有效的 PNG/JPEG/WebP 图片".to_string(),
        ));
    }
    Ok(extension)
}

fn uploaded_theme_name(file_name: &str) -> String {
    let trimmed = file_name.trim();
    let lower = trimmed.to_ascii_lowercase();
    let suffix_len = [".jpeg", ".webp", ".png", ".jpg"]
        .iter()
        .find(|suffix| lower.ends_with(**suffix))
        .map_or(0, |suffix| suffix.len());
    let candidate = trimmed[..trimmed.len().saturating_sub(suffix_len)].trim();
    let candidate = if candidate.is_empty() {
        "我的图片皮肤"
    } else {
        candidate
    };
    if candidate.len() <= 160 {
        return candidate.to_string();
    }
    let mut end = 160;
    while !candidate.is_char_boundary(end) {
        end -= 1;
    }
    candidate[..end].trim().to_string()
}

fn unique_theme_id_in(root: &Path, preferred: &str) -> Result<String> {
    let preferred = normalize_theme_id(preferred)?;
    let available = |id: &str| -> Result<bool> {
        match fs::symlink_metadata(root.join(id)) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(io_err(&root.join(id), error)),
        }
    };
    if available(&preferred)? {
        return Ok(preferred);
    }
    let mut index = 2usize;
    loop {
        let suffix = format!("-{index}");
        let base_len = MAX_THEME_ID_BYTES.saturating_sub(suffix.len());
        let candidate = format!("{}{}", &preferred[..preferred.len().min(base_len)], suffix);
        if available(&candidate)? {
            return Ok(candidate);
        }
        index += 1;
    }
}

fn write_manifest(dir: &Path, manifest: &SkinThemeManifest) -> Result<()> {
    ensure_directory(dir)?;
    let path = dir.join("theme.json");
    let text = serde_json::to_string_pretty(manifest)
        .map_err(|e| CodexxError::Config(format!("序列化主题失败: {e}")))?;
    atomic_write(&path, format!("{text}\n").as_bytes())
}

fn install_builtin_theme(
    manifest: &SkinThemeManifest,
    expected_id: &str,
    image_bytes: &[u8],
) -> Result<()> {
    if manifest.id != expected_id {
        return Err(CodexxError::Config(format!(
            "内置主题 ID 不匹配: {expected_id}"
        )));
    }
    let dir = themes_root()?.join(&manifest.id);
    ensure_directory(&dir)?;
    let mut installed_manifest = manifest.clone();
    if let Ok(existing) = read_manifest(&dir) {
        if existing.surface_opacity.is_some() {
            installed_manifest.surface_opacity = existing.surface_opacity;
        }
    }
    write_manifest(&dir, &installed_manifest)?;
    let image = dir.join(&manifest.image);
    let image_matches = fs::read(&image).is_ok_and(|current| current == image_bytes);
    if !image_matches {
        atomic_write(&image, image_bytes)?;
    }
    Ok(())
}

fn migrate_legacy_placeholder_themes() -> Result<()> {
    let legacy_ids = ["aurora-terminal", "sakura-glass", "neon-night"];
    let mut state = read_skin_state()?;
    let mut state_changed = false;
    for legacy_id in legacy_ids {
        let legacy_dir = themes_root()?.join(legacy_id);
        let legacy_image = legacy_dir.join("background.png");
        let Ok(bytes) = fs::read(&legacy_image) else {
            continue;
        };
        if format!("{:x}", Sha256::digest(&bytes)) != LEGACY_PLACEHOLDER_SHA256 {
            continue;
        }
        if state.current_theme_id.as_deref() == Some(legacy_id) {
            let replacement_dir = themes_root()?.join(BUILTIN_SKIN_ID);
            let replacement = read_manifest(&replacement_dir)?;
            copy_theme_files(&replacement_dir, &current_root()?, &replacement)?;
            state.current_theme_id = Some(BUILTIN_SKIN_ID.to_string());
            state.updated_at = Some(now_rfc3339());
            state_changed = true;
        }
        fs::remove_dir_all(&legacy_dir).map_err(|source| io_err(&legacy_dir, source))?;
    }
    if state_changed {
        write_skin_state(&state)?;
    }
    Ok(())
}

fn remove_retired_theme_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_err(path, error)),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).map_err(|source| io_err(path, source))
    } else {
        fs::remove_file(path).map_err(|source| io_err(path, source))
    }
}

fn migrate_retired_builtin_themes() -> Result<()> {
    let root = themes_root()?;
    let mut state = read_skin_state()?;
    let selected_retired = state
        .current_theme_id
        .as_deref()
        .is_some_and(|id| retired_builtin_skin_ids().contains(&id));
    if selected_retired {
        let replacement_dir = root.join(BUILTIN_SKIN_ID);
        let replacement = read_manifest(&replacement_dir)?;
        copy_theme_files(&replacement_dir, &current_root()?, &replacement)?;
        state.current_theme_id = Some(BUILTIN_SKIN_ID.to_string());
        state.updated_at = Some(now_rfc3339());
        write_skin_state(&state)?;
    }
    for id in retired_builtin_skin_ids() {
        remove_retired_theme_path(&root.join(id))?;
    }
    Ok(())
}

fn ensure_builtin_themes() -> Result<()> {
    ensure_directory(&themes_root()?)?;
    ensure_directory(&current_root()?)?;
    ensure_directory(&exports_root()?)?;
    for asset in builtin_skin_assets() {
        let manifest: SkinThemeManifest = serde_json::from_str(asset.manifest)
            .map_err(|e| CodexxError::Config(format!("解析内置主题 {} 失败: {e}", asset.id)))?;
        validate_manifest_fields(&manifest)?;
        install_builtin_theme(&manifest, asset.id, asset.image)?;
    }
    migrate_legacy_placeholder_themes()?;
    migrate_retired_builtin_themes()?;
    Ok(())
}

fn read_skin_state() -> Result<SkinStateFile> {
    let path = state_path()?;
    if !path.is_file() {
        return Ok(SkinStateFile::default());
    }
    let text = fs::read_to_string(&path).map_err(|e| io_err(&path, e))?;
    serde_json::from_str(&text).map_err(|e| CodexxError::Config(format!("读取皮肤状态失败: {e}")))
}

fn write_skin_state(state: &SkinStateFile) -> Result<()> {
    let path = state_path()?;
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| CodexxError::Config(format!("序列化皮肤状态失败: {e}")))?;
    atomic_write(&path, format!("{text}\n").as_bytes())
}

fn validate_manifest_fields(manifest: &SkinThemeManifest) -> Result<()> {
    if manifest.schema_version != 1 {
        return Err(CodexxError::Config(
            "仅支持 schemaVersion = 1 的主题".to_string(),
        ));
    }
    if manifest.name.trim().is_empty() || manifest.name.len() > 160 {
        return Err(CodexxError::Config(
            "主题名称不能为空且不能超过 160 个字符".to_string(),
        ));
    }
    if !manifest.id.is_empty() && manifest.id.len() > MAX_THEME_ID_BYTES {
        return Err(CodexxError::Config(format!(
            "主题 ID 不能超过 {MAX_THEME_ID_BYTES} 个 ASCII 字符"
        )));
    }
    validate_image_name(&manifest.image)?;
    if let Some(appearance) = manifest.appearance.as_deref() {
        if !matches!(appearance, "auto" | "light" | "dark") {
            return Err(CodexxError::Config(
                "appearance 仅支持 auto/light/dark".to_string(),
            ));
        }
    }
    if manifest
        .surface_opacity
        .is_some_and(|value| !value.is_finite() || !(MIN_SURFACE_OPACITY..=1.0).contains(&value))
    {
        return Err(CodexxError::Config(format!(
            "surfaceOpacity 必须在 {MIN_SURFACE_OPACITY} 到 1 之间"
        )));
    }
    if let Some(art) = &manifest.art {
        for (label, value) in [("focusX", art.focus_x), ("focusY", art.focus_y)] {
            if value.is_some_and(|item| !item.is_finite() || !(0.0..=1.0).contains(&item)) {
                return Err(CodexxError::Config(format!(
                    "主题 {label} 必须在 0 到 1 之间"
                )));
            }
        }
        if art
            .safe_area
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto" | "left" | "right" | "center" | "none"))
        {
            return Err(CodexxError::Config(
                "safeArea 仅支持 auto/left/right/center/none".to_string(),
            ));
        }
        if art
            .task_mode
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto" | "ambient" | "banner" | "off"))
        {
            return Err(CodexxError::Config(
                "taskMode 仅支持 auto/ambient/banner/off".to_string(),
            ));
        }
    }
    Ok(())
}

fn read_manifest(dir: &Path) -> Result<SkinThemeManifest> {
    let path = dir.join("theme.json");
    let metadata = fs::symlink_metadata(&path).map_err(|e| io_err(&path, e))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_THEME_MANIFEST_BYTES
    {
        return Err(CodexxError::Config(
            "theme.json 必须是主题目录内不超过 1MB 的普通文件".to_string(),
        ));
    }
    let bytes = fs::read(&path).map_err(|e| io_err(&path, e))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| CodexxError::Config("theme.json 必须使用 UTF-8 编码".to_string()))?;
    if text.contains('\0') {
        return Err(CodexxError::Config(
            "theme.json 不能包含 NUL 字符".to_string(),
        ));
    }
    let manifest: SkinThemeManifest = serde_json::from_str(text)
        .map_err(|e| CodexxError::Config(format!("解析 theme.json 失败: {e}")))?;
    validate_manifest_fields(&manifest)?;
    let image = dir.join(&manifest.image);
    let meta = fs::symlink_metadata(&image).map_err(|e| io_err(&image, e))?;
    if meta.file_type().is_symlink()
        || !meta.is_file()
        || meta.len() == 0
        || meta.len() > MAX_THEME_IMAGE_BYTES
    {
        return Err(CodexxError::Config(
            "主题图片必须是主题目录内不超过 16MB 的普通文件".to_string(),
        ));
    }
    Ok(manifest)
}

fn copy_theme_files(from: &Path, to: &Path, manifest: &SkinThemeManifest) -> Result<()> {
    let parent = to
        .parent()
        .ok_or_else(|| CodexxError::Config(format!("主题目标目录无效: {}", to.display())))?;
    ensure_directory(parent)?;
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        chrono::Local::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
    );
    let stage = parent.join(format!(".theme-stage-{suffix}"));
    let backup = parent.join(format!(".theme-backup-{suffix}"));
    ensure_directory(&stage)?;
    let result = (|| -> Result<()> {
        write_manifest(&stage, manifest)?;
        let src_image = from.join(&manifest.image);
        let image_bytes = fs::read(&src_image).map_err(|e| io_err(&src_image, e))?;
        atomic_write(&stage.join(&manifest.image), &image_bytes)?;

        let had_existing = match fs::symlink_metadata(to) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::rename(to, &backup).map_err(|e| io_err(to, e))?;
                true
            }
            Ok(_) => {
                return Err(CodexxError::Config(format!(
                    "主题目标不是普通目录: {}",
                    to.display()
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(io_err(to, error)),
        };
        if let Err(error) = fs::rename(&stage, to) {
            if had_existing {
                let _ = fs::rename(&backup, to);
            }
            return Err(io_err(to, error));
        }
        if had_existing {
            fs::remove_dir_all(&backup).map_err(|e| io_err(&backup, e))?;
        }
        Ok(())
    })();
    if stage.exists() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

fn theme_summary(
    id: String,
    dir: PathBuf,
    source: &str,
    enabled: bool,
    manifest: SkinThemeManifest,
) -> SkinThemeSummary {
    let art = manifest.art.clone().unwrap_or_default();
    let adaptive = manifest.colors.is_none();
    SkinThemeSummary {
        id,
        name: manifest.name,
        tagline: if manifest.tagline.trim().is_empty() {
            "自适应图片主题".to_string()
        } else {
            manifest.tagline
        },
        quote: manifest.quote,
        image_path: dir.join(&manifest.image).to_string_lossy().to_string(),
        image: manifest.image,
        source: source.to_string(),
        enabled,
        directory: dir.to_string_lossy().to_string(),
        adaptive,
        surface_opacity: manifest.surface_opacity.unwrap_or(1.0),
        art,
        colors: manifest.colors.unwrap_or_default(),
    }
}

fn normalize_theme_metadata(
    value: String,
    label: &str,
    max_chars: usize,
    required: bool,
) -> Result<String> {
    let value = value.trim().to_string();
    if required && value.is_empty() {
        return Err(CodexxError::Config(format!("{label}不能为空")));
    }
    if value.chars().count() > max_chars {
        return Err(CodexxError::Config(format!(
            "{label}不能超过 {max_chars} 个字符"
        )));
    }
    if value.chars().any(|ch| {
        ch.is_control() || matches!(ch, '\u{007f}'..='\u{009f}' | '\u{2028}' | '\u{2029}')
    }) {
        return Err(CodexxError::Config(format!("{label}不能包含控制字符")));
    }
    Ok(value)
}

fn normalize_surface_opacity(value: f64) -> Result<f64> {
    if !value.is_finite() || !(MIN_SURFACE_OPACITY..=1.0).contains(&value) {
        return Err(CodexxError::Config(format!(
            "界面透明度必须在 0% 到 {}% 之间",
            ((1.0 - MIN_SURFACE_OPACITY) * 100.0).round() as u32
        )));
    }
    Ok((value * 100.0).round() / 100.0)
}

pub(crate) fn update_skin_theme_settings_inner(
    id: String,
    name: String,
    tagline: String,
    surface_opacity: f64,
) -> Result<SkinActionResult> {
    let _guard = SKIN_OPERATION_LOCK
        .lock()
        .map_err(|_| CodexxError::Config("皮肤操作锁已损坏，请重启 Codex-X".to_string()))?;
    ensure_builtin_themes()?;
    let id = normalize_theme_id(&id)?;
    let builtin = builtin_skin_assets().iter().any(|asset| asset.id == id);
    let dir = themes_root()?.join(&id);
    if !dir.is_dir() {
        return Err(CodexxError::Config(format!("没有找到主题: {id}")));
    }
    let mut manifest = read_manifest(&dir)?;
    if manifest.id.trim().is_empty() || normalize_theme_id(&manifest.id)? != id {
        return Err(CodexxError::Config(format!(
            "主题目录与 theme.json ID 不一致: {id}"
        )));
    }
    let original = manifest.clone();
    if !builtin {
        manifest.name = normalize_theme_metadata(name, "主题名称", 80, true)?;
        manifest.tagline = normalize_theme_metadata(tagline, "主题简介", 160, false)?;
    }
    manifest.surface_opacity = Some(normalize_surface_opacity(surface_opacity)?);
    validate_manifest_fields(&manifest)?;
    let state = read_skin_state()?;
    let selected = state.current_theme_id.as_deref() == Some(id.as_str());
    let runtime = skin_runtime_status();
    let active = runtime.active && runtime.theme_id.as_deref() == Some(id.as_str());
    let update_result = (|| -> Result<()> {
        write_manifest(&dir, &manifest)?;
        if selected {
            copy_theme_files(&dir, &current_root()?, &manifest)?;
        }
        if active {
            match apply_skin_runtime(&dir, &id, false)? {
                SkinRuntimeAction::Applied(_) => {}
                SkinRuntimeAction::RestartRequired(message) => {
                    return Err(CodexxError::Config(message));
                }
                _ => {
                    return Err(CodexxError::Config(
                        "皮肤运行时返回了无效的更新状态".to_string(),
                    ));
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = update_result {
        let _ = write_manifest(&dir, &original);
        if selected {
            let _ = copy_theme_files(&dir, &current_root()?, &original);
        }
        return Err(error);
    }
    Ok(SkinActionResult {
        message: format!("已保存皮肤设置：{}", manifest.name),
        state: get_skin_center_state_inner()?,
        restart_required: false,
    })
}

pub(crate) fn get_skin_center_state_inner() -> Result<SkinCenterState> {
    ensure_builtin_themes()?;
    let skin_state = read_skin_state()?;
    let current_id = skin_state.current_theme_id.clone();
    let builtin_ids = builtin_skin_assets()
        .iter()
        .map(|asset| asset.id.to_string())
        .collect::<HashSet<_>>();
    let mut themes = Vec::new();
    let theme_root = themes_root()?;
    for entry in fs::read_dir(&theme_root).map_err(|e| io_err(&theme_root, e))? {
        let entry = entry.map_err(|e| io_err(&theme_root, e))?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if id.starts_with('.') {
            continue;
        }
        let manifest = match read_manifest(&dir) {
            Ok(manifest) => manifest,
            Err(_) => continue,
        };
        let source = if builtin_ids.contains(&id) {
            "builtin"
        } else {
            "imported"
        };
        themes.push(theme_summary(
            id.clone(),
            dir,
            source,
            current_id.as_deref() == Some(id.as_str()),
            manifest,
        ));
    }
    themes.sort_by(|a, b| {
        let source_rank = |source: &str| if source == "builtin" { 0 } else { 1 };
        source_rank(&a.source)
            .cmp(&source_rank(&b.source))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let current_theme_path = if current_id.is_some() {
        Some(
            current_root()?
                .join("theme.json")
                .to_string_lossy()
                .to_string(),
        )
    } else {
        None
    };
    Ok(SkinCenterState {
        skins_dir: skins_root()?.to_string_lossy().to_string(),
        current_theme_id: current_id,
        current_theme_path,
        themes,
        runtime: skin_runtime_status(),
    })
}

pub(crate) fn enable_skin_theme_inner(
    id: String,
    restart_existing: bool,
) -> Result<SkinActionResult> {
    let _guard = SKIN_OPERATION_LOCK
        .lock()
        .map_err(|_| CodexxError::Config("皮肤操作锁已损坏，请重启 Codex-X".to_string()))?;
    ensure_builtin_themes()?;
    let id = normalize_theme_id(&id)?;
    let src = themes_root()?.join(&id);
    if !src.is_dir() {
        return Err(CodexxError::Config(format!("没有找到主题: {id}")));
    }
    let manifest = read_manifest(&src)?;
    if manifest.id.trim().is_empty() || normalize_theme_id(&manifest.id)? != id {
        return Err(CodexxError::Config(format!(
            "主题目录与 theme.json ID 不一致: {id}"
        )));
    }
    let runtime_action = apply_skin_runtime(&src, &id, restart_existing)?;
    let SkinRuntimeAction::Applied(message) = runtime_action else {
        let SkinRuntimeAction::RestartRequired(message) = runtime_action else {
            return Err(CodexxError::Config(
                "皮肤运行时返回了无效的应用状态".to_string(),
            ));
        };
        return Ok(SkinActionResult {
            message,
            state: get_skin_center_state_inner()?,
            restart_required: true,
        });
    };
    let persist_result = (|| -> Result<()> {
        copy_theme_files(&src, &current_root()?, &manifest)?;
        write_skin_state(&SkinStateFile {
            current_theme_id: Some(id.clone()),
            updated_at: Some(now_rfc3339()),
        })
    })();
    if let Err(error) = persist_result {
        let _ = pause_skin_runtime();
        return Err(error);
    }
    Ok(SkinActionResult {
        message,
        state: get_skin_center_state_inner()?,
        restart_required: false,
    })
}

pub(crate) fn pause_skin_theme_inner() -> Result<SkinActionResult> {
    let _guard = SKIN_OPERATION_LOCK
        .lock()
        .map_err(|_| CodexxError::Config("皮肤操作锁已损坏，请重启 Codex-X".to_string()))?;
    let SkinRuntimeAction::Paused(message) = pause_skin_runtime()? else {
        return Err(CodexxError::Config(
            "皮肤运行时返回了无效的暂停状态".to_string(),
        ));
    };
    Ok(SkinActionResult {
        message,
        state: get_skin_center_state_inner()?,
        restart_required: false,
    })
}

pub(crate) fn restore_skin_theme_inner(restart_existing: bool) -> Result<SkinActionResult> {
    let _guard = SKIN_OPERATION_LOCK
        .lock()
        .map_err(|_| CodexxError::Config("皮肤操作锁已损坏，请重启 Codex-X".to_string()))?;
    match restore_skin_runtime(restart_existing)? {
        SkinRuntimeAction::Restored(message) => Ok(SkinActionResult {
            message,
            state: get_skin_center_state_inner()?,
            restart_required: false,
        }),
        SkinRuntimeAction::RestartRequired(message) => Ok(SkinActionResult {
            message,
            state: get_skin_center_state_inner()?,
            restart_required: true,
        }),
        _ => Err(CodexxError::Config(
            "皮肤运行时返回了无效的恢复状态".to_string(),
        )),
    }
}

pub(crate) fn import_skin_theme_zip_inner(
    file_name: String,
    bytes: Vec<u8>,
) -> Result<SkinActionResult> {
    let _guard = SKIN_OPERATION_LOCK
        .lock()
        .map_err(|_| CodexxError::Config("皮肤操作锁已损坏，请重启 Codex-X".to_string()))?;
    ensure_builtin_themes()?;
    if !file_name.to_ascii_lowercase().ends_with(".zip") {
        return Err(CodexxError::Config("请选择 .zip 主题包".to_string()));
    }
    if bytes.is_empty() || bytes.len() > MAX_THEME_ZIP_BYTES {
        return Err(CodexxError::Config("主题包不能超过 24MB".to_string()));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| CodexxError::Config(format!("读取主题 ZIP 失败: {e}")))?;
    if archive.is_empty() || archive.len() > MAX_THEME_ARCHIVE_ENTRIES {
        return Err(CodexxError::Config(format!(
            "主题包文件数量必须在 1 到 {MAX_THEME_ARCHIVE_ENTRIES} 之间"
        )));
    }
    let tmp = skins_root()?
        .join("tmp")
        .join(format!("theme-{}", chrono::Local::now().timestamp_millis()));
    ensure_directory(&tmp)?;
    let result = (|| -> Result<String> {
        let mut total_size = 0u64;
        let mut wrapper: Option<String> = None;
        let mut root_layout = false;
        let mut written = HashSet::new();
        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| CodexxError::Config(format!("读取 ZIP 条目失败: {e}")))?;
            if file.is_dir() {
                continue;
            }
            if file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err(CodexxError::Config("主题包不能包含符号链接".to_string()));
            }
            let path = file
                .enclosed_name()
                .map(|p| p.to_path_buf())
                .ok_or_else(|| CodexxError::Config("主题包包含越界路径".to_string()))?;
            let normalized = path.to_string_lossy().replace('\\', "/");
            let parts = normalized.split('/').collect::<Vec<_>>();
            if parts
                .iter()
                .any(|part| part.is_empty() || part.starts_with('.') || *part == "..")
            {
                continue;
            }
            let relative = match parts.as_slice() {
                [name] => {
                    root_layout = true;
                    (*name).to_string()
                }
                [folder, name] if !root_layout => {
                    if let Some(current) = &wrapper {
                        if current != folder {
                            return Err(CodexxError::Config(
                                "主题包只能包含一个顶层目录".to_string(),
                            ));
                        }
                    } else {
                        wrapper = Some((*folder).to_string());
                    }
                    (*name).to_string()
                }
                _ => {
                    return Err(CodexxError::Config(
                        "主题文件必须位于 ZIP 根目录或单一顶层目录".to_string(),
                    ))
                }
            };
            if root_layout && wrapper.is_some() {
                return Err(CodexxError::Config(
                    "主题包不能混用根目录和嵌套目录布局".to_string(),
                ));
            }
            let lower = relative.to_ascii_lowercase();
            if lower != "theme.json"
                && ![".png", ".jpg", ".jpeg", ".webp"]
                    .iter()
                    .any(|suffix| lower.ends_with(suffix))
            {
                continue;
            }
            if !written.insert(lower.clone()) {
                return Err(CodexxError::Config(format!(
                    "主题包包含重复文件: {relative}"
                )));
            }
            total_size += file.size();
            if total_size > MAX_THEME_ZIP_BYTES as u64 {
                return Err(CodexxError::Config("主题包解压后超过 24MB".to_string()));
            }
            let out = tmp.join(relative);
            let mut data = Vec::new();
            let per_file_limit = if lower == "theme.json" {
                MAX_THEME_MANIFEST_BYTES
            } else {
                MAX_THEME_IMAGE_BYTES
            };
            file.by_ref()
                .take(per_file_limit + 1)
                .read_to_end(&mut data)
                .map_err(|e| io_err(&out, e))?;
            if data.len() as u64 > per_file_limit {
                return Err(CodexxError::Config(format!(
                    "主题文件过大: {}",
                    out.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("unknown")
                )));
            }
            atomic_write(&out, &data)?;
        }
        let mut manifest = read_manifest(&tmp)?;
        let mut id = normalize_theme_id(if manifest.id.trim().is_empty() {
            &manifest.name
        } else {
            &manifest.id
        })?;
        if builtin_skin_assets().iter().any(|asset| asset.id == id) {
            id = normalize_theme_id(&format!("imported-{id}"))?;
        }
        manifest.id = id.clone();
        let dest = themes_root()?.join(&id);
        copy_theme_files(&tmp, &dest, &manifest)?;
        Ok(manifest.name)
    })();
    let _ = fs::remove_dir_all(&tmp);
    let name = result?;
    Ok(SkinActionResult {
        message: format!("已导入皮肤主题：{name}"),
        state: get_skin_center_state_inner()?,
        restart_required: false,
    })
}

pub(crate) fn create_skin_theme_from_image_inner(
    file_name: String,
    bytes: Vec<u8>,
) -> Result<SkinActionResult> {
    let _guard = SKIN_OPERATION_LOCK
        .lock()
        .map_err(|_| CodexxError::Config("皮肤操作锁已损坏，请重启 Codex-X".to_string()))?;
    ensure_builtin_themes()?;
    let extension = uploaded_image_extension(&file_name, &bytes)?;
    let name = uploaded_theme_name(&file_name);
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let preferred_id = if name.chars().any(|ch| ch.is_ascii_alphanumeric()) {
        format!("image-{}", sanitize_id(&name))
    } else {
        format!("image-{}", &digest[..10])
    };
    let preferred_id = &preferred_id[..preferred_id.len().min(MAX_THEME_ID_BYTES)];
    let root = themes_root()?;
    let id = unique_theme_id_in(&root, &preferred_id)?;
    let image = format!("background.{extension}");
    let manifest = SkinThemeManifest {
        schema_version: 1,
        id: id.clone(),
        name: name.clone(),
        brand_subtitle: String::new(),
        tagline: String::new(),
        project_prefix: String::new(),
        project_label: String::new(),
        status_text: String::new(),
        quote: String::new(),
        image: image.clone(),
        appearance: Some("auto".to_string()),
        surface_opacity: Some(IMAGE_THEME_SURFACE_OPACITY),
        art: Some(SkinThemeArt {
            safe_area: Some("auto".to_string()),
            task_mode: Some("auto".to_string()),
            ..SkinThemeArt::default()
        }),
        colors: None,
        extra: BTreeMap::new(),
    };
    validate_manifest_fields(&manifest)?;

    let tmp = skins_root()?.join("tmp").join(format!(
        "image-theme-{}-{}",
        std::process::id(),
        chrono::Local::now().timestamp_millis()
    ));
    ensure_directory(&tmp)?;
    let result = (|| -> Result<()> {
        atomic_write(&tmp.join(&image), &bytes)?;
        write_manifest(&tmp, &manifest)?;
        let staged_manifest = read_manifest(&tmp)?;
        copy_theme_files(&tmp, &root.join(&id), &staged_manifest)
    })();
    if tmp.exists() {
        let _ = fs::remove_dir_all(&tmp);
    }
    result?;
    Ok(SkinActionResult {
        message: format!("已从图片创建皮肤：{name}"),
        state: get_skin_center_state_inner()?,
        restart_required: false,
    })
}

pub(crate) fn export_skin_theme_inner(
    id: String,
    destination_path: String,
) -> Result<SkinExportResult> {
    let _guard = SKIN_OPERATION_LOCK
        .lock()
        .map_err(|_| CodexxError::Config("皮肤操作锁已损坏，请重启 Codex-X".to_string()))?;
    ensure_builtin_themes()?;
    let id = normalize_theme_id(&id)?;
    let src = themes_root()?.join(&id);
    if !src.is_dir() {
        return Err(CodexxError::Config(format!("没有找到主题: {id}")));
    }
    let manifest = read_manifest(&src)?;
    let path = normalize_export_destination(&destination_path)?;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let manifest_text = serde_json::to_string_pretty(&manifest)
        .map_err(|e| CodexxError::Config(format!("序列化主题失败: {e}")))?;
    zip.start_file("theme.json", options)
        .map_err(|e| CodexxError::Config(format!("写入主题包失败: {e}")))?;
    zip.write_all(format!("{manifest_text}\n").as_bytes())
        .map_err(|e| io_err(&path, e))?;
    zip.start_file(&manifest.image, options)
        .map_err(|e| CodexxError::Config(format!("写入主题图片失败: {e}")))?;
    let image = src.join(&manifest.image);
    let mut image_file = fs::File::open(&image).map_err(|e| io_err(&image, e))?;
    std::io::copy(&mut image_file, &mut zip).map_err(|e| io_err(&image, e))?;
    let bytes = zip
        .finish()
        .map_err(|e| CodexxError::Config(format!("完成主题包失败: {e}")))?
        .into_inner();
    fs::write(&path, bytes).map_err(|error| io_err(&path, error))?;
    Ok(SkinExportResult {
        path: path.to_string_lossy().to_string(),
        message: format!("已导出主题包：{}", path.display()),
    })
}

#[cfg(test)]
#[path = "skin_tests.rs"]
mod tests;
