//! Онлайн: аутентификация + рекорды на сервере Deemo.
//! Конфиг и токен хранятся в data/auth.json. Сервер по умолчанию — публичный.

use serde::{Deserialize, Serialize};
use std::time::Duration;

const AUTH_PATH: &str = "data/auth.json";
/// Публичный сервер чартов/рекордов (тот же, что отдаёт сайт).
pub const DEFAULT_SERVER: &str = "http://199.247.22.102";
/// Прежние значения по умолчанию: их в data/auth.json надо поднять до
/// DEFAULT_SERVER, иначе игра так и будет стучаться в localhost.
const LEGACY_SERVERS: &[&str] = &["", "http://127.0.0.1:8090", "http://localhost:8090", "127.0.0.1:8090"];

#[derive(Serialize, Deserialize, Clone)]
pub struct AuthState {
    #[serde(default = "default_server")]
    pub server: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
}

impl Default for AuthState {
    fn default() -> Self {
        AuthState { server: default_server(), token: None, nickname: None }
    }
}

fn default_server() -> String {
    DEFAULT_SERVER.to_string()
}

/// Старые установки хранят в auth.json локальный адрес (или пусто) — переводим
/// их на публичный сервер. Вручную заданные адреса не трогаем.
fn normalize_server(state: &mut AuthState) {
    let s = state.server.trim();
    if LEGACY_SERVERS.iter().any(|l| l.eq_ignore_ascii_case(s)) {
        state.server = default_server();
    }
}

/// Рекорд с сервера (лидерборд / мои скоры).
#[derive(Deserialize, Clone)]
pub struct OnlineRecord {
    pub nickname: String,
    pub song: String,
    pub diff: String,
    pub score: u64,
    pub accuracy: f32,
    pub max_combo: u32,
    pub perfect: u32,
    pub good: u32,
    pub miss: u32,
    pub rank: String,
    pub full_combo: bool,
    #[serde(default)]
    pub speed: f32,
    pub timestamp: u64,
}

#[derive(Clone)]
pub struct Online {
    pub state: AuthState,
}

impl Online {
    pub fn load() -> Online {
        let mut state: AuthState = std::fs::read_to_string(AUTH_PATH)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        normalize_server(&mut state);
        Online { state }
    }

    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(&self.state) {
            let _ = std::fs::write(AUTH_PATH, s);
        }
    }

    pub fn logged_in(&self) -> bool {
        self.state.token.is_some()
    }

    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(6))
            .build()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.state.server.trim_end_matches('/'), path)
    }

    /// Регистрация или вход. Возвращает никнейм при успехе.
    pub fn auth(&mut self, nickname: &str, password: &str, register: bool) -> Result<String, String> {
        let path = if register { "/api/register" } else { "/api/login" };
        let body = serde_json::json!({"nickname": nickname, "password": password});
        let resp = self
            .agent()
            .post(&self.url(path))
            .send_json(body)
            .map_err(|e| fmt_err(e))?;
        let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
        let token = v["token"].as_str().ok_or("no token in server response")?.to_string();
        let nick = v["nickname"].as_str().unwrap_or(nickname).to_string();
        self.state.token = Some(token);
        self.state.nickname = Some(nick.clone());
        self.save();
        Ok(nick)
    }

    pub fn logout(&mut self) {
        self.state.token = None;
        self.state.nickname = None;
        self.save();
    }

    /// Тело POST /api/scores — прежние поля плюс аддитивная статистика по
    /// рукам `hands` (`{"mouse": {...}, "tablet": {...}}`), по которой сервер
    /// считает pp. Старые серверы лишнее поле игнорируют.
    ///
    /// `accuracy` — НАСТОЯЩАЯ точность попаданий (проценты, 2 знака, osu-стиль,
    /// её показывает сайт), `achievement` — прежнее значение `Play::accuracy()`
    /// (achievement rate из счёта с комбо-бонусом). `score` остаётся в теле:
    /// сервер его хранит, хотя сайт для рейтинга берёт accuracy/pp.
    pub fn score_body(
        song: &str,
        diff: &str,
        play: &crate::play::Play,
        speed: f32,
        hands: &crate::play::Hands,
    ) -> serde_json::Map<String, serde_json::Value> {
        let hit_acc = round2(play.hit_accuracy() as f64);
        let achievement = play.accuracy();
        let body = serde_json::json!({
            "song": song, "diff": diff,
            "score": play.score,
            "accuracy": hit_acc,
            "achievement": achievement,
            "max_combo": play.max_combo,
            "perfect": play.counts[0], "good": play.counts[1], "miss": play.counts[2],
            // ранг по-прежнему из achievement — как в самой игре
            "rank": crate::results::rank_for(achievement, play.counts[2] == 0),
            "full_combo": play.counts[2] == 0,
            "speed": speed,
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            "hands": hands,
        });
        body.as_object().cloned().unwrap_or_default()
    }

    /// Запись для очереди отправки: тело рекорда + сервер и токен, под которыми
    /// он сыгран. `None` — пользователь не вошёл, отправлять некуда.
    pub fn pending_score(
        &self,
        song: &str,
        diff: &str,
        play: &crate::play::Play,
        speed: f32,
        hands: &crate::play::Hands,
    ) -> Option<crate::online_queue::PendingScore> {
        let token = self.state.token.clone()?;
        let body = Self::score_body(song, diff, play, speed, hands);
        Some(crate::online_queue::PendingScore::new(
            &self.state.server,
            &token,
            body,
        ))
    }

    /// Лидерборд песни+сложности (топ игроков).
    pub fn leaderboard(&self, song: &str, diff: &str, limit: usize) -> Result<Vec<OnlineRecord>, String> {
        let url = self.url(&format!("/api/leaderboard?song={song}&diff={diff}&limit={limit}"));
        let resp = self.agent().get(&url).call().map_err(|e| fmt_err(e))?;
        let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
        let recs: Vec<OnlineRecord> = serde_json::from_value(v["leaderboard"].clone()).map_err(|e| e.to_string())?;
        Ok(recs)
    }
}

/// Округление до 2 знаков — так сервер и сайт показывают проценты.
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn fmt_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let v: serde_json::Value = resp.into_json().unwrap_or(serde_json::Value::Null);
            v["error"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("server error {code}"))
        }
        ureq::Error::Transport(t) => format!("connection failed: {t}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_server_is_the_public_one() {
        assert_eq!(DEFAULT_SERVER, "http://199.247.22.102");
        assert_eq!(AuthState::default().server, DEFAULT_SERVER);
        assert!(!DEFAULT_SERVER.contains("127.0.0.1"));
    }

    #[test]
    fn legacy_localhost_server_upgrades_to_public() {
        for legacy in ["", "  ", "http://127.0.0.1:8090", "http://localhost:8090", "127.0.0.1:8090"] {
            let mut st = AuthState { server: legacy.to_string(), ..Default::default() };
            normalize_server(&mut st);
            assert_eq!(st.server, DEFAULT_SERVER, "legacy {legacy:?} must upgrade");
        }
    }

    #[test]
    fn custom_server_is_left_alone() {
        for keep in ["https://deemo.example:8443", "http://199.247.22.102", "http://10.8.1.5:8090"] {
            let mut st = AuthState { server: keep.to_string(), ..Default::default() };
            normalize_server(&mut st);
            assert_eq!(st.server, keep);
        }
    }

    /// Тело записи: прежние поля на месте, `hands` — в формате сервера.
    #[test]
    fn score_body_carries_per_hand_counts() {
        use crate::chart::{Chart, Note, NoteKind};
        use crate::play::{HandCounts, Hands, Play};
        let chart = Chart {
            speed: 6.0,
            notes: vec![Note {
                time: 1.0,
                pos: 0.5,
                size: 1.0,
                speed: 1.0,
                kind: NoteKind::Normal,
                hold: 0.0,
                sounds: vec![],
            }],
            links: vec![],
        };
        let mut play = Play::new(&chart);
        play.score = 987_430;
        play.max_combo = 388;
        play.counts = [412, 18, 6];
        let hands = Hands {
            mouse: HandCounts { perfect: 210, good: 9, miss: 4 },
            tablet: HandCounts { perfect: 202, good: 9, miss: 2 },
        };
        let body = Online::score_body("12thchance", "hard", &play, 4.0, &hands);
        let v = serde_json::Value::Object(body);
        // старые поля не изменились
        assert_eq!(v["song"], "12thchance");
        assert_eq!(v["diff"], "hard");
        assert_eq!(v["score"], 987_430);
        assert_eq!(v["perfect"], 412);
        assert_eq!(v["good"], 18);
        assert_eq!(v["miss"], 6);
        assert_eq!(v["max_combo"], 388);
        assert_eq!(v["full_combo"], false);
        assert_eq!(v["speed"], 4.0);
        // `accuracy` — настоящая точность попаданий: (412 + 0.5·18) / 436
        let hit = (412.0 + 0.5 * 18.0) / 436.0 * 100.0;
        assert!(
            (v["accuracy"].as_f64().unwrap() - hit).abs() < 0.005,
            "accuracy must be the hit accuracy: {} vs {hit}",
            v["accuracy"]
        );
        // `achievement` — прежнее значение из счёта (с комбо-бонусом)
        assert!(
            (v["achievement"].as_f64().unwrap() - 98.74).abs() < 1e-4,
            "achievement must be the score-derived rate: {}",
            v["achievement"]
        );
        // точность попаданий строго ниже achievement при таких счётах
        assert!(v["accuracy"].as_f64().unwrap() < v["achievement"].as_f64().unwrap());
        // аддитивная статистика по рукам
        let h = &v["hands"];
        assert_eq!(h["mouse"]["perfect"], 210);
        assert_eq!(h["mouse"]["good"], 9);
        assert_eq!(h["mouse"]["miss"], 4);
        assert_eq!(h["tablet"]["perfect"], 202);
        assert_eq!(h["tablet"]["good"], 9);
        assert_eq!(h["tablet"]["miss"], 2);
        assert_eq!(
            h["mouse"].as_object().unwrap().len(),
            3,
            "only perfect/good/miss"
        );
    }
}
