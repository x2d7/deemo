//! Папки карт: просто кладёшь папку в data/imported/<id>/ (чарты *.json, аудио, обложка),
//! игра сама подхватывает. Манифест pack.json необязателен (есть авто-определение).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FORMAT_VERSION: u32 = 1;
pub const IMPORT_DIR: &str = "data/imported";

/// Манифест пака (pack.json). Пути внутри пака — относительные.
#[derive(Serialize, Deserialize, Clone)]
pub struct PackManifest {
    #[serde(default = "def_ver")]
    pub format_version: u32,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub composer: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub levels: Vec<String>,
    pub charts: Vec<PackChart>,
    #[serde(default)]
    pub audio: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub duration: f32,
    /// необязательное описание для каталога
    #[serde(default)]
    pub description: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PackChart {
    pub diff: String,
    pub path: String,
}

fn def_ver() -> u32 {
    FORMAT_VERSION
}

/// Безопасное имя папки пака (без слэшей, .., абсолютных путей).
pub fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Список установленных паков (из data/imported/<id>/).
pub fn installed() -> Vec<PackManifest> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(IMPORT_DIR) {
        for e in rd.flatten() {
            let dir = e.path();
            if !dir.is_dir() {
                continue;
            }
            if let Some(m) = manifest_of(&dir) {
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

/// Манифест пака из папки: pack.json, либо авто-определение по файлам.
pub fn manifest_of(dir: &Path) -> Option<PackManifest> {
    // 1) явный pack.json
    if let Ok(s) = std::fs::read_to_string(dir.join("pack.json")) {
        if let Ok(m) = serde_json::from_str::<PackManifest>(&s) {
            return Some(m);
        }
    }
    // 2) авто-определение
    auto_manifest(dir)
}

fn diff_of(name: &str, text: &str) -> String {
    let stem = name.trim_end_matches(".json").to_lowercase();
    for d in ["easy", "normal", "hard", "extra", "expert"] {
        if stem.contains(d) {
            return d.to_string();
        }
    }
    // по содержимому: ищем "difficulty" или кол-во нот
    if text.to_lowercase().contains("\"extra\"") {
        return "extra".into();
    }
    stem
}

/// Автоматически собрать манифест из папки (без pack.json).
/// Чарты — *.json с полем "notes" (в корне или charts/), аудио/обложка/превью — по расширению.
pub fn auto_manifest(dir: &Path) -> Option<PackManifest> {
    let id = dir.file_name()?.to_str()?.to_string();
    if !safe_id(&id) {
        return None;
    }
    // необязательный meta.json для названия/артиста/уровней
    let meta: serde_json::Value = std::fs::read_to_string(dir.join("meta.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    let mut charts: Vec<PackChart> = Vec::new();
    for sub in [dir.to_path_buf(), dir.join("charts")] {
        let Ok(rd) = std::fs::read_dir(&sub) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(fname) = p.file_name().and_then(|x| x.to_str()) else { continue };
            if fname == "pack.json" || fname == "meta.json" {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            if !text.contains("\"notes\"") {
                continue;
            }
            let diff = diff_of(fname, &text);
            let rel = p.strip_prefix(dir).ok()?.to_string_lossy().to_string();
            charts.push(PackChart { diff, path: rel });
        }
    }
    if charts.is_empty() {
        return None;
    }
    // сортировка по стандартному порядку сложностей
    let order = |d: &str| match d {
        "easy" => 0,
        "normal" => 1,
        "hard" => 2,
        "extra" => 3,
        "expert" => 4,
        _ => 5,
    };
    charts.sort_by_key(|c| order(&c.diff));

    // файлы в корне папки
    let mut audio: Option<String> = None;
    let mut preview: Option<String> = None;
    let mut cover: Option<String> = None;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|x| x.to_str()) else { continue };
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
            let n = name.to_lowercase();
            match ext.as_str() {
                "ogg" | "mp3" | "wav" | "flac" => {
                    if n.contains("preview") || n.contains("_pv") {
                        preview = Some(name.to_string());
                    } else if audio.is_none() || n.starts_with("audio") || n.starts_with("song") || n.starts_with("music") {
                        audio = Some(name.to_string());
                    }
                }
                "png" | "jpg" | "jpeg" => {
                    if n.contains("cover") || n.contains("art") || cover.is_none() {
                        cover = Some(name.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    let title = meta["title"].as_str().map(|s| s.to_string()).unwrap_or_else(|| id.clone());
    let composer = meta["composer"].as_str().unwrap_or("").to_string();
    let album = meta["album"].as_str().unwrap_or("Imported").to_string();
    let levels: Vec<String> = meta["levels"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string()).or_else(|| v.as_u64().map(|n| n.to_string()))).collect())
        .unwrap_or_default();
    let duration = meta["duration"].as_f64().unwrap_or(0.0) as f32;

    Some(PackManifest {
        format_version: FORMAT_VERSION,
        id,
        title,
        composer,
        album,
        levels,
        charts,
        audio,
        preview,
        cover,
        duration,
        description: meta["description"].as_str().unwrap_or("").to_string(),
    })
}

pub fn is_installed(id: &str) -> bool {
    Path::new(IMPORT_DIR).join(id).join("pack.json").exists()
}

/// Удалить установленный пак.
pub fn remove(id: &str) -> Result<()> {
    if !safe_id(id) {
        bail!("invalid id");
    }
    let dir = Path::new(IMPORT_DIR).join(id);
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}