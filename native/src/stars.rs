//! Кэш звёзд сложности с серверного каталога (`GET /api/packs`).
//!
//! На диске (`pack.json`) есть только `levels`; `stars` живут в каталоге
//! сервера и в анализе чартов. Библиотека ключуется `song_id:diff` — тот же
//! ключ, что у лучших результатов, поэтому кэш можно положить в
//! `data/stars.json` и читать при старте без сети.
//!
//! Обновление — фоновым потоком (см. `App::request_stars`): каталог весит
//! сотни килобайт, кадр его не ждёт. Ошибки сети молчат: остаётся прошлый
//! кэш или подпись уровня вместо звезды.

use std::collections::HashMap;
use std::time::Duration;

pub const PATH: &str = "data/stars.json";

/// `song_id:diff` → звезда сложности (0..15, одна десятка).
pub type StarMap = HashMap<String, f32>;

/// Ключ карты. Формат совпадает с [`crate::results::Results::key`].
pub fn key(song: &str, diff: &str) -> String {
    format!("{song}:{diff}")
}

/// Прочитать кэш; нет файла/битый JSON — пустая карта (оффлайн-фолбэк).
pub fn load() -> StarMap {
    std::fs::read_to_string(PATH)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Записать кэш; ошибки молчат — кэш не важнее игры.
pub fn save(map: &StarMap) {
    if let Some(dir) = std::path::Path::new(PATH).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(PATH, text);
    }
}

/// Звезда карты, если она известна и правдоподобна.
pub fn star(map: &StarMap, song: &str, diff: &str) -> Option<f32> {
    map.get(&key(song, diff))
        .copied()
        .filter(|s| s.is_finite() && *s > 0.0)
}

/// `★ 9.6` — одна десятка, как на сайте.
pub fn label(star: f32) -> String {
    format!("★ {star:.1}")
}

/// Разбор ответа `/api/packs` в карту `song_id:diff` → star.
///
/// Берём только положительные значения: у старых/ручных паков анализа нет,
/// и `0` не должен подменять уровень в интерфейсе.
pub fn parse_catalog(v: &serde_json::Value) -> StarMap {
    let mut out = StarMap::new();
    let Some(packs) = v.get("packs").and_then(|p| p.as_array()) else {
        return out;
    };
    for pack in packs {
        let Some(id) = pack.get("id").and_then(|s| s.as_str()) else {
            continue;
        };
        if !crate::pack::safe_id(id) {
            continue;
        }
        let Some(charts) = pack.get("charts").and_then(|c| c.as_array()) else {
            continue;
        };
        for chart in charts {
            let Some(diff) = chart.get("diff").and_then(|d| d.as_str()) else {
                continue;
            };
            let Some(stars) = chart.get("stars").and_then(|s| s.as_f64()) else {
                continue;
            };
            if stars.is_finite() && stars > 0.0 {
                out.insert(key(id, diff), stars as f32);
            }
        }
    }
    out
}

/// Скачать каталог и собрать карту. Публичный эндпоинт, без токена.
/// Ошибка — строка для лога фонового потока, в UI не показывается.
pub fn fetch(server: &str) -> Result<StarMap, String> {
    let url = format!("{}/api/packs", server.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .build();
    let v: serde_json::Value = agent
        .get(&url)
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    let map = parse_catalog(&v);
    if map.is_empty() {
        return Err("catalog has no chart stars".into());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каталог с несколькими сложностями одной песни: ключи song:diff,
    /// нули и мусор не попадают.
    #[test]
    fn catalog_parses_per_chart_stars() {
        let v = serde_json::json!({
            "count": 2,
            "packs": [
                {"id": "ice10", "charts": [
                    {"diff": "easy", "stars": 1.409999966621399},
                    {"diff": "hard", "stars": 7.269999980926514},
                    {"diff": "extra", "stars": 0.0},
                ]},
                {"id": "../bad", "charts": [{"diff": "hard", "stars": 9.0}]},
                {"id": "no_charts"},
            ]
        });
        let map = parse_catalog(&v);
        assert_eq!(map.len(), 2);
        assert!((star(&map, "ice10", "easy").unwrap() - 1.41).abs() < 0.001);
        assert!((star(&map, "ice10", "hard").unwrap() - 7.27).abs() < 0.001);
        assert_eq!(star(&map, "ice10", "extra"), None, "zero is not a star");
        assert_eq!(star(&map, "../bad", "hard"), None, "unsafe id skipped");
        assert_eq!(star(&map, "ice10", "normal"), None);
        assert_eq!(label(7.2699999), "★ 7.3");
        assert_eq!(label(9.0), "★ 9.0");
    }

    #[test]
    fn malformed_catalog_is_empty_not_a_panic() {
        assert!(parse_catalog(&serde_json::json!({})).is_empty());
        assert!(parse_catalog(&serde_json::json!({"packs": "nope"})).is_empty());
    }
}
