//! Визуальная тема меню: палитра Deemo (ночная комната с пианино), тонкие
//! линейки, капс-лейблы и «падающие клавиши» из UI-квадов.
//!
//! Цвета заданы в линейное пространстве (рендер умножает их в linear и пишет в
//! sRGB-таргет), поэтому значения намеренно ниже «экранных».

use crate::render::{Renderer, UiQuad};
use crate::text::{Face, Text};

// --- палитра ---

/// Фон меню: почти чёрный, чуть холодный.
pub const BG: [f32; 4] = [0.0038, 0.0038, 0.0052, 1.0];
/// Приподнятая панель (накладывается на BG).
pub const PANEL: [f32; 4] = [0.84, 0.80, 0.72, 0.035];
/// Более заметная панель/наведение.
pub const PANEL_HI: [f32; 4] = [0.84, 0.80, 0.72, 0.050];
/// Тёплая «бумага»: основной цвет текста.
pub const INK: [f32; 4] = [0.84, 0.80, 0.72, 1.0];
/// Яркий ink для акцентов и активных строк.
pub const INK_HI: [f32; 4] = [0.97, 0.94, 0.88, 1.0];
/// Вторичный текст.
pub const INK_DIM: [f32; 4] = [0.84, 0.80, 0.72, 0.64];
/// Третичный текст (метаданные).
pub const INK_FAINT: [f32; 4] = [0.84, 0.80, 0.72, 0.42];
/// Еле видная заливка/рамка.
pub const GHOST: [f32; 4] = [0.84, 0.80, 0.72, 0.10];
/// Тонкая линейка.
pub const RULE: [f32; 4] = [0.84, 0.80, 0.72, 0.16];
/// Заметная линейка/рамка.
pub const RULE_STRONG: [f32; 4] = [0.84, 0.80, 0.72, 0.34];
/// Единственный акцент — тёплый янтарь (свеча/золотые листья).
pub const ACCENT: [f32; 4] = [0.69, 0.44, 0.12, 1.0];
/// Янтарный текст (читаемее на чёрном, чем ACCENT).
pub const ACCENT_INK: [f32; 4] = [0.94, 0.76, 0.40, 1.0];
/// Текст/иконки на светлой кнопке.
pub const ON_INK: [f32; 4] = [0.004, 0.004, 0.005, 1.0];
pub const DANGER_INK: [f32; 4] = [0.90, 0.42, 0.34, 1.0];
/// Холодный акцент для вторичных данных (гистограмма таймингов).
pub const COOL: [f32; 4] = [0.24, 0.42, 0.62, 1.0];

/// Цвет ранга в тёмной теме.
pub fn rank_color(rank: &str) -> [f32; 4] {
    match rank {
        "SS" => [0.98, 0.82, 0.42, 1.0],
        "S" => [0.94, 0.70, 0.30, 1.0],
        "A" => [0.84, 0.80, 0.72, 1.0],
        "B" => [0.50, 0.60, 0.72, 1.0],
        "C" => [0.62, 0.52, 0.68, 1.0],
        _ => [0.52, 0.52, 0.52, 1.0],
    }
}

// --- масштаб ---

/// Опорное разрешение меню: при нём масштаб ровно 1.0 и вся композиция
/// выглядит так, как выглядела до появления глобального масштаба.
pub const BASE_W: f32 = 1280.0;
pub const BASE_H: f32 = 720.0;

/// Единый масштаб всего меню. Учитываются обе стороны кадра, поэтому
/// ультравайд (3440×1440) не растягивает интерфейс по ширине, а маленькие
/// экраны (1024×600) не обрезают его. 1280×720 → ровно 1.0.
///
/// Нижняя граница 0.75 нужна, чтобы на совсем маленьком окне UI оставался
/// читаемым; верхняя 3.0 — предел разумного кегля для 4K.
pub fn ui_scale(w: f32, h: f32) -> f32 {
    (w / BASE_W).min(h / BASE_H).clamp(0.75, 3.0)
}

/// Толщина хайрлайна при масштабе `s`: на 1280×720 — 1 px, на 4K — 3 px.
/// Ниже 1 px не опускаемся, иначе линия начнёт пропадать.
pub fn hairline(s: f32) -> f32 {
    s.max(1.0)
}

// --- примитивы ---

pub fn fill(ui: &mut Vec<UiQuad>, white: usize, rect: [f32; 4], color: [f32; 4]) {
    if rect[2] <= 0.0 || rect[3] <= 0.0 {
        return;
    }
    ui.push(UiQuad {
        tex: white,
        rect,
        uv: [0.0, 0.0, 1.0, 1.0],
        color,
    });
}

pub fn hair_h(ui: &mut Vec<UiQuad>, white: usize, x: f32, y: f32, w: f32, color: [f32; 4], s: f32) {
    fill(ui, white, [x, y, w, hairline(s)], color);
}

pub fn hair_v(ui: &mut Vec<UiQuad>, white: usize, x: f32, y: f32, h: f32, color: [f32; 4], s: f32) {
    fill(ui, white, [x, y, hairline(s), h], color);
}

/// Рамка толщиной [`hairline`].
pub fn frame(ui: &mut Vec<UiQuad>, white: usize, r: [f32; 4], color: [f32; 4], s: f32) {
    let t = hairline(s);
    hair_h(ui, white, r[0], r[1], r[2], color, s);
    hair_h(ui, white, r[0], r[1] + r[3] - t, r[2], color, s);
    hair_v(ui, white, r[0], r[1], r[3], color, s);
    hair_v(ui, white, r[0] + r[2] - t, r[1], r[3], color, s);
}

/// Короткие угловые засечки (рамка-«видоискатель» вокруг фокуса).
pub fn corner_ticks(
    ui: &mut Vec<UiQuad>,
    white: usize,
    r: [f32; 4],
    color: [f32; 4],
    len: f32,
    s: f32,
) {
    let t = hairline(s);
    let (x, y, w, h) = (r[0], r[1], r[2], r[3]);
    hair_h(ui, white, x, y, len, color, s);
    hair_v(ui, white, x, y, len, color, s);
    hair_h(ui, white, x + w - len, y, len, color, s);
    hair_v(ui, white, x + w - t, y, len, color, s);
    hair_h(ui, white, x, y + h - t, len, color, s);
    hair_v(ui, white, x, y + h - len, len, color, s);
    hair_h(ui, white, x + w - len, y + h - t, len, color, s);
    hair_v(ui, white, x + w - t, y + h - len, len, color, s);
}

// --- типографика ---

/// Мелкий капс-лейбл (Dragon Serial) с трекингом.
pub fn label(
    text: &Text,
    ui: &mut Vec<UiQuad>,
    s: &str,
    x: f32,
    y: f32,
    size: f32,
    tracking: f32,
    color: [f32; 4],
) {
    text.draw_tracked(Face::Serif, s, x, y, size, tracking, color, ui);
}

pub fn label_width(text: &Text, s: &str, size: f32, tracking: f32) -> f32 {
    text.measure_tracked(Face::Serif, s, size, tracking)
}

/// Секционный заголовок: капс-лейбл + тонкая линейка до правого края.
pub fn section(
    text: &Text,
    ui: &mut Vec<UiQuad>,
    white: usize,
    s: &str,
    x: f32,
    y: f32,
    right: f32,
    color: [f32; 4],
    scale: f32,
) {
    label(text, ui, s, x, y, 11.0 * scale, 2.6 * scale, color);
    let w = label_width(text, s, 11.0 * scale, 2.6 * scale);
    let gap = 14.0 * scale;
    hair_h(ui, white, x + w + gap, y + 7.0 * scale, (right - x - w - gap).max(0.0), RULE, scale);
}

/// Вид кнопки.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Btn {
    /// Светлая заливка, тёмный текст (главное действие).
    Primary,
    /// Прозрачная с рамкой.
    Ghost,
}

/// Кнопка; `key` — подсказка клавиши справа внутри кнопки.
pub fn button(
    text: &Text,
    ui: &mut Vec<UiQuad>,
    white: usize,
    r: [f32; 4],
    caption: &str,
    kind: Btn,
    key: Option<&str>,
    s: f32,
) {
    let (fg, key_col) = match kind {
        Btn::Primary => {
            fill(ui, white, r, INK);
            (ON_INK, [ON_INK[0], ON_INK[1], ON_INK[2], 0.55])
        }
        Btn::Ghost => {
            fill(ui, white, r, PANEL);
            frame(ui, white, r, RULE_STRONG, s);
            (INK, INK_FAINT)
        }
    };
    let mut size = 12.0 * s;
    let mut tracking = 2.4 * s;
    let mut cw = text.measure_tracked(Face::Serif, caption, size, tracking);
    let kw = key
        .map(|k| text.measure_tracked(Face::Serif, k, 9.0 * s, 1.4 * s) + 12.0 * s)
        .unwrap_or(0.0);
    if cw + kw > r[2] - 20.0 * s {
        size = 10.0 * s;
        tracking = 1.5 * s;
        cw = text.measure_tracked(Face::Serif, caption, size, tracking);
    }
    if cw + kw > r[2] - 16.0 * s {
        tracking = 0.6 * s;
        cw = text.measure_tracked(Face::Serif, caption, size, tracking);
    }
    let cap_y = r[1] + (r[3] - size) * 0.5 - 1.0 * s;
    if kw > 0.0 {
        let total = cw + kw;
        let x0 = r[0] + (r[2] - total) * 0.5;
        text.draw_tracked(Face::Serif, caption, x0, cap_y, size, tracking, fg, ui);
        if let Some(k) = key {
            text.draw_tracked(Face::Serif, k, x0 + cw + 12.0 * s, r[1] + (r[3] - 9.0 * s) * 0.5 - 1.0 * s, 9.0 * s, 1.4 * s, key_col, ui);
        }
    } else {
        text.draw_centered_tracked(Face::Serif, caption, r[0] + r[2] * 0.5, cap_y, size, tracking, fg, ui);
    }
}

/// Маленький треугольник «play» из горизонтальных полос (без юникод-глифа).
pub fn play_glyph(ui: &mut Vec<UiQuad>, white: usize, x: f32, y: f32, size: f32, color: [f32; 4]) {
    let steps = (size.round() as i32).max(3);
    let max_w = size * 0.62;
    for i in 0..steps {
        let t = (i as f32 + 0.5) / steps as f32;
        let h = size / steps as f32 + 0.5;
        let w = max_w * (1.0 - (2.0 * t - 1.0).abs());
        fill(ui, white, [x, y + t * size, w.max(0.5), h], color);
    }
}

// --- мотив «падающие клавиши» ---

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 % 10_000) as f32 / 10_000.0
    }
}

/// Вертикальные полупрозрачные полосы, медленно падающие вниз, — намёк на
/// падающие клавиши Deemo. Рисуется только UI-квадами, очень дешёвый.
#[allow(clippy::too_many_arguments)]
pub fn falling_keys(
    ui: &mut Vec<UiQuad>,
    white: usize,
    x0: f32,
    x1: f32,
    top: f32,
    bottom: f32,
    t: f32,
    seed: u32,
    strength: f32,
    s: f32,
) {
    if x1 <= x0 || bottom <= top {
        return;
    }
    let mut rng = Rng(seed.wrapping_mul(2_654_435_761).max(1));
    let span_x = x1 - x0;
    let span_y = bottom - top;
    for _ in 0..10 {
        let cx = x0 + rng.next() * span_x;
        let bw = (6.0 + rng.next() * 18.0) * s;
        let len = (70.0 + rng.next() * span_y * 0.55) * s;
        let speed = (7.0 + rng.next() * 9.0) * s;
        let phase = rng.next();
        let total = span_y + len;
        let y = top - len + (phase * total + speed * t).rem_euclid(total);
        // полоса обрезается границами области
        let y0 = y.max(top);
        let y1 = (y + len).min(bottom);
        if y1 <= y0 {
            continue;
        }
        let a = strength * (0.45 + rng.next() * 0.55);
        fill(ui, white, [cx, y0, bw, y1 - y0], [0.84, 0.80, 0.72, a]);
        // «торец» клавиши чуть ярче
        let cap_y = y + len - 2.0 * s;
        if cap_y >= top && cap_y + 2.0 * s <= bottom {
            fill(ui, white, [cx, cap_y, bw, 2.0 * s], [0.84, 0.80, 0.72, a * 1.8]);
        }
    }
}

// --- мотив «настоящие ноты песни» ---

/// Сколько секунд до «момента ноты» видно в фоне меню.
pub const NOTE_LEAD: f32 = 3.5;
/// Потолок полос за кадр: плотный чарт не должен превращаться в стену.
pub const NOTE_KEY_MAX: usize = 48;
/// Нота перестаёт рисоваться чуть раньше `t`, чтобы не мигать на нижней кромке.
const NOTE_TAIL: f32 = 0.15;

/// Прямоугольники полос для нот из окна `[t - 0.15, t + lead]`.
///
/// Чистая функция (без GPU) — по ней же идут тесты. `rect` = `[x, top, w, h]`,
/// `notes` отсортированы по времени. Полоса «приходит» в момент ноты: её
/// нижний торец стоит на нижней границе `rect`, когда `time == t`, и на
/// верхней, когда `time == t + lead`. Сами полосы не обрезаются по `rect` —
/// это делает [`note_keys`] при рисовании.
///
/// Позиция ноты (-2..2) отображается на всю ширину, размер задаёт толщину и
/// длину полосы. Если в окне больше [`NOTE_KEY_MAX`] нот, берётся каждая N-я.
pub fn note_key_rects(
    rect: [f32; 4],
    notes: &[(f32, f32, f32)],
    t: f32,
    lead: f32,
    s: f32,
) -> Vec<[f32; 4]> {
    let (x0, top, w, h) = (rect[0], rect[1], rect[2], rect[3]);
    if w <= 0.0 || h <= 0.0 || lead <= 0.0 || notes.is_empty() {
        return Vec::new();
    }
    let from = t - NOTE_TAIL;
    let to = t + lead;
    let start = notes.partition_point(|n| n.0 < from);
    let end = notes.partition_point(|n| n.0 <= to);
    if start >= end {
        return Vec::new();
    }
    let count = end - start;
    let stride = count.div_ceil(NOTE_KEY_MAX).max(1);
    let mut out = Vec::with_capacity(count.div_ceil(stride));
    for n in notes[start..end].iter().step_by(stride) {
        let (time, pos, size) = *n;
        if !time.is_finite() || !pos.is_finite() || !size.is_finite() {
            continue;
        }
        // 1 — нота на нижней кромке (момент t), 0 — только вошла сверху
        let u = ((time - t) / lead).clamp(0.0, 1.0);
        let anchor = top + (1.0 - u) * h;
        // размер ноты обычно 1..2; толщина 9..14 px, длина 30..38 px —
        // вытянутая «клавиша», а не квадрат, иначе фон читается как мусор UI
        let sz = size.clamp(0.5, 2.0);
        let bw = (5.0 + 4.5 * sz) * s;
        let bh = (22.0 + 8.0 * sz) * s;
        let cx = x0 + ((pos.clamp(-2.0, 2.0) + 2.0) / 4.0) * w;
        let bx = (cx - bw * 0.5).clamp(x0, (x0 + w - bw).max(x0));
        out.push([bx, anchor - bh, bw, bh]);
    }
    out
}

/// То же, что [`falling_keys`], но полосы — настоящие ноты песни, а их
/// вертикальная позиция привязана к позиции превью. Альфа плавно проявляется
/// у верхней кромки и тает к нижней, чтобы фон «дышал», а не стоял стеной.
#[allow(clippy::too_many_arguments)]
pub fn note_keys(
    ui: &mut Vec<UiQuad>,
    white: usize,
    rect: [f32; 4],
    notes: &[(f32, f32, f32)],
    t: f32,
    lead: f32,
    strength: f32,
    s: f32,
) {
    let (top, h) = (rect[1], rect[3]);
    if strength <= 0.0 || h <= 0.0 {
        return;
    }
    let (clip_top, clip_bottom) = (rect[1], rect[1] + rect[3]);
    for r in note_key_rects(rect, notes, t, lead, s) {
        let anchor = r[1] + r[3];
        let u = ((anchor - top) / h).clamp(0.0, 1.0);
        let fade = (u / 0.08).min(1.0) * ((1.0 - u) / 0.16).min(1.0);
        let a = strength * fade;
        if a <= 0.0 {
            continue;
        }
        let y0 = clip_top.max(r[1]);
        let y1 = clip_bottom.min(anchor);
        if y1 <= y0 {
            continue;
        }
        fill(ui, white, [r[0], y0, r[2], y1 - y0], [0.84, 0.80, 0.72, a]);
        // торец-«голова» ноты чуть ярче — тот же язык, что у случайных полос
        let cap_y = (anchor - 2.0 * s).max(y0);
        if y1 > cap_y {
            fill(ui, white, [r[0], cap_y, r[2], y1 - cap_y], [0.84, 0.80, 0.72, (a * 1.8).min(1.0)]);
        }
    }
}

/// Плавные градиенты (текстура-маска), чтобы фейды арта не полосились.
/// Возвращает (горизонтальный, вертикальный) handles UI-текстур.
pub fn make_fades(renderer: &mut Renderer) -> (usize, usize) {
    const N: u32 = 256;
    let mut hx = vec![255u8; (N * 4) as usize];
    for x in 0..N {
        let t = x as f32 / (N - 1) as f32;
        let a = ((1.0 - t) * (1.0 - t) * 255.0) as u8;
        let i = (x * 4) as usize;
        hx[i + 3] = a;
    }
    let fx = renderer.add_ui_texture("fade-x", N, 1, &hx);
    let mut hy = vec![255u8; (N * 4) as usize];
    for y in 0..N {
        let t = y as f32 / (N - 1) as f32;
        // 0 сверху → 1 снизу (мягкое затемнение к низу)
        let a = (t * t * 255.0) as u8;
        let i = (y * 4) as usize;
        hy[i + 3] = a;
    }
    let fy = renderer.add_ui_texture("fade-y", 1, N, &hy);
    (fx, fy)
}

/// Горизонтальный фейд в цвет `color` (альфа 1 слева → 0 справа).
pub fn fade_x(ui: &mut Vec<UiQuad>, tex: usize, rect: [f32; 4], color: [f32; 4]) {
    ui.push(UiQuad {
        tex,
        rect,
        uv: [0.0, 0.0, 1.0, 1.0],
        color,
    });
}

/// Вертикальный фейд (альфа 0 сверху → 1 снизу).
pub fn fade_y(ui: &mut Vec<UiQuad>, tex: usize, rect: [f32; 4], color: [f32; 4]) {
    ui.push(UiQuad {
        tex,
        rect,
        uv: [0.0, 0.0, 1.0, 1.0],
        color,
    });
}
#[cfg(test)]
mod tests {
    use super::{NOTE_KEY_MAX, NOTE_LEAD, note_key_rects};

    const RECT: [f32; 4] = [100.0, 50.0, 400.0, 300.0];

    fn bottom(r: [f32; 4]) -> f32 {
        r[1] + r[3]
    }

    #[test]
    fn note_at_now_lands_on_the_bottom_edge() {
        let r = note_key_rects(RECT, &[(10.0, 0.0, 1.0)], 10.0, NOTE_LEAD, 1.0);
        assert_eq!(r.len(), 1);
        assert!(
            (bottom(r[0]) - (RECT[1] + RECT[3])).abs() < 1e-3,
            "note at t must sit on the bottom edge, got {}",
            bottom(r[0])
        );
    }

    #[test]
    fn note_at_lead_lands_on_the_top_edge() {
        // нота через lead секунд — уже у верхней кромки
        let r = note_key_rects(RECT, &[(10.0 + NOTE_LEAD, 0.0, 1.0)], 10.0, NOTE_LEAD, 1.0);
        assert_eq!(r.len(), 1);
        assert!(
            (bottom(r[0]) - RECT[1]).abs() < 1e-3,
            "note at t+lead must sit on the top edge, got {}",
            bottom(r[0])
        );
    }

    #[test]
    fn time_maps_monotonically_between_edges() {
        let notes = [(10.0, 0.0, 1.0), (11.0, 0.0, 1.0), (12.0, 0.0, 1.0)];
        let r = note_key_rects(RECT, &notes, 10.0, NOTE_LEAD, 1.0);
        assert_eq!(r.len(), 3);
        // более поздняя нота всегда выше (y меньше)
        assert!(bottom(r[0]) > bottom(r[1]));
        assert!(bottom(r[1]) > bottom(r[2]));
    }

    #[test]
    fn pos_maps_left_to_right_and_clamps_past_two() {
        let notes = [(11.0, -3.0, 1.0), (11.0, -1.0, 1.0), (11.0, 0.0, 1.0), (11.0, 1.0, 1.0), (11.0, 3.0, 1.0)];
        let r = note_key_rects(RECT, &notes, 10.0, NOTE_LEAD, 1.0);
        assert_eq!(r.len(), 5);
        for pair in r.windows(2) {
            assert!(pair[0][0] < pair[1][0], "pos must map left → right");
        }
        // |pos| > 2 упирается в те же края, что и ровно ±2
        let edge = note_key_rects(
            RECT,
            &[(11.0, -2.0, 1.0), (11.0, 2.0, 1.0)],
            10.0,
            NOTE_LEAD,
            1.0,
        );
        assert!((r[0][0] - edge[0][0]).abs() < 1e-3);
        assert!((r[4][0] - edge[1][0]).abs() < 1e-3);
        // и не вылезает за rect по горизонтали
        for q in &r {
            assert!(q[0] >= RECT[0] - 1e-3);
            assert!(q[0] + q[2] <= RECT[0] + RECT[2] + 1e-3);
        }
    }

    #[test]
    fn notes_outside_the_window_are_skipped() {
        let notes = [
            (9.0, 0.0, 1.0),                          // уже позади (t - 1.0)
            (11.0, 0.0, 1.0),                         // в окне
            (10.0 + NOTE_LEAD + 0.5, 0.0, 1.0),       // ещё слишком далеко
        ];
        let r = note_key_rects(RECT, &notes, 10.0, NOTE_LEAD, 1.0);
        assert_eq!(r.len(), 1, "only the note inside the window is drawn");
        assert!((bottom(r[0]) - (RECT[1] + RECT[3] * (1.0 - 1.0 / NOTE_LEAD))).abs() < 1e-3);
    }

    #[test]
    fn empty_input_gives_empty_output() {
        assert!(note_key_rects(RECT, &[], 10.0, NOTE_LEAD, 1.0).is_empty());
        // вырожденный rect тоже ничего не рисует
        assert!(note_key_rects([0.0, 0.0, 0.0, 300.0], &[(10.0, 0.0, 1.0)], 10.0, NOTE_LEAD, 1.0).is_empty());
        assert!(note_key_rects([0.0, 0.0, 400.0, 0.0], &[(10.0, 0.0, 1.0)], 10.0, NOTE_LEAD, 1.0).is_empty());
    }

    #[test]
    fn dense_chart_is_capped_per_frame() {
        let notes: Vec<(f32, f32, f32)> = (0..500)
            .map(|i| (10.0 + i as f32 * NOTE_LEAD / 600.0, (i % 5) as f32 - 2.0, 1.0))
            .collect();
        let r = note_key_rects(RECT, &notes, 10.0, NOTE_LEAD, 1.0);
        assert!(!r.is_empty());
        assert!(r.len() <= NOTE_KEY_MAX, "cap must hold, got {}", r.len());
        // выборка всё ещё покрывает всё окно, а не только начало
        assert!(bottom(r[0]) > RECT[1] + RECT[3] * 0.85);
        assert!(bottom(r[r.len() - 1]) < RECT[1] + RECT[3] * 0.2);
    }
}
