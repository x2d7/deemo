//! Визуальная тема меню: палитра Deemo (ночная комната с пианино), тонкие
//! линейки, капс-лейблы и «падающие клавиши» из UI-квадов.
//!
//! Цвета заданы в линейное пространстве (рендер умножает их в linear и пишет в
//! sRGB-таргет), поэтому значения намеренно ниже «экранных».

use crate::bg::BgNote;
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

// --- мотив «миниатюра падающего чарта» ---

/// Максимальный размах нити слайда относительно поля по каждой оси: только
/// короткие локальные диагонали рядом с нотами. Полотно, перечёркнутое
/// линией через пол-экрана, читается как царапина, а не как слайд.
const LINK_SPAN: f32 = 0.28;

/// Нарисовать падающее поле меню: плоские полосы-ноты и тонкие нити слайдов.
///
/// `notes` — живой набор [`crate::bg::BgField`], `now` — его часы. В отличие
/// от старого «окна анализа» здесь нет обрезки по внутреннему прямоугольнику,
/// градиентов и ярких торцов: полоса просто едет вниз, уходит под нижнюю
/// кромку и перекрывается нижней панелью. Ширина/длина зависят только от
/// `size`, поэтому фон читается как мини-поле, а не график.
///
/// `dim` — необязательная зона `(rect, factor)`: квады, попавшие в неё,
/// рисуются с приглушённой альфой. Нужна для титульного блока main-экрана,
/// чтобы вордмарк оставался главным.
///
/// `link_clip` — необязательная область, в которой разрешены нити слайдов
/// (например, колонка списка). Ноты могут быть шире, но длинные линии не
/// должны перечёркивать заголовок или титульный блок.
#[allow(clippy::too_many_arguments)]
pub fn chart_fall(
    ui: &mut Vec<UiQuad>,
    white: usize,
    rect: [f32; 4],
    notes: &[BgNote],
    now: f32,
    strength: f32,
    s: f32,
    dim: Option<([f32; 4], f32)>,
    link_clip: Option<[f32; 4]>,
) {
    let (x0, top, w, h) = (rect[0], rect[1], rect[2], rect[3]);
    if w <= 0.0 || h <= 0.0 || strength <= 0.0 || notes.is_empty() {
        return;
    }
    let bottom = top + h;
    let dim_factor = |r: [f32; 4]| -> f32 {
        match dim {
            Some((d, k))
                if r[0] + r[2] * 0.5 >= d[0]
                    && r[0] + r[2] * 0.5 <= d[0] + d[2]
                    && r[1] + r[3] * 0.5 >= d[1]
                    && r[1] + r[3] * 0.5 <= d[1] + d[3] =>
            {
                k
            }
            _ => 1.0,
        }
    };
    let stride = crate::bg::draw_stride(notes.len());
    let mut bars: Vec<([f32; 4], Option<([f32; 2], [f32; 2])>)> =
        Vec::with_capacity(notes.len() / stride + 1);
    for n in notes.iter().step_by(stride) {
        let anchor = crate::bg::note_y(rect, n.t, now);
        let sz = n.size.clamp(0.5, 2.0);
        // мини-нота, а не гистограммная палочка: заметная толщина, короткая
        // длина; размер чарта меняет обе, но в пределах клампа
        let bw = (8.0 + 7.0 * sz) * s;
        let bh = (24.0 + 9.0 * sz) * s;
        // верх полосы ниже нижней кромки — нота уже ушла; нижний край выше
        // верхней кромки — ещё не вошла. Никаких фейдов на кромках.
        if anchor < top || anchor - bh >= bottom {
            continue;
        }
        let cx = x0 + ((n.pos.clamp(-2.0, 2.0) + 2.0) / 4.0) * w;
        let bx = (cx - bw * 0.5).clamp(x0, (x0 + w - bw).max(x0));
        // Поле начинается под верхней панелью: полоса не имеет права
        // выглядывать над top. Часть, вошедшая в кадр, обрезается по кромке,
        // а не рисуется поверх header/search.
        let bar_top = anchor - bh;
        let y0 = bar_top.max(top);
        let y1 = anchor.min(bottom);
        if y1 <= y0 {
            continue;
        }
        let bar = [bx, y0, bw, y1 - y0];
        // Нить слайда: только короткая локальная диагональ. Оба конца должны
        // быть в поле, а размах по вертикали и горизонтали — небольшим
        // (LINK_SPAN). Связка из нескольких нот даёт несколько таких отрезков.
        let link = n.next.and_then(|(npos, nt, nsize)| {
            let nsz = nsize.clamp(0.5, 2.0);
            let nbh = (24.0 + 9.0 * nsz) * s;
            let nanchor = crate::bg::note_y(rect, nt, now);
            let c1 = anchor - bh * 0.5;
            let c2 = nanchor - nbh * 0.5;
            let nx = x0 + ((npos.clamp(-2.0, 2.0) + 2.0) / 4.0) * w;
            if c1 < top || c1 > bottom || c2 < top || c2 > bottom {
                return None;
            }
            if nx < x0 || nx > x0 + w {
                return None;
            }
            // нить не должна вылезать из разрешённой области (колонка списка):
            // обе ноты обязаны быть в ней целиком
            let clip = link_clip.unwrap_or(rect);
            let (cx0, cy0, cw, ch) = (clip[0], clip[1], clip[2], clip[3]);
            if cx < cx0 || cx > cx0 + cw || nx < cx0 || nx > cx0 + cw {
                return None;
            }
            if c1 < cy0 || c1 > cy0 + ch || c2 < cy0 || c2 > cy0 + ch {
                return None;
            }
            if (c2 - c1).abs() > h * LINK_SPAN || (nx - cx).abs() > w * LINK_SPAN {
                return None;
            }
            Some(([cx, c1], [nx, c2]))
        });
        bars.push((bar, link));
    }
    // сначала нити (под нотами), потом сами полосы — как в игровом поле.
    // Нить заметно бледнее нот: это подсказка о слайде, а не второй слой
    // графики.
    for (_, link) in &bars {
        if let Some((p, q)) = link {
            let mid = [(p[0] + q[0]) * 0.5, (p[1] + q[1]) * 0.5];
            let alpha = strength * 0.10 * dim_factor([mid[0], mid[1], 0.0, 0.0]);
            ribbon(ui, white, *p, *q, s, [0.84, 0.80, 0.72, alpha]);
        }
    }
    for (bar, _) in &bars {
        let a = strength * dim_factor(*bar);
        fill(ui, white, *bar, [0.84, 0.80, 0.72, a]);
    }
}

/// Отрезок под произвольным углом: UI-квад не умеет поворот, поэтому линия
/// собирается из полос, идущих вдоль доминирующей оси. Полосы стыкуются без
/// наложения, иначе перекрытие альфы даёт «бусины» на прямой.
fn ribbon(ui: &mut Vec<UiQuad>, white: usize, a: [f32; 2], b: [f32; 2], s: f32, color: [f32; 4]) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    if dx.abs() < 0.5 && dy.abs() < 0.5 {
        return;
    }
    let thick = (2.0 * s).max(1.0);
    let band = (1.5 * s).max(1.0);
    if dy.abs() >= dx.abs() {
        // вертикальная доминанта: горизонтальные полосы, шаг по y
        let (a, b) = if a[1] <= b[1] { (a, b) } else { (b, a) };
        let h_total = b[1] - a[1];
        let n = ((h_total / band).ceil() as usize).clamp(1, 240);
        let h = h_total / n as f32;
        for i in 0..n {
            let u0 = i as f32 / n as f32;
            let u1 = (i + 1) as f32 / n as f32;
            let x0 = a[0] + (b[0] - a[0]) * u0;
            let x1 = a[0] + (b[0] - a[0]) * u1;
            fill(
                ui,
                white,
                [
                    x0.min(x1) - thick * 0.5,
                    a[1] + h_total * u0,
                    (x0 - x1).abs() + thick,
                    h.max(0.5),
                ],
                color,
            );
        }
    } else {
        // горизонтальная доминанта: вертикальные полосы, шаг по x
        let (a, b) = if a[0] <= b[0] { (a, b) } else { (b, a) };
        let w_total = b[0] - a[0];
        let n = ((w_total / band).ceil() as usize).clamp(1, 240);
        let w = w_total / n as f32;
        for i in 0..n {
            let u0 = i as f32 / n as f32;
            let u1 = (i + 1) as f32 / n as f32;
            let y0 = a[1] + (b[1] - a[1]) * u0;
            let y1 = a[1] + (b[1] - a[1]) * u1;
            fill(
                ui,
                white,
                [
                    a[0] + w_total * u0,
                    y0.min(y1) - thick * 0.5,
                    w.max(0.5),
                    (y0 - y1).abs() + thick,
                ],
                color,
            );
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
    use super::chart_fall;
    use crate::bg::BgNote;

    const RECT: [f32; 4] = [0.0, 50.0, 400.0, 300.0];

    fn note(t: f32) -> BgNote {
        BgNote {
            t,
            pos: 0.0,
            size: 1.0,
            next: None,
        }
    }

    #[test]
    fn note_is_one_flat_quad_without_cap_or_ramp() {
        let mut ui = Vec::new();
        chart_fall(&mut ui, 1, RECT, &[note(10.0)], 10.0, 0.1, 1.0, None, None);
        assert_eq!(ui.len(), 1, "a note must be one flat rectangle, not a body + head");
        assert_eq!(ui[0].color[3], 0.1, "uniform ink: no per-bar alpha ramp");
    }

    #[test]
    fn note_past_the_bottom_is_gone_not_clamped() {
        let mut ui = Vec::new();
        // кромка: полоса ещё видна и не обрезана внутренним rect
        chart_fall(&mut ui, 1, RECT, &[note(10.0)], 10.1, 0.1, 1.0, None, None);
        assert_eq!(ui.len(), 1);
        assert!(ui[0].rect[1] > RECT[1] + RECT[3] - 60.0);
        // ушла целиком — не рисуем вовсе
        ui.clear();
        chart_fall(&mut ui, 1, RECT, &[note(10.0)], 11.0, 0.1, 1.0, None, None);
        assert!(ui.is_empty(), "a note below the region must vanish, not stick to an edge");
    }

    #[test]
    fn bar_enters_below_the_top_edge_without_touching_the_header() {
        let mut ui = Vec::new();
        // нота ровно на верхней кромке поля: её полоса целиком выше кадра
        chart_fall(&mut ui, 1, RECT, &[note(12.0)], 10.0, 0.1, 1.0, None, None);
        assert!(ui.is_empty(), "a note at the top edge must not paint into the header");
        // чуть ниже: видимая часть обрезана по top, а не торчит над ним
        ui.clear();
        chart_fall(&mut ui, 1, RECT, &[note(11.9)], 10.0, 0.1, 1.0, None, None);
        assert_eq!(ui.len(), 1);
        assert!(
            ui[0].rect[1] >= RECT[1] - 1e-3,
            "bar top {} must stay below field top {}",
            ui[0].rect[1],
            RECT[1]
        );
        assert!(
            ui[0].rect[1] + ui[0].rect[3] <= RECT[1] + RECT[3] + 1e-3,
            "bar must stay inside the field"
        );
    }

    #[test]
    fn slide_draws_the_note_and_a_faint_local_connector() {
        let mut ui = Vec::new();
        let mut n = note(10.0);
        n.next = Some((1.0, 10.3, 1.0));
        chart_fall(&mut ui, 1, RECT, &[n], 10.0, 0.1, 1.0, None, None);
        assert!(ui.len() > 1, "connector must add quads under the bar");
        // последний квад — сама нота поверх нити, нить заметно бледнее
        let last = ui.last().unwrap();
        assert_eq!(last.color[3], 0.1);
        assert!(last.rect[3] > 20.0);
        assert!(
            ui[0].color[3] < last.color[3] * 0.2,
            "ribbon must be much lower alpha than the notes"
        );
    }

    #[test]
    fn screen_spanning_slide_is_skipped() {
        let mut ui = Vec::new();
        let mut n = note(10.0);
        // прыжок через пол-поля: такая «царапина» через весь экран не рисуется
        n.next = Some((2.0, 10.3, 1.0));
        chart_fall(&mut ui, 1, RECT, &[n], 10.0, 0.1, 1.0, None, None);
        assert_eq!(ui.len(), 1, "only the note bar remains");
    }

    #[test]
    fn connector_outside_the_link_clip_is_skipped() {
        let mut ui = Vec::new();
        let mut n = note(10.0);
        n.next = Some((1.0, 10.3, 1.0));
        // обе ноты в поле, но вторая — уже за разрешённой колонкой списка
        chart_fall(
            &mut ui,
            1,
            RECT,
            &[n],
            10.0,
            0.1,
            1.0,
            None,
            Some([0.0, 50.0, 250.0, 300.0]),
        );
        assert_eq!(ui.len(), 1, "a ribbon must not leave the list column");
    }
}

