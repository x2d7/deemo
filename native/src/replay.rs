//! Запись реплея: сырой ввод (мышь + планшет) и действия игры с временем
//! относительно карты. Сохраняется по F2 (в конце карты / на результате).
//!
//! Формат JSON: meta (настройки + результат), notes (карта для справки),
//! events (сырые события ввода и судейство). Время t — секунды карты.

use crate::chart::{Chart, NoteKind};
use crate::play::{Judge, Play};
use crate::stage::note_x;
use crate::tablet::RawTabletEvent;
use serde_json::{json, Value};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// источники
const SRC_MOUSE: u8 = 0;
// типы событий
const EV_MOTION: u8 = 0;
const EV_DOWN: u8 = 1;
const EV_UP: u8 = 2;
const EV_REPORT: u8 = 3;
const EV_TAP: u8 = 4;
const EV_SLIDE: u8 = 5;

/// Компактное событие (Copy, без heap-аллокаций на каждый ввод).
#[derive(Clone, Copy)]
struct Ev {
    t: f32,
    src: u8,
    ev: u8,
    x: f32,
    dx: f32,
    x_raw: i32,
    p: i32,
    hover: bool,
    press: bool,
    xn: f32,
    judge: i8, // -1 нет, 0 Perfect, 1 Good, 2 Miss
    err_ms: f32,
    raw: [u8; 16],
    raw_len: u8,
}

impl Default for Ev {
    fn default() -> Self {
        Ev {
            t: 0.0,
            src: SRC_MOUSE,
            ev: EV_MOTION,
            x: 0.0,
            dx: 0.0,
            x_raw: 0,
            p: 0,
            hover: false,
            press: false,
            xn: 0.0,
            judge: -1,
            err_ms: 0.0,
            raw: [0; 16],
            raw_len: 0,
        }
    }
}

fn src_name(s: u8) -> &'static str {
    match s {
        0 => "mouse",
        1 => "parblo",
        2 => "gaomon",
        _ => "?",
    }
}

fn judge_name(j: i8) -> &'static str {
    match j {
        0 => "Perfect",
        1 => "Good",
        2 => "Miss",
        _ => "",
    }
}

fn judge_code(j: Judge) -> i8 {
    match j {
        Judge::Perfect => 0,
        Judge::Good => 1,
        Judge::Miss => 2,
    }
}

fn kind_name(k: NoteKind) -> &'static str {
    match k {
        NoteKind::Normal => "normal",
        NoteKind::Slide => "slide",
        NoteKind::Swipe => "swipe",
        NoteKind::NoSound => "nosound",
    }
}

pub struct Replay {
    events: Vec<Ev>,
    song_id: String,
    diff: String,
    title: String,
    speed: f32,
    offset_ms: f32,
    tablet_zoom: f32,
    x_tolerance: f32,
    mouse_offset_ms: f32,
    tablet_offset_ms: f32,
    notes: Vec<Value>,
    started: Instant,
}

impl Replay {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        song_id: &str,
        diff: &str,
        title: &str,
        chart: &Chart,
        speed: f32,
        offset_ms: f32,
        tablet_zoom: f32,
        x_tolerance: f32,
    ) -> Replay {
        let notes = chart
            .notes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                json!({"i": i, "t": n.time, "x": note_x(n.pos), "kind": kind_name(n.kind)})
            })
            .collect();
        Replay {
            events: Vec::new(),
            song_id: song_id.to_string(),
            diff: diff.to_string(),
            title: title.to_string(),
            speed,
            offset_ms,
            tablet_zoom,
            x_tolerance,
            mouse_offset_ms: 0.0,
            tablet_offset_ms: 0.0,
            notes,
            started: Instant::now(),
        }
    }

    /// Раздельные оффсеты ввода (для meta реплея).
    pub fn set_input_offsets(&mut self, mouse: f32, tablet: f32) {
        self.mouse_offset_ms = mouse;
        self.tablet_offset_ms = tablet;
    }

    pub fn mouse_motion(&mut self, t: f32, dx: f32, x: f32) {
        self.events.push(Ev {
            t,
            src: SRC_MOUSE,
            ev: EV_MOTION,
            dx,
            x,
            ..Default::default()
        });
    }

    pub fn mouse_button(&mut self, t: f32, down: bool, x: f32) {
        self.events.push(Ev {
            t,
            src: SRC_MOUSE,
            ev: if down { EV_DOWN } else { EV_UP },
            x,
            ..Default::default()
        });
    }

    pub fn tablet(&mut self, t: f32, e: &RawTabletEvent) {
        let src = match e.kind {
            "parblo" => 1,
            "gaomon" => 2,
            _ => 3,
        };
        self.events.push(Ev {
            t,
            src,
            ev: EV_REPORT,
            x_raw: e.x_raw,
            p: e.pressure,
            hover: e.hover,
            press: e.press,
            xn: e.x_norm,
            raw: e.raw,
            raw_len: e.raw_len,
            ..Default::default()
        });
    }

    pub fn action(&mut self, t: f32, src: u8, tap: bool, x: f32, judge: Option<Judge>, err_ms: f32) {
        self.events.push(Ev {
            t,
            src,
            ev: if tap { EV_TAP } else { EV_SLIDE },
            x,
            judge: judge.map(judge_code).unwrap_or(-1),
            err_ms,
            ..Default::default()
        });
    }

    fn ev_json(e: &Ev) -> Value {
        let mut v = json!({"t": e.t, "src": src_name(e.src)});
        match e.ev {
            EV_MOTION => {
                v["ev"] = json!("motion");
                v["dx"] = json!(e.dx);
                v["x"] = json!(e.x);
            }
            EV_DOWN | EV_UP => {
                v["ev"] = json!(if e.ev == EV_DOWN { "down" } else { "up" });
                v["x"] = json!(e.x);
            }
            EV_REPORT => {
                let raw: String = e.raw[..e.raw_len as usize]
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                v["ev"] = json!("report");
                v["x_raw"] = json!(e.x_raw);
                v["p"] = json!(e.p);
                v["hover"] = json!(e.hover);
                v["press"] = json!(e.press);
                v["xn"] = json!(e.xn);
                v["raw"] = json!(raw);
            }
            EV_TAP | EV_SLIDE => {
                v["ev"] = json!(if e.ev == EV_TAP { "tap" } else { "slide" });
                v["x"] = json!(e.x);
                if e.judge >= 0 {
                    v["judge"] = json!(judge_name(e.judge));
                    v["err_ms"] = json!(e.err_ms);
                }
            }
            _ => {}
        }
        v
    }

    /// Сохранить реплей в data/replays/<song>_<diff>_<ts>.json.
    pub fn save(&self, play: &Play) -> anyhow::Result<std::path::PathBuf> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let dir = "data/replays";
        std::fs::create_dir_all(dir)?;
        let path = format!("{}/{}_{}_{}.json", dir, self.song_id, self.diff, ts);
        let events: Vec<Value> = self.events.iter().map(Self::ev_json).collect();
        let doc = json!({
            "meta": {
                "song": self.song_id,
                "diff": self.diff,
                "title": self.title,
                "speed": self.speed,
                "offset_ms": self.offset_ms,
                "perfect_ms": crate::play::PERFECT_MS,
                "good_ms": crate::play::GOOD_MS,
                "tablet_zoom": self.tablet_zoom,
                "x_tolerance": self.x_tolerance,
                "mouse_offset_ms": self.mouse_offset_ms,
                "tablet_offset_ms": self.tablet_offset_ms,
                "timestamp": ts,
                "duration_s": self.started.elapsed().as_secs_f32(),
                "score": play.score,
                "max_combo": play.max_combo,
                "counts": {"perfect": play.counts[0], "good": play.counts[1], "miss": play.counts[2]},
                "accuracy": play.accuracy(),
                "mean_err_ms": play.mean_error() * 1000.0,
                "event_count": self.events.len(),
            },
            "notes": self.notes,
            "events": events,
        });
        std::fs::write(&path, serde_json::to_string(&doc)?)?;
        Ok(path.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_save() {
        let chart = Chart::empty();
        let play = Play::new(&chart);
        let mut r = Replay::new("testsong", "hard", "Test Song", &chart, 6.5, -26.0, 1.0, 0.75);
        r.mouse_motion(1.0, 3.0, 0.5);
        r.mouse_button(1.1, true, 0.5);
        r.action(1.1, 0, true, 0.5, Some(Judge::Perfect), -3.0);
        r.mouse_button(1.2, false, 0.5);
        let ev = RawTabletEvent {
            wall: Instant::now(),
            kind: "parblo",
            x_raw: 12345,
            pressure: 200,
            hover: false,
            press: true,
            x_norm: 0.6,
            raw: [0xa1, 0x00, 0x39, 0x30, 0, 0, 0xc8, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            raw_len: 10,
        };
        r.tablet(1.3, &ev);
        r.action(1.3, 1, true, 0.8, Some(Judge::Good), 20.0);
        let path = r.save(&play).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["meta"]["song"], "testsong");
        assert_eq!(doc["meta"]["diff"], "hard");
        assert_eq!(doc["meta"]["perfect_ms"], 50.0);
        assert_eq!(doc["meta"]["good_ms"], 120.0);
        assert_eq!(doc["meta"]["event_count"], 6);
        let events = doc["events"].as_array().unwrap();
        assert_eq!(events[0]["ev"], "motion");
        assert_eq!(events[0]["src"], "mouse");
        assert_eq!(events[2]["ev"], "tap");
        assert_eq!(events[2]["judge"], "Perfect");
        assert_eq!(events[4]["ev"], "report");
        assert_eq!(events[4]["src"], "parblo");
        assert_eq!(events[4]["x_raw"], 12345);
        assert_eq!(events[4]["raw"], "a10039300000c8000000");
        assert_eq!(events[5]["judge"], "Good");
        let _ = std::fs::remove_file(&path);
    }
}