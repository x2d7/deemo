//! Локальные (на карту) оффсеты ввода: мышь и планшет.
//!
//! Файл `data/offsets.json` — плоская карта:
//! ```json
//! { "song_id:hard": { "mouse_ms": 7.0, "tablet_ms": -3.0 } }
//! ```
//! Это НЕ глобальные настройки (`data/settings.json`): кнопка на экране
//! результата пишет сюда, а при игре эффективный оффсет = глобальная настройка
//! + локальная поправка именно этой карты (см. `App::input_offsets`).
//!
//! Рекомендация считается по ОДНОМУ плею: `local + mean tap error` (мс),
//! clamp ±[`CLAMP_MS`]. Никакой истории, накопления между плеями и флага
//! «визуального игрока» здесь нет: прежняя эвристика
//! (`results::recommended_offset`) читала `.work/offset_history.json`, липла к
//! флагу visual и при нём возвращала `current` без поправки — из-за этого
//! кнопка всегда показывала «+0», выбрасывая измеренные −10.6 мс.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Устройство: мышь.
pub const MOUSE: usize = 0;
/// Устройство: планшет (стилус).
pub const TABLET: usize = 1;
/// Предел локальной поправки (мс) — как у прежней раздельной калибровки.
pub const CLAMP_MS: f32 = 80.0;
/// Сколько карт максимум держим в файле (защита от роста).
pub const MAX_MAPS: usize = 2000;

const PATH: &str = "data/offsets.json";

/// Локальные оффсеты одной карты по устройствам.
#[derive(Serialize, Deserialize, Clone, Copy, Default, Debug, PartialEq)]
pub struct MapOffset {
    #[serde(default)]
    pub mouse_ms: f32,
    #[serde(default)]
    pub tablet_ms: f32,
}

impl MapOffset {
    pub fn get(&self, device: usize) -> f32 {
        if device == TABLET { self.tablet_ms } else { self.mouse_ms }
    }

    pub fn set(&mut self, device: usize, ms: f32) {
        if device == TABLET {
            self.tablet_ms = clamp(ms);
        } else {
            self.mouse_ms = clamp(ms);
        }
    }

    fn is_zero(&self) -> bool {
        self.mouse_ms == 0.0 && self.tablet_ms == 0.0
    }

    fn sanitize(&mut self) {
        self.mouse_ms = clamp(self.mouse_ms);
        self.tablet_ms = clamp(self.tablet_ms);
    }
}

/// Привести значение к ±[`CLAMP_MS`]; NaN/inf → 0.
fn clamp(ms: f32) -> f32 {
    if !ms.is_finite() {
        return 0.0;
    }
    ms.clamp(-CLAMP_MS, CLAMP_MS)
}

/// Хранилище локальных оффсетов: ключ `"song:diff"` → [`MapOffset`].
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(transparent)]
pub struct MapOffsets {
    maps: HashMap<String, MapOffset>,
}

impl MapOffsets {
    pub fn load() -> MapOffsets {
        Self::load_from(Path::new(PATH))
    }

    pub fn save(&self) {
        self.save_to(Path::new(PATH));
    }

    /// Ключ карты — как у `results::Results`: `song_id:diff`.
    pub fn key(song: &str, diff: &str) -> String {
        format!("{song}:{diff}")
    }

    /// Применённый локальный оффсет карты (0, если записи нет).
    pub fn local_ms(&self, song: &str, diff: &str, device: usize) -> f32 {
        self.maps
            .get(&Self::key(song, diff))
            .map(|m| m.get(device))
            .unwrap_or(0.0)
    }

    /// Записать локальный оффсет карты (clamp ±[`CLAMP_MS`]). Возвращает
    /// сохранённое значение. Нулевое значение не создаёт пустую запись.
    pub fn set_local(&mut self, song: &str, diff: &str, device: usize, ms: f32) -> f32 {
        let ms = clamp(ms);
        let key = Self::key(song, diff);
        if ms == 0.0 && !self.maps.contains_key(&key) {
            return 0.0;
        }
        self.maps.entry(key).or_default().set(device, ms);
        self.trim();
        ms
    }

    /// Рекомендация локального оффсета по ОДНОМУ плею:
    /// `текущий локальный + средняя ошибка тапов этого плея`, clamp ±80.
    ///
    /// `mean_err_ms = None` — у устройства не было тапов в этом прохождении:
    /// рекомендации нет (`—` в UI), запись карты не меняется.
    pub fn recommend(
        &self,
        song: &str,
        diff: &str,
        device: usize,
        mean_err_ms: Option<f32>,
    ) -> Option<f32> {
        let mean = mean_err_ms?;
        if !mean.is_finite() {
            return None;
        }
        // Смещение прибавляется к моменту тапа (t_judged = t + offset), а
        // ошибка пишется как t_judged - note. Значит остаточную среднюю
        // ошибку надо ВЫЧЕСТЬ: иначе оффсет поедет в обратную сторону.
        Some(clamp(self.local_ms(song, diff, device) - mean))
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.maps.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.maps.is_empty()
    }

    /// Не даём файлу расти бесконечно: сначала выкидываем пустые записи, потом
    /// (если всё ещё много) — самые первые по алфавиту. Детерминированно и без
    /// метаданных времени в формате.
    fn trim(&mut self) {
        if self.maps.len() <= MAX_MAPS {
            return;
        }
        self.maps.retain(|_, v| !v.is_zero());
        if self.maps.len() <= MAX_MAPS {
            return;
        }
        let mut keys: Vec<String> = self.maps.keys().cloned().collect();
        keys.sort();
        for k in keys {
            if self.maps.len() <= MAX_MAPS {
                break;
            }
            self.maps.remove(&k);
        }
    }

    // ===== файл =====

    fn load_from(path: &Path) -> MapOffsets {
        let Ok(text) = std::fs::read_to_string(path) else {
            return MapOffsets::default();
        };
        Self::parse(&text)
    }

    /// Терпимый разбор: битый файл = пусто, частично битый — валидные записи
    /// сохраняются (неизвестные/неправильные поля пропускаются).
    fn parse(text: &str) -> MapOffsets {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
            return MapOffsets::default();
        };
        let Some(obj) = v.as_object() else {
            return MapOffsets::default();
        };
        let mut maps = HashMap::new();
        for (k, val) in obj {
            let Ok(mut m) = serde_json::from_value::<MapOffset>(val.clone()) else {
                continue;
            };
            m.sanitize();
            if k.contains(':') {
                maps.insert(k.clone(), m);
            }
        }
        let mut out = MapOffsets { maps };
        out.trim();
        out
    }

    /// Атомарная запись: temp-файл рядом + rename. При ошибке temp убирается,
    /// старый файл остаётся целым.
    fn save_to(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        let Ok(text) = serde_json::to_string_pretty(self) else {
            return;
        };
        let tmp = tmp_path(path);
        if std::fs::write(&tmp, text).is_err() {
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            eprintln!("offsets: save failed: {e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn temp_dir() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("deemo-offsets-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Поправка этого плея прибавляется к локальному значению карты:
    /// stored −3 + средняя ошибка +5 → +2.
    #[test]
    fn recommendation_is_single_play_delta() {
        let mut m = MapOffsets::default();
        // тапы на 5 мс позже -> оффсет на 5 мс меньше
        m.set_local("song", "hard", MOUSE, -3.0);
        assert_eq!(m.recommend("song", "hard", MOUSE, Some(5.0)), Some(-8.0));
        // тапы раньше -> оффсет больше
        m.set_local("song", "hard", TABLET, -3.0);
        assert_eq!(m.recommend("song", "hard", TABLET, Some(-5.0)), Some(2.0));
    }

    /// Без тапов в этом прохождении рекомендации нет (в UI «—»), запись не трогается.
    #[test]
    fn no_taps_means_no_suggestion() {
        let mut m = MapOffsets::default();
        m.set_local("song", "hard", TABLET, 4.0);
        assert_eq!(m.recommend("song", "hard", MOUSE, None), None);
        assert_eq!(m.recommend("song", "hard", TABLET, None), None);
        // даже когда у другого устройства данные есть
        assert_eq!(m.recommend("song", "hard", TABLET, Some(1.0)), Some(3.0));
        assert_eq!(m.local_ms("song", "hard", TABLET), 4.0);
        assert_eq!(m.local_ms("song", "hard", MOUSE), 0.0);
    }

    /// Регресс на «всегда +0»: измеренные −10.6 мс должны давать −10.6,
    /// а не current (0) из-за липкого флага «визуального игрока».
    #[test]
    fn sticky_visual_flag_is_gone() {
        let m = MapOffsets::default();
        // тапы на 10.6 мс раньше -> оффсет должен вырасти на 10.6
        let rec = m.recommend("song", "hard", MOUSE, Some(-10.6)).unwrap();
        assert!((rec - 10.6).abs() < 1e-4, "got {rec}");
        assert_ne!(rec, 0.0, "measured mean must never be discarded");
        // с уже применённой поправкой карты: 35 - (-10.6) = 45.6
        let mut m = MapOffsets::default();
        m.set_local("song", "hard", MOUSE, 35.0);
        let rec = m.recommend("song", "hard", MOUSE, Some(-10.6)).unwrap();
        assert!((rec - 45.6).abs() < 1e-4, "got {rec}");
    }

    #[test]
    fn clamped_to_80_ms() {
        let mut m = MapOffsets::default();
        // ошибка +500 мс -> поправка -500 -> кламп -80
        assert_eq!(m.recommend("s", "h", MOUSE, Some(500.0)), Some(-CLAMP_MS));
        assert_eq!(m.recommend("s", "h", MOUSE, Some(-500.0)), Some(CLAMP_MS));
        // база тоже зажимается: 200 в карту не попадёт
        assert_eq!(m.set_local("s", "h", MOUSE, 200.0), CLAMP_MS);
        assert_eq!(m.recommend("s", "h", MOUSE, Some(-20.0)), Some(CLAMP_MS));
        assert_eq!(m.local_ms("s", "h", MOUSE), CLAMP_MS);
        // 75 - (-20) = 95 -> 80
        m.set_local("s", "h", MOUSE, 75.0);
        assert_eq!(m.recommend("s", "h", MOUSE, Some(-20.0)), Some(CLAMP_MS));
        // NaN не превращается в «есть предложение»
        assert_eq!(m.recommend("s", "h", MOUSE, Some(f32::NAN)), None);
    }

    /// Мышь и планшет независимы: кнопка пишет оба, но значения не смешиваются.
    #[test]
    fn devices_are_independent() {
        let mut m = MapOffsets::default();
        m.set_local("song", "hard", MOUSE, 7.0);
        assert_eq!(m.local_ms("song", "hard", MOUSE), 7.0);
        assert_eq!(m.local_ms("song", "hard", TABLET), 0.0);
        m.set_local("song", "hard", TABLET, -3.0);
        assert_eq!(m.local_ms("song", "hard", MOUSE), 7.0);
        assert_eq!(m.local_ms("song", "hard", TABLET), -3.0);
        // ошибка +1 мс -> поправка -1: 7-1=6, -3-1=-4
        assert_eq!(m.recommend("song", "hard", MOUSE, Some(1.0)), Some(6.0));
        assert_eq!(m.recommend("song", "hard", TABLET, Some(1.0)), Some(-4.0));
    }

    /// Разные сложности одной песни — разные карты.
    #[test]
    fn maps_are_isolated() {
        let mut m = MapOffsets::default();
        m.set_local("song", "easy", MOUSE, 12.0);
        m.set_local("song", "hard", MOUSE, -5.0);
        m.set_local("other", "hard", TABLET, 3.0);
        assert_eq!(m.local_ms("song", "easy", MOUSE), 12.0);
        assert_eq!(m.local_ms("song", "hard", MOUSE), -5.0);
        assert_eq!(m.local_ms("other", "hard", MOUSE), 0.0);
        assert_eq!(m.local_ms("song", "easy", TABLET), 0.0);
        assert_eq!(m.local_ms("other", "hard", TABLET), 3.0);
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn json_roundtrip() {
        let dir = temp_dir();
        let path = dir.join("offsets.json");
        let mut m = MapOffsets::default();
        m.set_local("a", "easy", MOUSE, 7.0);
        m.set_local("a", "easy", TABLET, -3.5);
        m.set_local("b", "hard", MOUSE, 1.5);
        m.save_to(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        // формат: плоская карта song:diff -> {mouse_ms, tablet_ms}
        assert!(text.contains("\"a:easy\""), "{text}");
        assert!(text.contains("\"mouse_ms\""), "{text}");
        assert!(text.contains("\"tablet_ms\""), "{text}");
        let back = MapOffsets::load_from(&path);
        assert_eq!(back.len(), 2);
        assert_eq!(back.local_ms("a", "easy", MOUSE), 7.0);
        assert_eq!(back.local_ms("a", "easy", TABLET), -3.5);
        assert_eq!(back.local_ms("b", "hard", MOUSE), 1.5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_tolerated() {
        // целиком мусор — пусто, без паники
        assert!(MapOffsets::parse("not json at all").is_empty());
        assert!(MapOffsets::parse("[]").is_empty());
        assert!(MapOffsets::parse("").is_empty());
        // частично битый: валидные записи остаются
        let m = MapOffsets::parse(
            r#"{"ok:hard":{"mouse_ms":4.0,"tablet_ms":1.0},
                "bad:hard":{"mouse_ms":"x"},
                "old":{"mouse_ms":9.0}}"#,
        );
        assert_eq!(m.local_ms("ok", "hard", MOUSE), 4.0);
        assert_eq!(m.local_ms("bad", "hard", MOUSE), 0.0);
        assert_eq!(m.len(), 1, "keys without ':diff' are dropped");
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = temp_dir();
        assert!(MapOffsets::load_from(&dir.join("nope.json")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Атомарная запись: после save рядом не остаётся temp-файла, а старый
    /// файл заменён целиком (rename).
    #[test]
    fn atomic_save_leaves_no_temp_file() {
        let dir = temp_dir();
        let path = dir.join("offsets.json");
        let mut m = MapOffsets::default();
        m.set_local("a", "hard", MOUSE, 2.0);
        m.save_to(&path);
        assert!(!tmp_path(&path).exists(), "temp file must be renamed away");
        assert!(path.exists());
        // вторая запись не ломает файл
        m.set_local("b", "normal", TABLET, -1.0);
        m.save_to(&path);
        assert!(!tmp_path(&path).exists());
        let back = MapOffsets::load_from(&path);
        assert_eq!(back.len(), 2);
        assert_eq!(back.local_ms("b", "normal", TABLET), -1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn map_count_is_capped() {
        let mut m = MapOffsets::default();
        for i in 0..(MAX_MAPS + 25) {
            m.set_local(&format!("s{i:04}"), "hard", MOUSE, 1.0);
        }
        assert_eq!(m.len(), MAX_MAPS);
        // нулевые записи не создаются и не занимают место
        let mut m = MapOffsets::default();
        m.set_local("x", "hard", MOUSE, 0.0);
        assert!(m.is_empty());
    }
}
