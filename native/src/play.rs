//! Судейство (tap) и счёт: Perfect / Good / Miss, комбо, очки.

use crate::chart::{Chart, NoteKind};
use crate::stage::note_x;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Judge {
    Perfect,
    Good,
    Miss,
}

/// Зашитые окна судейства. Раньше их можно было крутить в настройках; теперь
/// значения фиксированы в коде и совпадают с реверсом Deemo 3.7.3
/// (NoteBasic: PERFECT_TIME_RANGE=0.05, HIT_TIME_RANGE=0.12) — именно с ними
/// игра выпускается и играется (data/settings.json хранил те же 50/120 мс).
/// Perfect ±50 мс, Good ±120 мс; из UI их не поменять.
pub const PERFECT_MS: f32 = 50.0;
pub const GOOD_MS: f32 = 120.0;
pub const PERFECT_WINDOW: f32 = PERFECT_MS / 1000.0;
pub const GOOD_WINDOW: f32 = GOOD_MS / 1000.0;
/// Окно реакции на тап (сек). В реверсе Deemo 3.7.3 CLICK_TIME_RANGE=0.3;
/// заодно это окно, после которого нота автоматически уходит в Miss.
pub const CLICK_WINDOW: f32 = 0.3;
/// Горизонтальный допуск тапа (мировые единицы) — из ClickNote.touch/SlideNote.touch.
const X_TOLERANCE: f32 = 0.75;

#[derive(Clone)]
pub struct NoteState {
    pub judged: bool,
    pub result: Judge,
    pub hit_time: f32,
    /// note.time − hit_time: где нота была в момент попадания (для позиции эффекта)
    pub hit_dt: f32,
}

#[derive(Clone)]
pub struct Play {
    pub states: Vec<NoteState>,
    pub combo: u32,
    pub max_combo: u32,
    pub score: u64,
    pub counts: [u32; 3],
    pub last_judge: Option<(Judge, f32)>,
    pub last_hit_time: f32,
    pub last_hit_x: f32,
    /// окна судейства (сек), настраиваются
    pub perfect: f32,
    pub good: f32,
    pub miss: f32,
    /// окно реакции на тап (CLICK_TIME_RANGE), сек
    pub click: f32,
    /// горизонтальный допуск тапа/слайда (мировые единицы); 0.75 = аутентичный Deemo
    pub x_tol: f32,
    /// тайминги попаданий (t − note.time, сек): + поздно, − рано
    pub errors: Vec<f32>,
    /// тайминги только ТАПОВ (без слайдеров) — для калибровки оффсета:
    /// ошибка слайдера механическая (ранняя граница Z-окна), не индикатор синхронизации
    pub tap_errors: Vec<f32>,
    /// скоринг Deemo (из 1_000_000): noteScore + comboScore
    note_score: f64,
    combo_score: f64,
    combo_stack: i32,
    n_base: f64,
    c_base: f64,
}

impl Play {
    pub fn new(chart: &Chart) -> Play {
        let n = chart.notes.len().max(1) as f64;
        let n_base = 800000.0 / n;
        let c_base = if n > 1.0 { 400000.0 / (n * (n - 1.0)) } else { 0.0 };
        Play {
            states: vec![
                NoteState {
                    judged: false,
                    result: Judge::Miss,
                    hit_time: 0.0,
                    hit_dt: 0.0,
                };
                chart.notes.len()
            ],
            combo: 0,
            max_combo: 0,
            score: 0,
            counts: [0; 3],
            last_judge: None,
            last_hit_time: -10.0,
            last_hit_x: 0.0,
            perfect: PERFECT_WINDOW,
            good: GOOD_WINDOW,
            miss: CLICK_WINDOW,
            click: CLICK_WINDOW,
            x_tol: X_TOLERANCE,
            errors: Vec::new(),
            tap_errors: Vec::new(),
            note_score: 0.0,
            combo_score: 0.0,
            combo_stack: 0,
            n_base,
            c_base,
        }
    }

    /// Горизонтальный допуск (мировые единицы). Шире = удобнее на ПК.
    pub fn set_x_tolerance(&mut self, x: f32) {
        self.x_tol = x.max(0.1);
    }

    fn apply(&mut self, i: usize, r: Judge, t: f32, x: f32, note_time: f32) {
        self.states[i].judged = true;
        self.states[i].result = r;
        self.states[i].hit_time = t;
        self.states[i].hit_dt = note_time - t;
        self.last_judge = Some((r, t));
        // ratio как в judge() оригинала: (good - diff)/(good - perfect), Perfect=1
        let diff = (note_time - t).abs();
        let ratio = match r {
            Judge::Perfect => 1.0f64,
            Judge::Good => {
                ((self.good - diff) / (self.good - self.perfect)).clamp(0.0, 1.0) as f64
            }
            Judge::Miss => 0.0,
        };
        match r {
            Judge::Miss => {
                self.combo = 0;
                self.counts[2] += 1;
                // comboStack decay ×0.7 (не полный сброс) — как в оригинале
                self.combo_stack = (self.combo_stack as f64 * 0.7).round() as i32;
            }
            _ => {
                self.errors.push(t - note_time);
                self.combo += 1;
                self.max_combo = self.max_combo.max(self.combo);
                self.counts[if r == Judge::Perfect { 0 } else { 1 }] += 1;
                self.note_score += self.n_base * (0.5 + 0.5 * ratio);
                self.combo_score += self.c_base * self.combo_stack as f64;
                self.combo_stack += 1;
                self.last_hit_time = t;
                self.last_hit_x = x;
            }
        }
        self.score = (self.note_score + self.combo_score).round() as u64;
    }

    /// Автоматические Miss для нот, ушедших за окно.
    pub fn update(&mut self, chart: &Chart, t: f32) {
        for (i, n) in chart.notes.iter().enumerate() {
            if !self.states[i].judged && t - n.time > self.miss {
                let x = note_x(n.pos);
                self.apply(i, Judge::Miss, t, x, n.time);
            }
        }
    }

    /// Тап в мировой точке x. Нота реагирует в пределах click-окна (300 мс),
    /// судится по таймингу: Perfect ≤50мс, Good ≤120мс, иначе Miss.
    pub fn tap(&mut self, chart: &Chart, t: f32, x: f32) -> Option<Judge> {
        let mut best: Option<(usize, f32)> = None;
        for (i, n) in chart.notes.iter().enumerate() {
            if self.states[i].judged {
                continue;
            }
            let dt = (n.time - t).abs();
            if dt > self.click {
                continue;
            }
            if (note_x(n.pos) - x).abs() > self.x_tol {
                continue;
            }
            if best.map(|(_, b)| dt < b).unwrap_or(true) {
                best = Some((i, dt));
            }
        }
        let (i, dt) = best?;
        let r = if dt <= self.perfect {
            Judge::Perfect
        } else if dt <= self.good {
            Judge::Good
        } else {
            Judge::Miss
        };
        let nx = note_x(chart.notes[i].pos);
        self.apply(i, r, t, nx, chart.notes[i].time);
        if r != Judge::Miss {
            self.tap_errors.push(t - chart.notes[i].time);
        }
        Some(r)
    }

    /// Слайдинг как в оригинале (SlideNote.move): нота сбивается, когда касание
    /// в пределах 0.75 по X И по Z от её ТЕКУЩЕЙ позиции (касание на линии, z=0).
    /// Пространственное окно по Z зависит от скорости падения. Затем judge по таймингу.
    pub fn slide(&mut self, chart: &Chart, t: f32, x: f32, fall_speed: f32) -> bool {
        let mut hit = false;
        let zm = crate::stage::z_multiplier(fall_speed);
        for (i, n) in chart.notes.iter().enumerate() {
            if self.states[i].judged || n.kind != NoteKind::Slide {
                continue;
            }
            let dt = n.time - t;
            let note_z = fall_speed * n.speed * zm * dt;
            // Z-окно (когда слайд сбивается) — аутентичное 0.75, НЕ расширяем:
            // иначе слайд срабатывает рано и не даёт Perfect. Расширяем только X.
            if note_z.abs() > X_TOLERANCE {
                continue;
            }
            if (note_x(n.pos) - x).abs() > self.x_tol {
                continue;
            }
            let adt = dt.abs();
            let r = if adt <= self.perfect {
                Judge::Perfect
            } else if adt <= self.good {
                Judge::Good
            } else {
                Judge::Miss
            };
            let nx = note_x(n.pos);
            self.apply(i, r, t, nx, n.time);
            hit = true;
        }
        hit
    }

    /// Завершена ли песня (все ноты рассуждены + хвост).
    pub fn finished(&self, chart: &Chart, t: f32) -> bool {
        let last = chart.notes.last().map(|n| n.time).unwrap_or(0.0);
        t > last + 1.5 && self.states.iter().all(|s| s.judged)
    }

    pub fn accuracy(&self) -> f32 {
        // Deemo (ResultManager.DisplayScore): round(totalScore * 0.01) * 0.0001,
        // т.е. процент от 1_000_000 с округлением до 2 знаков: round(score/100)/100.
        // Это achievement rate (включает комбо-бонус), не чистая точность попаданий.
        ((self.score as f64 / 100.0).round() / 100.0) as f32
    }

    /// Средний тайминг попаданий (сек): + значит аудио отстаёт (жмём поздно).
    pub fn mean_error(&self) -> f32 {
        if self.errors.is_empty() {
            return 0.0;
        }
        self.errors.iter().sum::<f32>() / self.errors.len() as f32
    }

    /// Средний тайминг только тапов (без слайдеров) — для калибровки оффсета.
    pub fn mean_tap_error(&self) -> f32 {
        if self.tap_errors.is_empty() {
            return self.mean_error();
        }
        self.tap_errors.iter().sum::<f32>() / self.tap_errors.len() as f32
    }

    pub fn median_error(&self) -> f32 {
        if self.errors.is_empty() {
            return 0.0;
        }
        let mut v = self.errors.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    #[allow(dead_code)]
    pub fn kind_of(chart: &Chart, i: usize) -> NoteKind {
        chart.notes[i].kind
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
                .map(|&t| Note {
                    time: t,
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
    fn perfect_and_good() {
        let c = chart_with(&[1.0, 2.0]);
        let mut p = Play::new(&c);
        assert_eq!(p.tap(&c, 1.0, 0.0), Some(Judge::Perfect));
        // между окнами Perfect (90 мс) и Good (180 мс)
        let dt = (PERFECT_WINDOW + GOOD_WINDOW) * 0.5;
        assert_eq!(p.tap(&c, 2.0 + dt, 0.0), Some(Judge::Good));
        assert_eq!(p.combo, 2);
        assert_eq!(p.counts, [1, 1, 0]);
        assert!(p.score > 0);
    }

    /// Окна судейства зашиты в игру (в настройках их больше нет).
    #[test]
    fn judging_windows_are_fixed() {
        assert!((PERFECT_MS - 50.0).abs() < 1e-6);
        assert!((GOOD_MS - 120.0).abs() < 1e-6);
        let c = chart_with(&[1.0]);
        let p = Play::new(&c);
        assert!((p.perfect - 0.050).abs() < 1e-6);
        assert!((p.good - 0.120).abs() < 1e-6);
    }

    #[test]
    fn miss_on_timeout() {
        let c = chart_with(&[1.0]);
        let mut p = Play::new(&c);
        p.update(&c, 1.0 + CLICK_WINDOW + 0.01);
        assert_eq!(p.counts[2], 1);
        assert_eq!(p.combo, 0);
        assert!(p.states[0].judged);
    }

    #[test]
    fn no_double_hit() {
        let c = chart_with(&[1.0]);
        let mut p = Play::new(&c);
        assert!(p.tap(&c, 1.0, 0.0).is_some());
        assert!(p.tap(&c, 1.0, 0.0).is_none());
    }

    #[test]
    fn x_tolerance() {
        let c = chart_with(&[1.0]);
        let mut p = Play::new(&c);
        // далеко по горизонтали — не попадаем
        assert!(p.tap(&c, 1.0, 10.0).is_none());
    }

    #[test]
    fn accuracy_and_finish() {
        let c = chart_with(&[1.0, 2.0]);
        let mut p = Play::new(&c);
        p.tap(&c, 1.0, 0.0);
        p.update(&c, 2.0 + CLICK_WINDOW + 0.01);
        // 1 Perfect из 2: noteScore=400000, comboScore=0 → 40%
        assert!((p.accuracy() - 40.0).abs() < 1e-3);
        assert!(p.finished(&c, 4.0));
    }

    #[test]
    fn full_combo_all_perfect_is_million() {
        let c = chart_with(&[1.0, 2.0, 3.0, 4.0]);
        let mut p = Play::new(&c);
        for t in [1.0, 2.0, 3.0, 4.0] {
            assert_eq!(p.tap(&c, t, 0.0), Some(Judge::Perfect));
        }
        assert_eq!(p.score, 1_000_000);
        assert!((p.accuracy() - 100.0).abs() < 1e-3);
    }

    #[test]
    fn slide_waits_for_perfect() {
        let mut c = chart_with(&[1.0]);
        c.notes[0].kind = NoteKind::Slide;
        let mut p = Play::new(&c);
        // рано (dt=0.1, z далеко) — уже-зажатый слайд не бьёт
        assert!(!p.slide(&c, 0.9, 0.0, 6.5));
        assert!(!p.states[0].judged);
        // на линии (dt=0, z=0) — бьёт Perfect
        assert!(p.slide(&c, 1.0, 0.0, 6.5));
        assert_eq!(p.counts[0], 1);
    }

    #[test]
    fn press_on_slide_judges_by_timing() {
        let mut c = chart_with(&[1.0]);
        c.notes[0].kind = NoteKind::Slide;
        let mut p = Play::new(&c);
        // нажатие когда нота в окне, но не perfect → Good
        assert_eq!(p.tap(&c, 0.9, 0.0), Some(Judge::Good));
    }

    #[test]
    fn slide_ignores_normal_notes() {
        let c = chart_with(&[1.0]);
        let mut p = Play::new(&c);
        assert!(!p.slide(&c, 1.0, 0.0, 6.5));
        assert!(!p.states[0].judged);
    }
}