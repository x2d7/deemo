//! Локальное сохранение результатов (лучший счёт на песню+сложность).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone)]
pub struct SongResult {
    pub score: u64,
    /// Легаси achievement (из счёта); оставлен для старых файлов и ранга.
    pub accuracy: f32,
    /// Настоящая точность попаданий (osu-style). У старых записей поля нет —
    /// её считаем из counts через [`SongResult::real_accuracy`].
    #[serde(default)]
    pub hit_accuracy: Option<f32>,
    pub max_combo: u32,
    pub perfect: u32,
    pub good: u32,
    pub miss: u32,
    pub rank: String,
    pub full_combo: bool,
    pub timestamp: u64,
}

impl SongResult {
    pub fn from_play(play: &crate::play::Play) -> SongResult {
        let acc = play.accuracy();
        let rank = rank_for(acc, play.counts[2] == 0).to_string();
        SongResult {
            score: play.score,
            accuracy: acc,
            hit_accuracy: Some(play.hit_accuracy()),
            max_combo: play.max_combo,
            perfect: play.counts[0],
            good: play.counts[1],
            miss: play.counts[2],
            rank,
            full_combo: play.counts[2] == 0,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}

impl SongResult {
    /// Точность для показа: сохранённая hit accuracy или вычисленная из
    /// counts (старые results.json). Никогда не achievement.
    pub fn real_accuracy(&self) -> f32 {
        self.hit_accuracy
            .unwrap_or_else(|| hit_accuracy_from_counts(self.perfect, self.good, self.miss))
    }
}

/// Точность попаданий из счётчиков: `(perfect + 0.5·good) / total · 100`.
pub fn hit_accuracy_from_counts(perfect: u32, good: u32, miss: u32) -> f32 {
    let total = perfect + good + miss;
    if total == 0 {
        0.0
    } else {
        ((perfect as f64 + 0.5 * good as f64) / total as f64 * 100.0) as f32
    }
}

/// Ранг в стиле osu! по точности.
pub fn rank_for(accuracy: f32, full_combo: bool) -> &'static str {
    if accuracy >= 100.0 {
        "SS"
    } else if accuracy >= 95.0 {
        if full_combo { "S" } else { "A" }
    } else if accuracy >= 90.0 {
        "A"
    } else if accuracy >= 80.0 {
        "B"
    } else if accuracy >= 70.0 {
        "C"
    } else {
        "D"
    }
}

const PATH: &str = "data/results.json";

/// Запись одного плея (для истории ранкинга).
#[derive(Serialize, Deserialize, Clone)]
pub struct PlayRecord {
    pub song: String,
    pub diff: String,
    pub score: u64,
    /// Легаси achievement; для показа используйте [`PlayRecord::real_accuracy`].
    pub accuracy: f32,
    /// Настоящая точность попаданий; None в старых results.json.
    #[serde(default)]
    pub hit_accuracy: Option<f32>,
    pub max_combo: u32,
    pub perfect: u32,
    pub good: u32,
    pub miss: u32,
    pub rank: String,
    pub full_combo: bool,
    pub timestamp: u64,
    #[serde(default)]
    pub speed: f32,
}

impl PlayRecord {
    pub fn real_accuracy(&self) -> f32 {
        self.hit_accuracy
            .unwrap_or_else(|| hit_accuracy_from_counts(self.perfect, self.good, self.miss))
    }
}

/// Дата из unix-времени: «ДД.ММ ЧЧ:ММ» (UTC).
pub fn fmt_date(ts: u64) -> String {
    let days = (ts / 86400) as i64;
    let secs = ts % 86400;
    let hh = secs / 3600;
    let mm = (secs % 3600) / 60;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{d:02}.{m:02} {hh:02}:{mm:02}")
}

#[derive(Serialize, Deserialize, Default)]
pub struct Results {
    /// ключ = song_id:diff → лучший счёт
    #[serde(default)]
    pub map: HashMap<String, SongResult>,
    /// история плеев (все попытки, свежие в конце)
    #[serde(default)]
    pub history: Vec<PlayRecord>,
}

impl Results {
    pub fn load() -> Results {
        let text = std::fs::read_to_string(PATH).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        // новый формат {map, history}; старый — плоская map лучших
        if v.get("map").is_some() || v.get("history").is_some() {
            serde_json::from_value(v).unwrap_or_default()
        } else {
            let map = serde_json::from_value(v).unwrap_or_default();
            Results { map, history: Vec::new() }
        }
    }

    pub fn save(&self) {
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(PATH, t);
        }
    }

    pub fn key(song: &str, diff: &str) -> String {
        format!("{song}:{diff}")
    }

    /// Записать плей в историю + обновить лучший счёт. Возвращает true если новый рекорд.
    pub fn record(&mut self, song: &str, diff: &str, play: &crate::play::Play, speed: f32) -> bool {
        let r = SongResult::from_play(play);
        self.history.push(PlayRecord {
            song: song.to_string(),
            diff: diff.to_string(),
            score: r.score,
            accuracy: r.accuracy,
            hit_accuracy: r.hit_accuracy,
            max_combo: r.max_combo,
            perfect: r.perfect,
            good: r.good,
            miss: r.miss,
            rank: r.rank.clone(),
            full_combo: r.full_combo,
            timestamp: r.timestamp,
            speed,
        });
        if self.history.len() > 1000 {
            let n = self.history.len() - 1000;
            self.history.drain(..n);
        }
        let k = Self::key(song, diff);
        let better = self.map.get(&k).map(|old| r.score > old.score).unwrap_or(true);
        if better {
            self.map.insert(k, r);
        }
        self.save();
        better
    }

    pub fn best(&self, song: &str, diff: &str) -> Option<&SongResult> {
        self.map.get(&Self::key(song, diff))
    }

    /// История плеев песни по конкретной сложности (свежие первыми).
    pub fn history_for_diff(&self, song: &str, diff: &str, limit: usize) -> Vec<&PlayRecord> {
        let mut v: Vec<&PlayRecord> = self
            .history
            .iter()
            .filter(|r| r.song == song && r.diff == diff)
            .collect();
        v.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        v.truncate(limit);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::{Chart, Note, NoteKind};

    fn chart_with(times: &[f32]) -> Chart {
        Chart {
            speed: 6.0,
            notes: times
                .iter()
                .map(|&time| Note {
                    time,
                    pos: 0.0,
                    size: 1.0,
                    speed: 1.0,
                    kind: NoteKind::Normal,
                    hold: 0.0,
                    sounds: vec![],
                })
                .collect(),
            links: vec![],
        }
    }

    #[test]
    fn hit_accuracy_from_counts_matches_play() {
        let chart = chart_with(&[1.0, 2.0, 3.0]);
        let mut play = crate::play::Play::new(&chart);
        play.counts = [2, 1, 7];
        assert!((hit_accuracy_from_counts(2, 1, 7) - play.hit_accuracy()).abs() < 1e-6);
        assert_eq!(hit_accuracy_from_counts(0, 0, 0), 0.0);
    }

    #[test]
    fn old_json_without_hit_accuracy_is_computed_from_counts() {
        let old: SongResult = serde_json::from_str(
            r#"{"score":900000,"accuracy":97.5,"max_combo":100,"perfect":80,"good":10,"miss":10,"rank":"A","full_combo":false,"timestamp":1}"#,
        )
        .unwrap();
        let want = (80.0 + 0.5 * 10.0) / 100.0 * 100.0;
        assert!((old.real_accuracy() - want).abs() < 1e-6);
        let old_play: PlayRecord = serde_json::from_str(
            r#"{"song":"s","diff":"hard","score":1,"accuracy":90.0,"max_combo":1,"perfect":9,"good":2,"miss":9,"rank":"A","full_combo":false,"timestamp":1}"#,
        )
        .unwrap();
        let want_play = (9.0 + 1.0) / 20.0 * 100.0;
        assert!((old_play.real_accuracy() - want_play).abs() < 1e-6);
    }

    #[test]
    fn new_saves_round_trip_hit_accuracy() {
        let chart = chart_with(&[1.0]);
        let mut play = crate::play::Play::new(&chart);
        play.counts = [1, 0, 0];
        let r = SongResult::from_play(&play);
        assert!((r.hit_accuracy.unwrap() - 100.0).abs() < 1e-6);
        assert!((r.real_accuracy() - 100.0).abs() < 1e-6);
        let text = serde_json::to_string(&r).unwrap();
        let back: SongResult = serde_json::from_str(&text).unwrap();
        assert_eq!(back.hit_accuracy, r.hit_accuracy);
        assert!((back.real_accuracy() - 100.0).abs() < 1e-6);
    }
}
