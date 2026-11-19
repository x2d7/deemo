//! Судейство (tap) и счёт: Perfect / Good / Miss, комбо, очки.

use crate::chart::{Chart, NoteKind};
use crate::stage::note_x;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Judge {
    Perfect,
    Good,
    Miss,
}

/// Рука, которой сыграна нота. Та же конвенция, что у сервера:
/// левая половина экрана (`pos < 0`) — перо/планшет, правая (`pos >= 0`) — мышь.
pub const HAND_MOUSE: usize = 0;
pub const HAND_TABLET: usize = 1;

/// Рука ноты по её горизонтальной позиции (`Note::pos`).
pub fn hand_of_pos(pos: f32) -> usize {
    if pos < 0.0 { HAND_TABLET } else { HAND_MOUSE }
}

/// Счётчики судейства по одному устройству/руке.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandCounts {
    #[serde(default)]
    pub perfect: u32,
    #[serde(default)]
    pub good: u32,
    #[serde(default)]
    pub miss: u32,
}

impl HandCounts {
    pub fn add(&mut self, j: Judge) {
        match j {
            Judge::Perfect => self.perfect += 1,
            Judge::Good => self.good += 1,
            Judge::Miss => self.miss += 1,
        }
    }

    /// Всего рассуженных нот этой рукой (тесты/диагностика).
    #[allow(dead_code)]
    pub fn total(&self) -> u32 {
        self.perfect + self.good + self.miss
    }
}

/// Статистика по рукам. Сериализуется ровно в формат сервера:
/// `{"mouse": {...}, "tablet": {...}}` (поле `hands` тела `POST /api/scores`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hands {
    pub mouse: HandCounts,
    pub tablet: HandCounts,
}

impl Hands {
    /// Счётчики руки по её индексу ([`HAND_MOUSE`] / [`HAND_TABLET`]).
    #[allow(dead_code)]
    pub fn get_mut(&mut self, hand: usize) -> &mut HandCounts {
        if hand == HAND_TABLET { &mut self.tablet } else { &mut self.mouse }
    }

    /// Добавить авто-Miss, которые `play.update` атрибутировал рукам по знаку
    /// `pos` (у ноты нет устройства — рука выводится из её позиции).
    pub fn fold_auto_miss(&mut self, play: &Play) {
        self.mouse.miss += play.auto_miss[HAND_MOUSE];
        self.tablet.miss += play.auto_miss[HAND_TABLET];
    }

    /// Всего нот по обеим рукам (тесты/диагностика).
    #[allow(dead_code)]
    pub fn total(&self) -> u32 {
        self.mouse.total() + self.tablet.total()
    }
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
    /// мировая X указателя в момент судейства; NaN — нота не сбита (мисс).
    /// Пишется только для попаданий (Perfect/Good), в DMR3 едет как x_note.
    pub pointer_x: f32,
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
    /// Авто-Miss по рукам (индекс — [`hand_of_pos`]): в момент пропуска у ноты
    /// нет устройства, поэтому рука берётся из знака её `pos`.
    pub auto_miss: [u32; 2],
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
                    pointer_x: f32::NAN,
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
            auto_miss: [0; 2],
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

    /// Применить ноту из реплея без ввода: `hit_time` — записанное время
    /// попадания (секунды карты). Судья выводится из той же разницы, что и у
    /// тапа; промах приходит как `note.time + CLICK_WINDOW` от драйвера и
    /// остаётся Miss. Повторный вызов по уже рассужденной ноте — no-op.
    /// Возвращает итогового судью (для кольца нажатия в просмотре).
    pub fn replay_hit(&mut self, chart: &Chart, i: usize, hit_time: f32) -> Judge {
        let Some(n) = chart.notes.get(i) else {
            return Judge::Miss;
        };
        if self.states.get(i).map(|s| s.judged).unwrap_or(true) {
            return self.states.get(i).map(|s| s.result).unwrap_or(Judge::Miss);
        }
        let dt = (hit_time - n.time).abs();
        let r = if dt <= self.perfect {
            Judge::Perfect
        } else if dt <= self.good {
            Judge::Good
        } else {
            Judge::Miss
        };
        let x = note_x(n.pos);
        self.apply(i, r, hit_time, x, n.time);
        r
    }

    /// Автоматические Miss для нот, ушедших за окно. Каждый пропуск
    /// засчитывается руке по знаку `pos` (статистика `hands` для сервера).
    pub fn update(&mut self, chart: &Chart, t: f32) {
        for (i, n) in chart.notes.iter().enumerate() {
            if !self.states[i].judged && t - n.time > self.miss {
                let x = note_x(n.pos);
                self.auto_miss[hand_of_pos(n.pos)] += 1;
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
            // позиция указателя в момент судейства (для DMR3 x_note)
            self.states[i].pointer_x = x;
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
            if r != Judge::Miss {
                // позиция указателя (та же для всех нот, сбитых одним слайдом)
                self.states[i].pointer_x = x;
            }
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

    /// Настоящая точность попаданий в процентах (0..100), osu!-стиль:
    /// `(perfect + 0.5·good) / (perfect + good + miss)`.
    ///
    /// Это НЕ [`Play::accuracy`]: там achievement rate из счёта (с комбо-бонусом).
    /// Авто-Miss из [`Play::update`] уже сидят в `counts[2]`, поэтому промахи
    /// учитываются. Пустой чарт (ни одной рассуженной ноты) — `0.0`: судить
    /// нечего, и это совпадает с «всё промазано» (0%).
    pub fn hit_accuracy(&self) -> f32 {
        let [perfect, good, miss] = self.counts;
        let total = perfect + good + miss;
        if total == 0 {
            return 0.0;
        }
        ((perfect as f64 + 0.5 * good as f64) / total as f64 * 100.0) as f32
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

    /// `pos < 0` — левая рука (перо/планшет), `pos >= 0` — правая (мышь):
    /// та же конвенция, что у сервера в `hands`.
    #[test]
    fn hand_of_pos_splits_at_zero() {
        assert_eq!(hand_of_pos(-0.01), HAND_TABLET);
        assert_eq!(hand_of_pos(-3.0), HAND_TABLET);
        assert_eq!(hand_of_pos(0.0), HAND_MOUSE);
        assert_eq!(hand_of_pos(2.5), HAND_MOUSE);
    }

    /// Синтетический плей: тапы мышью, тап пером и авто-Miss, атрибутированные
    /// по знаку `pos`. Сумма `hands` должна сойтись с `play.counts`.
    #[test]
    fn hands_accumulate_taps_and_auto_miss_by_pos() {
        let c = Chart {
            speed: 6.0,
            notes: vec![
                Note { time: 1.0, pos: 0.6, size: 1.0, speed: 1.0, kind: NoteKind::Normal, hold: 0.0, sounds: vec![] },
                Note { time: 2.0, pos: -0.6, size: 1.0, speed: 1.0, kind: NoteKind::Normal, hold: 0.0, sounds: vec![] },
                Note { time: 3.0, pos: 0.7, size: 1.0, speed: 1.0, kind: NoteKind::Normal, hold: 0.0, sounds: vec![] },
                Note { time: 3.0, pos: -0.7, size: 1.0, speed: 1.0, kind: NoteKind::Normal, hold: 0.0, sounds: vec![] },
            ],
            links: vec![],
        };
        let mut p = Play::new(&c);
        let mut hands = Hands::default();
        // мышь: Perfect по правой ноте
        let x = note_x(0.6);
        let j = p.tap(&c, 1.0, x);
        assert_eq!(j, Some(Judge::Perfect));
        hands.mouse.add(j.unwrap());
        // перо: Good по левой ноте (в окне Good, но вне Perfect)
        let dt = (PERFECT_WINDOW + GOOD_WINDOW) * 0.5;
        let x = note_x(-0.6);
        let j = p.tap(&c, 2.0 + dt, x);
        assert_eq!(j, Some(Judge::Good));
        hands.tablet.add(j.unwrap());
        // обе оставшиеся ноты уходят за окно — рука берётся из pos
        p.update(&c, 3.0 + CLICK_WINDOW + 0.01);
        assert_eq!(p.auto_miss, [1, 1], "one auto-miss per hand");
        hands.fold_auto_miss(&p);

        assert_eq!(hands.mouse, HandCounts { perfect: 1, good: 0, miss: 1 });
        assert_eq!(hands.tablet, HandCounts { perfect: 0, good: 1, miss: 1 });
        assert_eq!(hands.total(), p.counts.iter().sum::<u32>());
        assert_eq!(p.counts, [1, 1, 2]);
    }

    /// Настоящая точность попаданий (не achievement): osu-стиль 0.5 за Good.
    #[test]
    fn hit_accuracy_weighs_good_and_counts_misses() {
        let c = chart_with(&[1.0]);
        let mut p = Play::new(&c);
        // все Perfect → 100%
        p.counts = [10, 0, 0];
        assert!((p.hit_accuracy() - 100.0).abs() < 1e-4);
        // половина Good, половина Perfect → 75%
        p.counts = [5, 5, 0];
        assert!((p.hit_accuracy() - 75.0).abs() < 1e-4);
        // промахи в знаменателе: (2 + 0.5·1) / 10 = 25%
        p.counts = [2, 1, 7];
        assert!((p.hit_accuracy() - 25.0).abs() < 1e-4);
        // всё промазано → 0%
        p.counts = [0, 0, 4];
        assert!(p.hit_accuracy().abs() < 1e-4);
    }

    /// Пустой чарт (ни одной рассуженной ноты): судить нечего → 0%, без NaN.
    #[test]
    fn hit_accuracy_of_an_empty_play_is_zero() {
        let c = chart_with(&[]);
        let p = Play::new(&c);
        assert_eq!(p.counts, [0, 0, 0]);
        assert!(p.hit_accuracy().is_finite());
        assert_eq!(p.hit_accuracy(), 0.0);
        // заодно старый achievement тоже не паникует
        assert_eq!(p.accuracy(), 0.0);
    }

    /// Авто-Miss попадают в `miss`, поэтому точность проседает.
    #[test]
    fn hit_accuracy_includes_auto_missed_notes() {
        let c = chart_with(&[1.0, 2.0]);
        let mut p = Play::new(&c);
        assert_eq!(p.tap(&c, 1.0, 0.0), Some(Judge::Perfect));
        p.update(&c, 2.0 + CLICK_WINDOW + 0.01);
        assert_eq!(p.counts, [1, 0, 1]);
        assert!((p.hit_accuracy() - 50.0).abs() < 1e-4);
    }

    /// `hit_accuracy` и `accuracy` — разные числа: achievement учитывает комбо.
    #[test]
    fn achievement_is_not_hit_accuracy() {
        let c = chart_with(&[1.0]);
        let mut p = Play::new(&c);
        p.counts = [10, 0, 0];
        p.score = 900_000;
        assert!((p.accuracy() - 90.0).abs() < 1e-4);
        assert!((p.hit_accuracy() - 100.0).abs() < 1e-4);
    }

    /// Тело для сервера — ровно `{"mouse":{...},"tablet":{...}}` с именами
    /// полей perfect/good/miss.
    #[test]
    fn hands_serialize_in_the_server_shape() {
        let hands = Hands {
            mouse: HandCounts { perfect: 300, good: 12, miss: 3 },
            tablet: HandCounts { perfect: 280, good: 20, miss: 5 },
        };
        let v = serde_json::to_value(hands).unwrap();
        assert_eq!(v["mouse"]["perfect"], 300);
        assert_eq!(v["mouse"]["good"], 12);
        assert_eq!(v["mouse"]["miss"], 3);
        assert_eq!(v["tablet"]["perfect"], 280);
        assert_eq!(v["tablet"]["good"], 20);
        assert_eq!(v["tablet"]["miss"], 5);
        assert_eq!(v.as_object().unwrap().len(), 2, "no extra keys");
        // обратный разбор (нужен тестам и будущему чтению)
        let back: Hands = serde_json::from_value(v).unwrap();
        assert_eq!(back, hands);
    }
}