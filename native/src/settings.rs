//! Пользовательские настройки (скорость нот, скорость скролла меню и т.п.).
//! Хранятся в data/settings.json рядом с ассетами.
//!
//! Окна судейства Perfect/Good и сырые X краёв планшетов сюда НЕ входят:
//! они зашиты в игру (`play::PERFECT_MS`/`GOOD_MS`, `tablet::GAOMON_X_MAX`/
//! `PARBLO_X_MAX`) и не редактируются из UI. Старые settings.json с этими
//! ключами читаются как обычно — serde просто игнорирует неизвестные поля.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Settings {
    /// скорость падения нот (Deenote NoteFallSpeed, 0.5..9.5)
    #[serde(default = "def_speed")]
    pub speed: f32,
    /// множитель скорости прокрутки меню
    #[serde(default = "def_scroll")]
    pub scroll_speed: f32,
    /// полноэкранный режим
    #[serde(default)]
    pub fullscreen: bool,
    /// во сколько раз уменьшить зону планшета (центр тот же, движений меньше)
    #[serde(default = "def_zoom")]
    pub tablet_zoom: f32,
    /// поправка синхронизации аудио (мс): + если звук отстаёт
    #[serde(default)]
    pub audio_offset_ms: f32,
    /// направляющие линии от нот до линии нажатия
    #[serde(default)]
    pub note_guides: bool,
    /// горизонтальный допуск тапа (мировые ед.): 0.75 = аутентичный Deemo, шире = удобнее на ПК
    #[serde(default = "def_xtol")]
    pub x_tolerance: f32,
    /// мышь: абсолютная позиция курсора (клик по колонке ноты) вместо относительного delta
    #[serde(default)]
    pub mouse_absolute: bool,
    /// «Hide cursor (1D aim)»: в игре скрыть системный курсор в обоих режимах
    /// (relative/absolute); прицел остаётся только по X. Без новых визуалов.
    /// По умолчанию on: системный pointer в игре не нужен, курсоры — засечки.
    #[serde(default = "def_true")]
    pub line_cursor: bool,
    /// «RMB = pen press»: правая кнопка мыши управляет левой рукой (тап/слайд
    /// с планшетным оффсетом), собственный press пера игнорируется.
    #[serde(default)]
    pub rmb_pen_press: bool,
    /// доп. оффсет мыши (мс): + сдвигает тап позже (компенсирует ранний bias мыши)
    #[serde(default)]
    pub mouse_offset_ms: f32,
    /// доп. оффсет планшета (мс): + сдвигает тап позже
    #[serde(default)]
    pub tablet_offset_ms: f32,
    /// порог нажатия стилуса Parblo (давление > порога); 0 = любой тап (лёгкие тапы p=1..5)
    #[serde(default)]
    pub tablet_press_threshold: i32,
    /// громкость музыки (0..1)
    #[serde(default = "def_volume")]
    pub volume: f32,
}

fn def_volume() -> f32 {
    1.0
}

fn def_xtol() -> f32 {
    0.75
}

fn def_zoom() -> f32 {
    1.8
}

fn def_speed() -> f32 {
    6.5
}
fn def_scroll() -> f32 {
    1.0
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            speed: def_speed(),
            scroll_speed: def_scroll(),
            fullscreen: false,
            tablet_zoom: def_zoom(),
            audio_offset_ms: 0.0,
            note_guides: false,
            x_tolerance: def_xtol(),
            mouse_absolute: false,
            line_cursor: true,
            rmb_pen_press: false,
            mouse_offset_ms: 0.0,
            tablet_offset_ms: 0.0,
            tablet_press_threshold: 0,
            volume: def_volume(),
        }
    }
}

impl Settings {
    /// Привести значения к допустимым диапазонам (защита от битого json и
    /// от выхода за границы при правке в экране настроек).
    pub fn sanitize(&mut self) {
        self.speed = (self.speed.clamp(0.5, 9.5) * 10.0).round() / 10.0;
        self.scroll_speed = (self.scroll_speed.clamp(0.25, 5.0) * 100.0).round() / 100.0;
        self.tablet_zoom = (self.tablet_zoom.clamp(0.5, 4.0) * 100.0).round() / 100.0;
        self.audio_offset_ms = self.audio_offset_ms.clamp(-500.0, 500.0).round();
        self.x_tolerance = (self.x_tolerance.clamp(0.1, 6.0) * 100.0).round() / 100.0;
        self.mouse_offset_ms = self.mouse_offset_ms.clamp(-250.0, 250.0).round();
        self.tablet_offset_ms = self.tablet_offset_ms.clamp(-250.0, 250.0).round();
        self.tablet_press_threshold = self.tablet_press_threshold.clamp(0, 255);
        self.volume = (self.volume.clamp(0.0, 1.0) * 100.0).round() / 100.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Новые опции по умолчанию выключены; старый settings.json без них
    /// читается (serde default = false), а включённые значения переживают
    /// round-trip файла.
    #[test]
    fn cursor_options_default_and_round_trip() {
        let d = Settings::default();
        assert!(d.line_cursor && !d.rmb_pen_press);
        let old: Settings = serde_json::from_str(r#"{"speed":6.5,"mouse_absolute":true}"#)
            .expect("old settings file");
        assert!(old.line_cursor && !old.rmb_pen_press);
        let on = Settings {
            line_cursor: true,
            rmb_pen_press: true,
            ..Settings::default()
        };
        let text = serde_json::to_string(&on).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert!(back.line_cursor && back.rmb_pen_press);
    }
}

fn def_true() -> bool {
    true
}

const PATH: &str = "data/settings.json";

pub fn load() -> Settings {
    let mut s: Settings = std::fs::read_to_string(PATH)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    s.sanitize();
    s
}

pub fn save(s: &Settings) {
    let mut s = *s;
    s.sanitize();
    if let Ok(t) = serde_json::to_string_pretty(&s) {
        let _ = std::fs::write(PATH, t);
    }
}