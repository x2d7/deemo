//! Фон меню: миниатюрная падающая сцена выбранного чарта.
//!
//! Это не «окно анализа» и не срез таймлайна: ноты рождаются за верхней
//! кромкой, падают с постоянной скоростью и уходят под нижнюю кромку. Всё
//! движение считается в собственных часах фона ([`BgField::clock`]), а не в
//! позиции превью. Поэтому смена песни или сложности не сдвигает уже летящие
//! ноты: меняется только источник будущих нот (курсор в новом чарте).
//!
//! Чистые функции (`note_y`, `link_successors`) и состояние (`BgField`) живут
//! здесь, чтобы их можно было проверять тестами без GPU.

use crate::chart::{Chart, Link};

/// Время падения ноты от верхней кромки до нижней (секунды).
pub const BG_FALL: f32 = 2.0;
/// Потолок живых нот: даже на 2000-нотном чарте фон остаётся дешёвым.
pub const BG_LIVE_CAP: usize = 400;
/// Потолок нарисованных нот за кадр (остальные просто пропускаются).
pub const BG_DRAW_CAP: usize = 120;
/// Сколько ещё живёт нота после «посадки» на нижнюю кромку, пока её полоса
/// целиком не уедет под неё. Полоса ~30–40 px на ~600 px высоты — 0.25 с
/// хватает с запасом при любом масштабе UI.
const BG_TAIL: f32 = 0.25;
/// Потолок компактного списка нот источника (как старый кэш фона).
const BG_NOTES_CAP: usize = 20_000;

/// Одна падающая нота.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BgNote {
    /// Время часов фона, когда нижняя грань ноты стоит на нижней кромке.
    /// Дальше нота продолжает уходить под кромку (y не обрезается).
    pub t: f32,
    /// Позиция в сцене: -2..2, как `pos` чарта.
    pub pos: f32,
    pub size: f32,
    /// Следующая нота слайда: (позиция, время посадки, размер). Нить рисуется,
    /// только пока обе ноты в кадре. `None` — нота не в связке.
    pub next: Option<(f32, f32, f32)>,
}

/// Компактный источник нот для фона: без звуков и прочих данных чарта.
#[derive(Clone, Default)]
pub struct BgSource {
    /// (время ноты, позиция, размер), по времени (как `Chart::notes`).
    pub notes: Vec<(f32, f32, f32)>,
    /// Для каждой ноты — следующая в её слайде (индекс в `notes`).
    pub next: Vec<Option<usize>>,
}

impl BgSource {
    pub fn from_chart(chart: &Chart) -> BgSource {
        let notes: Vec<(f32, f32, f32)> = chart
            .notes
            .iter()
            .take(BG_NOTES_CAP)
            .map(|n| (n.time, n.pos, n.size))
            .collect();
        let next = link_successors(&chart.links, notes.len());
        BgSource { notes, next }
    }
}

/// Сосед по слайду для каждой ноты: `next[a] = b` для каждой пары подряд
/// идущих нот в связке. Ссылки за пределы списка (битые `$ref`, обрезанный
/// кэп) игнорируются, поэтому `next.len() == note_count` и рисование может
/// не проверять границы.
pub fn link_successors(links: &[Link], note_count: usize) -> Vec<Option<usize>> {
    let mut next = vec![None; note_count];
    for link in links {
        for pair in link.notes.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if a < note_count && b < note_count {
                next[a] = Some(b);
            }
        }
    }
    next
}

/// Y нижней грани ноты в момент `now` (часы фона).
///
/// Нота, у которой `t == now`, стоит ровно на нижней кромке `rect`; раньше
/// она выше (до верхней кромки при `now == t - BG_FALL`), позже — ниже
/// кромки. Никакого `clamp` здесь нет: полоса уезжает за экран, а не тает
/// на искусственной границе.
pub fn note_y(rect: [f32; 4], t: f32, now: f32) -> f32 {
    rect[1] + rect[3] * (1.0 - (t - now) / BG_FALL)
}

/// Шаг выборки нот при рисовании: на плотном чарте рисуем каждую N-ю так,
/// чтобы за кадр было не больше [`BG_DRAW_CAP`] полос.
pub fn draw_stride(len: usize) -> usize {
    len.div_ceil(BG_DRAW_CAP).max(1)
}

/// Живое падающее поле + курсор в текущем источнике.
#[derive(Default)]
pub struct BgField {
    live: Vec<BgNote>,
    notes: Vec<(f32, f32, f32)>,
    next: Vec<Option<usize>>,
    cursor: usize,
    clock: f32,
    /// последняя позиция превью: по ней замечаем перезапуск трека (seek назад)
    audio: Option<f32>,
}

impl BgField {
    pub fn live(&self) -> &[BgNote] {
        &self.live
    }

    pub fn clock(&self) -> f32 {
        self.clock
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Полный сброс (для демо-скриншотов); в обычной работе не нужен.
    pub fn reset(&mut self, clock: f32) {
        self.live.clear();
        self.notes.clear();
        self.next.clear();
        self.cursor = 0;
        self.clock = clock;
        self.audio = None;
    }

    /// Сменить чарт-источник, **не трогая** уже падающие ноты.
    ///
    /// Курсор ставится на первую ноту не раньше текущей позиции превью, так
    /// что новые ноты приходят из нового чарта, а старое поле продолжает
    /// падать как ни в чём не бывало.
    pub fn set_source(&mut self, src: BgSource, audio_now: f32) {
        let now = if audio_now.is_finite() { audio_now.max(0.0) } else { 0.0 };
        self.notes = src.notes;
        self.next = src.next;
        self.cursor = self.notes.partition_point(|n| n.0 < now);
        self.audio = Some(now);
        self.spawn(now);
    }

    /// Остановить эмиссию (нет выбранной песни/сложности), сохранив поле.
    pub fn clear_source(&mut self) {
        self.notes.clear();
        self.next.clear();
        self.cursor = 0;
    }

    /// Продвинуть часы фона и досыпать ноты, которые уже в пределах падения.
    pub fn advance(&mut self, dt: f32, audio_now: f32) {
        if dt.is_finite() && dt > 0.0 {
            self.clock += dt;
        }
        let now = if audio_now.is_finite() { audio_now.max(0.0) } else { 0.0 };
        // превью перезапустили (повторный клик по песне, seek назад): курсор
        // возвращается к текущей позиции, чтобы чарт снова начал сыпать ноты.
        // Уже летящие ноты при этом остаются как есть.
        if self.audio.is_some_and(|prev| now + 0.5 < prev) {
            self.cursor = self.notes.partition_point(|n| n.0 < now);
        }
        self.audio = Some(now);
        self.spawn(now);
        self.prune();
    }

    /// Досыпать все ноты, до которых осталось меньше [`BG_FALL`] секунд.
    ///
    /// Время посадки ноты кладётся в часы фона относительно текущей позиции
    /// превью: `land = clock + (note.time - audio_now)`. Поэтому нота, чей
    /// момент уже наступил, оказывается на нижней кромке, а будущая — выше.
    fn spawn(&mut self, audio_now: f32) {
        let limit = audio_now + BG_FALL;
        let mut added = false;
        while let Some(&(nt, pos, size)) = self.notes.get(self.cursor) {
            if nt > limit {
                break;
            }
            let idx = self.cursor;
            self.cursor += 1;
            if !(nt.is_finite() && pos.is_finite() && size.is_finite()) || nt < audio_now {
                continue;
            }
            let t = self.clock + (nt - audio_now);
            let next = self
                .next
                .get(idx)
                .copied()
                .flatten()
                .and_then(|j| self.notes.get(j))
                .map(|&(jt, jpos, jsize)| (jpos, self.clock + (jt - audio_now), jsize));
            self.live.push(BgNote { t, pos, size, next });
            added = true;
        }
        if added {
            self.enforce_cap();
        }
    }

    /// Старые ноты выбрасываем первыми: на плотном чарте это дешевле, чем
    /// рисовать сотни полос, которых всё равно не различить.
    fn enforce_cap(&mut self) {
        if self.live.len() > BG_LIVE_CAP {
            let extra = self.live.len() - BG_LIVE_CAP;
            self.live.drain(0..extra);
        }
    }

    /// Нота уходит из набора только после того, как её полоса целиком
    /// оказалась ниже нижней кромки (≈ [`BG_TAIL`] секунд после посадки).
    fn prune(&mut self) {
        let cutoff = self.clock - BG_TAIL;
        // порядок после смены источника может быть не строго по t, поэтому
        // фильтруем весь (небольшой, ≤ BG_LIVE_CAP) набор
        self.live.retain(|n| n.t >= cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BG_FALL, BG_LIVE_CAP, BgField, BgSource, Link, draw_stride, link_successors, note_y,
    };

    const RECT: [f32; 4] = [100.0, 50.0, 400.0, 300.0];

    fn source(notes: Vec<(f32, f32, f32)>, links: Vec<Link>) -> BgSource {
        let next = link_successors(&links, notes.len());
        BgSource { notes, next }
    }

    fn field(src: BgSource, now: f32) -> BgField {
        let mut f = BgField::default();
        f.reset(0.0);
        f.set_source(src, now);
        f
    }

    #[test]
    fn note_falls_linearly_and_passes_the_bottom_edge() {
        let bottom = RECT[1] + RECT[3];
        // за секунду до посадки, за полсекунды, в момент посадки
        let y0 = note_y(RECT, 10.0, 9.0);
        let y1 = note_y(RECT, 10.0, 9.5);
        let y2 = note_y(RECT, 10.0, 10.0);
        assert!((y2 - bottom).abs() < 1e-4, "note at its own time must sit on the bottom edge: {y2}");
        // равные интервалы времени дают равные шаги по y
        assert!(((y1 - y0) - (y2 - y1)).abs() < 1e-4, "fall must be linear");
        // позже кромки — ниже кромки, без clamp внутри области
        let below = note_y(RECT, 10.0, 10.5);
        assert!(below > bottom + 1.0, "note past the edge must not be clamped: {below}");
        assert!((below - (bottom + RECT[3] * 0.25)).abs() < 1e-4);
    }

    #[test]
    fn note_above_the_region_reaches_the_top_edge_at_full_fall() {
        let top = RECT[1];
        assert!((note_y(RECT, 10.0, 10.0 - BG_FALL) - top).abs() < 1e-4);
    }

    #[test]
    fn source_swap_keeps_the_falling_field() {
        let a = source(vec![(0.0, 0.0, 1.0), (0.5, 1.0, 1.0)], Vec::new());
        let mut f = field(a, 0.0);
        f.advance(0.3, 0.3);
        let before: Vec<_> = f.live().to_vec();
        assert!(!before.is_empty());

        // новая сложность: ноты далеко позже текущего момента превью
        let b = source(
            vec![(2.0, -2.0, 1.0), (2.5, 2.0, 2.0), (9.0, 0.0, 1.0)],
            Vec::new(),
        );
        f.set_source(b, 0.3);

        assert!(f.live().len() > before.len(), "new source must add notes");
        assert_eq!(&f.live()[..before.len()], before.as_slice(), "old notes must stay put");
        let new = &f.live()[before.len()..];
        assert_eq!(new.len(), 1, "only notes inside the fall lead are spawned");
        assert_eq!(new[0].pos, -2.0);
        // посадка новой ноты: clock + (2.0 - 0.3) = 2.0
        assert!((new[0].t - 2.0).abs() < 1e-4);
    }

    #[test]
    fn source_swap_does_not_replay_missed_notes() {
        let a = source(vec![(0.0, 0.0, 1.0)], Vec::new());
        let mut f = field(a, 0.0);
        f.advance(0.2, 0.2);
        let b = source(
            vec![(0.0, -1.0, 1.0), (1.0, 1.0, 1.0)],
            Vec::new(),
        );
        f.set_source(b, 0.9);
        // нота из нового чарта, которая уже «прошла» (0.0), не спавнится
        assert!(f.live().iter().all(|n| n.pos != -1.0));
        assert!(f.live().iter().any(|n| n.pos == 1.0));
    }

    #[test]
    fn slides_link_consecutive_notes_once_each() {
        let notes = vec![(0.0, 0.0, 1.0), (0.1, 1.0, 1.0), (0.2, 2.0, 1.5)];
        let links = vec![Link { notes: vec![0, 1, 2] }];
        let next = link_successors(&links, notes.len());
        assert_eq!(next, vec![Some(1), Some(2), None]);

        let f = field(source(notes, links), 0.0);
        let connectors = f.live().iter().filter(|n| n.next.is_some()).count();
        assert_eq!(connectors, 2, "a 3-note chain has one connector per pair");
        assert_eq!(f.live()[0].next, Some((1.0, 0.1, 1.0)));
        assert_eq!(f.live()[1].next, Some((2.0, 0.2, 1.5)));
    }

    #[test]
    fn dangling_slide_refs_are_ignored() {
        // последняя ссылка битая: её пара не должна создать нить
        let next = link_successors(&[Link { notes: vec![0, 1, 9] }], 2);
        assert_eq!(next, vec![Some(1), None]);
        // связка целиком за пределами списка
        let next = link_successors(&[Link { notes: vec![7, 8] }], 2);
        assert_eq!(next, vec![None, None]);
    }

    #[test]
    fn live_set_is_capped_on_a_dense_chart() {
        let notes: Vec<(f32, f32, f32)> = (0..2000)
            .map(|i| (i as f32 * 0.001, (i % 5) as f32 - 2.0, 1.0))
            .collect();
        let f = field(source(notes, Vec::new()), 0.0);
        assert_eq!(f.live().len(), BG_LIVE_CAP);
        // остались самые свежие (последние в чарте), старые выброшены
        assert_eq!(f.live()[0].pos, (1600 % 5) as f32 - 2.0);
        assert_eq!(f.live()[BG_LIVE_CAP - 1].pos, (1999 % 5) as f32 - 2.0);
    }

    #[test]
    fn draw_cap_limits_bars_per_frame() {
        let len = 400;
        let drawn = (0..len).step_by(draw_stride(len)).count();
        assert!(drawn <= super::BG_DRAW_CAP, "drawn={drawn}");
        assert_eq!(draw_stride(0), 1);
        assert_eq!(draw_stride(10), 1);
    }

    #[test]
    fn notes_are_dropped_only_after_they_leave_the_bottom() {
        let mut f = field(source(vec![(0.0, 0.0, 1.0)], Vec::new()), 0.0);
        assert_eq!(f.live().len(), 1);
        f.advance(0.1, 0.1);
        assert_eq!(f.live().len(), 1, "a note just past its time is still leaving the frame");
        f.advance(0.5, 0.6);
        assert!(f.live().is_empty(), "a note fully below the region must be dropped");
    }

    #[test]
    fn preview_restart_rewinds_the_source_but_keeps_the_field() {
        let src = source(
            vec![(0.0, 0.0, 1.0), (2.0, 2.0, 1.0), (5.0, 1.0, 1.0)],
            Vec::new(),
        );
        let mut f = field(src, 0.0);
        f.advance(5.0, 5.0);
        let before: Vec<_> = f.live().to_vec();
        assert_eq!(before.len(), 1, "only the note at 5s is still inside the fall");
        // повторный клик по песне: превью снова с нуля
        f.advance(1.0 / 60.0, 0.0);
        assert_eq!(f.live()[0], before[0], "falling notes must not teleport");
        assert!(f.live().iter().any(|n| n.pos == 0.0), "chart start emits again");
        assert!(f.live().iter().any(|n| n.pos == 2.0), "the next note is re-emitted too");
    }

    #[test]
    fn empty_source_is_harmless() {
        let f = field(BgSource::default(), 0.0);
        assert!(f.is_empty());
        assert_eq!(draw_stride(0), 1);
    }
}


