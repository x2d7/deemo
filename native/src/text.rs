//! Растровый текст через fontdue: кеш глифов по фактическому кеглю + UI-квады.
//!
//! В игре три гарнитуры (Deemo-набор из Deenote):
//! * [`Face::Display`] — Fantique Snails: декоративный шрифт для вордмарка,
//!   крупных заголовков и ранга (латиница, без кириллицы);
//! * [`Face::Serif`] — Dragon Serial: сдержанная антиква для подписей,
//!   кнопок, чисел и мелких капс-лейблов;
//! * [`Face::Body`] — Noto Sans: читаемый текст, значения и кириллица.
//!
//! У каждой гарнитуры свой набор глифов. Если в выбранной гарнитуре нет глифа
//! (например, кириллица в декоративной), он берётся из следующей по цепочке:
//! Display → Serif → Body. API для игрового HUD (`draw`/`measure`/
//! `draw_centered`) остался прежним и рисует Body-шрифтом.
//!
//! # Почему кегль, а не один «базовый» атлас
//!
//! Раньше каждая гарнитура растеризовалась один раз в фиксированном кегле
//! (Display 96, Serif 76, Body 64 px), а глифы масштабировались в размер
//! отрисовки. Уменьшение 64 → 13 px идёт без мип-уровней: билинейный фильтр
//! берёт 2×2 текселя из области ~5×5 — отсюда «зерно», рваные края и
//! нечитаемые мелкие подписи. Крупный текст (ранг на 1920×1200 рисуется
//! кеглем ~159 px) наоборот растягивался из 96 px и мылился.
//!
//! Теперь глиф растеризуется ровно в тот пиксельный кегль, в котором он
//! рисуется (`round(size)`), и кешируется по ключу (гарнитура, кегль, символ).
//! Квад ставится в целые пиксели, его размер равен размеру битмапа, поэтому
//! тексель атласа попадает в пиксель экрана 1:1 и фильтрация ничего не мажет.
//! UI раскладывается в физических пикселях (`window.inner_size()` →
//! `Renderer::set_ui_size`), значит `size` — это уже физический кегль, и DPI
//! (scale factor winit) учтён самим окном: на HiDPI буфер больше, а глифы
//! растеризуются в его настоящих пикселях. Если UI когда-нибудь переведут на
//! логические координаты, в [`px_for`] нужно домножить `size` на scale factor —
//! кеш всё равно останется корректным, потому что ключ — физический кегль.
//!
//! Глифы догружаются в атлас лениво: `draw*` только раскладывает квады и
//! копит изменения, [`Text::flush`] один раз за кадр переносит их в GPU.

use crate::render::{Renderer, UiQuad};
use anyhow::{Context, Result};
use fontdue::{Font, Metrics};
use std::cell::RefCell;
use std::collections::HashMap;

/// Страница атласа: 1024×1024 RGBA (4 МиБ). Одна страница покрывает все
/// встречающиеся в интерфейсе кегли; при переполнении берётся следующая.
const PAGE_W: u32 = 1024;
const PAGE_H: u32 = 1024;
/// Сколько страниц резервируется на гарнитуру. Текстура создаётся только
/// тогда, когда на странице реально появляется первый глиф.
const PAGES_PER_FACE: usize = 4;
/// Кегли вне диапазона не растеризуем (защита от абсурдных размеров).
const MIN_PX: u32 = 5;
const MAX_PX: u32 = 512;

/// Гарнитура. Порядок вариантов — цепочка фолбэка для отсутствующих глифов.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Face {
    /// Fantique Snails — вордмарк, крупные заголовки, ранг.
    Display,
    /// Dragon Serial — капс-лейблы, кнопки, числа.
    Serif,
    /// Noto Sans — основной текст, кириллица, HUD.
    Body,
}

impl Face {
    fn index(self) -> usize {
        match self {
            Face::Display => 0,
            Face::Serif => 1,
            Face::Body => 2,
        }
    }
}

/// Глиф, уже лежащий в странице атласа. Все размеры — в пикселях растра,
/// то есть ровно в тех, в которых глиф будет нарисован.
#[derive(Clone, Copy)]
struct Glyph {
    /// handle UI-текстуры (страницы), из которой брать глиф
    tex: usize,
    uv: [f32; 4],
    w: u32,
    h: u32,
    advance: f32,
    xmin: i32,
    ymin: i32,
}

/// Страница атласа одной гарнитуры.
struct Page {
    slot: usize,
    pen_x: u32,
    pen_y: u32,
    row_h: u32,
    pixels: Vec<u8>,
    /// текстура уже создана в GPU (иначе первый flush зальёт страницу целиком)
    created: bool,
    /// прямоугольник (x0, y0, x1, y1), который надо догрузить в текстуру
    dirty: Option<[u32; 4]>,
}

impl Page {
    fn new(slot: usize) -> Page {
        Page {
            slot,
            pen_x: 0,
            pen_y: 0,
            row_h: 0,
            pixels: vec![0; (PAGE_W * PAGE_H * 4) as usize],
            created: false,
            dirty: None,
        }
    }

    /// Положить битмап на страницу (полки слева-направо, сверху-вниз).
    /// `None` — страница заполнена, нужна следующая.
    fn place(&mut self, m: &Metrics, bitmap: &[u8], gw: u32, gh: u32) -> Option<Glyph> {
        if self.pen_x + gw > PAGE_W {
            self.pen_x = 0;
            self.pen_y += self.row_h + 1;
            self.row_h = 0;
        }
        if self.pen_y + gh > PAGE_H {
            return None;
        }
        for y in 0..gh {
            for x in 0..gw {
                let a = bitmap[(y * gw + x) as usize];
                let idx = (((self.pen_y + y) * PAGE_W + (self.pen_x + x)) * 4) as usize;
                self.pixels[idx] = 255;
                self.pixels[idx + 1] = 255;
                self.pixels[idx + 2] = 255;
                self.pixels[idx + 3] = a;
            }
        }
        let x0 = self.pen_x;
        let y0 = self.pen_y;
        let uv = [
            x0 as f32 / PAGE_W as f32,
            y0 as f32 / PAGE_H as f32,
            (x0 + gw) as f32 / PAGE_W as f32,
            (y0 + gh) as f32 / PAGE_H as f32,
        ];
        self.dirty = Some(match self.dirty {
            None => [x0, y0, x0 + gw, y0 + gh],
            Some([dx0, dy0, dx1, dy1]) => [
                dx0.min(x0),
                dy0.min(y0),
                dx1.max(x0 + gw),
                dy1.max(y0 + gh),
            ],
        });
        self.pen_x += gw + 1;
        self.row_h = self.row_h.max(gh);
        Some(Glyph {
            tex: self.slot,
            uv,
            w: gw,
            h: gh,
            advance: m.advance_width,
            xmin: m.xmin,
            ymin: m.ymin,
        })
    }
}

#[derive(Default)]
struct FaceCache {
    pages: Vec<Page>,
    /// ключ — (кегль растра, символ)
    glyphs: HashMap<(u32, char), Glyph>,
    ascents: HashMap<u32, f32>,
}

impl FaceCache {
    fn insert(&mut self, font: &Font, slots: &[usize], px: u32, ch: char) -> Option<Glyph> {
        let (m, bitmap) = font.rasterize(ch, px as f32);
        let gw = m.width as u32;
        let gh = m.height as u32;
        if gw == 0 || gh == 0 {
            // пробел и прочие пустые глифы: только advance
            let g = Glyph {
                tex: 0,
                uv: [0.0; 4],
                w: 0,
                h: 0,
                advance: m.advance_width,
                xmin: m.xmin,
                ymin: m.ymin,
            };
            self.glyphs.insert((px, ch), g);
            return Some(g);
        }
        if gw > PAGE_W || gh > PAGE_H {
            return None;
        }
        let mut reset_done = false;
        loop {
            if let Some(page) = self.pages.last_mut() {
                if let Some(g) = page.place(&m, &bitmap, gw, gh) {
                    self.glyphs.insert((px, ch), g);
                    return Some(g);
                }
            }
            // страница заполнена — открываем следующую зарезервированную
            if self.pages.len() < slots.len() {
                let slot = slots[self.pages.len()];
                self.pages.push(Page::new(slot));
                continue;
            }
            if reset_done || slots.is_empty() {
                return None;
            }
            // Все страницы заняты. Бывает только при долгом ресайзе окна:
            // кегли меняются, старые страницы заполняются. Сбрасываем кеш
            // (страница 0 перезаливается пустой) и растим глиф заново.
            self.pages.clear();
            self.pages.push(Page::new(slots[0]));
            self.glyphs.clear();
            self.ascents.clear();
            reset_done = true;
        }
    }
}

struct FaceData {
    font: Font,
    label: &'static str,
    /// зарезервированные handles страниц атласа (индексы UI-текстур)
    slots: Vec<usize>,
    cache: RefCell<FaceCache>,
}

impl FaceData {
    fn glyph(&self, px: u32, ch: char) -> Option<Glyph> {
        if let Some(g) = self.cache.borrow().glyphs.get(&(px, ch)) {
            return Some(*g);
        }
        // символа нет в гарнитуре — вернём None, сработает фолбэк
        if self.font.lookup_glyph_index(ch) == 0 {
            return None;
        }
        let mut cache = self.cache.borrow_mut();
        cache.insert(&self.font, &self.slots, px, ch)
    }

    fn ascent(&self, px: u32) -> f32 {
        if let Some(a) = self.cache.borrow().ascents.get(&px) {
            return *a;
        }
        let a = self
            .font
            .horizontal_line_metrics(px as f32)
            .map(|m| m.ascent)
            .unwrap_or(px as f32 * 0.8);
        self.cache.borrow_mut().ascents.insert(px, a);
        a
    }
}

pub struct Text {
    faces: [FaceData; 3],
    /// Необязательный CJK-фолбэк: в паках встречаются японские названия
    /// (水蓮 / 削除), а в декоративных гарнитурах и Noto Latin таких глифов нет.
    cjk: Option<FaceData>,
}

/// Пиксельный кегль для логического размера отрисовки.
///
/// UI уже в физических пикселях, поэтому кегль — это просто округлённый
/// размер; целое значение обязательно: только тогда битмап ложится на экран
/// пиксель-в-пиксель.
fn px_for(size: f32) -> u32 {
    size.round().clamp(MIN_PX as f32, MAX_PX as f32) as u32
}

impl Text {
    pub fn new(renderer: &mut Renderer) -> Result<Text> {
        // Noto обязателен (OFL, лежит в репозитории). Декоративные Deemo-шрифты
        // не распространяются с игрой: если их нет — берём Noto, чтобы игра
        // запускалась, просто выглядя строже.
        let body = build_face(renderer, "data/fonts/NotoSans-Regular.ttf", "font-body")
            .context("missing data/fonts/NotoSans-Regular.ttf")?;
        let display = match build_face(renderer, "data/fonts/Fantique-Snails.ttf", "font-display") {
            Ok(f) => f,
            Err(_) => {
                eprintln!(
                    "font: data/fonts/Fantique-Snails.ttf not found — using Noto for display text"
                );
                build_face(renderer, "data/fonts/NotoSans-Regular.ttf", "font-display")?
            }
        };
        let serif = match build_face(renderer, "data/fonts/Dragon-Serial-Regular.ttf", "font-serif") {
            Ok(f) => f,
            Err(_) => {
                eprintln!(
                    "font: data/fonts/Dragon-Serial-Regular.ttf not found — using Noto for serif text"
                );
                build_face(renderer, "data/fonts/NotoSans-Regular.ttf", "font-serif")?
            }
        };
        let cjk = build_cjk(renderer);
        Ok(Text {
            faces: [display, serif, body],
            cjk,
        })
    }

    fn face(&self, face: Face) -> &FaceData {
        &self.faces[face.index()]
    }

    /// Глиф с учётом фолбэка: Display → Serif → Body. Возвращает глиф,
    /// растеризованный в кегле `px`, уже лежащий в одной из страниц.
    fn glyph(&self, face: Face, px: u32, ch: char) -> Option<Glyph> {
        for f in &self.faces[face.index()..] {
            if let Some(g) = f.glyph(px, ch) {
                return Some(g);
            }
        }
        self.cjk.as_ref().and_then(|f| f.glyph(px, ch))
    }

    fn ascent(&self, face: Face, px: u32) -> f32 {
        self.face(face).ascent(px)
    }

    /// Ширина строки выбранной гарнитурой (с фолбэком по глифам).
    pub fn measure_face(&self, face: Face, s: &str, size: f32) -> f32 {
        self.measure_tracked(face, s, size, 0.0)
    }

    /// Ширина строки с дополнительным межбуквенным интервалом (px на глиф).
    pub fn measure_tracked(&self, face: Face, s: &str, size: f32, tracking: f32) -> f32 {
        let px = px_for(size);
        let mut w = 0.0;
        for ch in s.chars() {
            if let Some(g) = self.glyph(face, px, ch) {
                w += g.advance + tracking;
            }
        }
        if !s.is_empty() {
            w -= tracking; // после последнего глифа трекинг не нужен
        }
        w.max(0.0)
    }

    /// Рисует строку выбранной гарнитурой с верхним левым углом в (x, y).
    pub fn draw_face(
        &self,
        face: Face,
        s: &str,
        x: f32,
        y: f32,
        size: f32,
        color: [f32; 4],
        out: &mut Vec<UiQuad>,
    ) {
        self.draw_tracked(face, s, x, y, size, 0.0, color, out);
    }

    /// Как [`Text::draw_face`], но с межбуквенным интервалом (px).
    pub fn draw_tracked(
        &self,
        face: Face,
        s: &str,
        x: f32,
        y: f32,
        size: f32,
        tracking: f32,
        color: [f32; 4],
        out: &mut Vec<UiQuad>,
    ) {
        let px = px_for(size);
        // базу ставим на целый пиксель: тогда gy тоже целый
        let baseline = (y + self.ascent(face, px)).round();
        let mut pen = x;
        for ch in s.chars() {
            let Some(g) = self.glyph(face, px, ch) else {
                continue;
            };
            if g.w > 0 && g.h > 0 {
                let gx = (pen + g.xmin as f32).round();
                let gy = baseline - (g.ymin + g.h as i32) as f32;
                out.push(UiQuad {
                    tex: g.tex,
                    rect: [gx, gy, g.w as f32, g.h as f32],
                    uv: g.uv,
                    color,
                });
            }
            pen += g.advance + tracking;
        }
    }

    /// Строка по центру cx (выбранная гарнитура).
    pub fn draw_centered_face(
        &self,
        face: Face,
        s: &str,
        cx: f32,
        y: f32,
        size: f32,
        color: [f32; 4],
        out: &mut Vec<UiQuad>,
    ) {
        let w = self.measure_face(face, s, size);
        self.draw_face(face, s, cx - w * 0.5, y, size, color, out);
    }

    /// Строка по центру с трекингом (капс-лейблы).
    pub fn draw_centered_tracked(
        &self,
        face: Face,
        s: &str,
        cx: f32,
        y: f32,
        size: f32,
        tracking: f32,
        color: [f32; 4],
        out: &mut Vec<UiQuad>,
    ) {
        let w = self.measure_tracked(face, s, size, tracking);
        self.draw_tracked(face, s, cx - w * 0.5, y, size, tracking, color, out);
    }

    // --- совместимый API игрового HUD (Body) ---

    /// Ширина строки основным шрифтом.
    pub fn measure(&self, s: &str, size: f32) -> f32 {
        self.measure_face(Face::Body, s, size)
    }

    /// Рисует строку основным шрифтом с верхним левым углом в (x, y).
    pub fn draw(&self, s: &str, x: f32, y: f32, size: f32, color: [f32; 4], out: &mut Vec<UiQuad>) {
        self.draw_face(Face::Body, s, x, y, size, color, out);
    }

    /// Выровнять по центру относительно cx основным шрифтом.
    pub fn draw_centered(
        &self,
        s: &str,
        cx: f32,
        y: f32,
        size: f32,
        color: [f32; 4],
        out: &mut Vec<UiQuad>,
    ) {
        self.draw_centered_face(Face::Body, s, cx, y, size, color, out);
    }

    /// Перенести новые глифы из CPU-страниц в GPU. Вызывается один раз за
    /// кадр после того, как все `draw*` разложили квады, и до отрисовки.
    ///
    /// Пока страница не создана, `draw*` уже возвращает корректные UV и handle:
    /// страницы резервируются при [`Text::new`], поэтому порядок UI-текстур
    /// известен заранее.
    pub fn flush(&mut self, renderer: &mut Renderer) {
        // CJK-гарнитура тоже обязана грузить свои страницы: иначе её глифы
        // находятся, но рисуются из пустой 1×1 текстуры (текст «исчезает»).
        let mut all: Vec<&mut FaceData> = self.faces.iter_mut().collect();
        if let Some(cjk) = self.cjk.as_mut() {
            all.push(cjk);
        }
        for f in all {
            let label = f.label;
            let cache = f.cache.get_mut();
            for page in &mut cache.pages {
                if !page.created {
                    renderer.replace_ui_texture(page.slot, label, PAGE_W, PAGE_H, &page.pixels);
                    page.created = true;
                    page.dirty = None;
                } else if let Some([x0, y0, x1, y1]) = page.dirty.take() {
                    let w = x1.saturating_sub(x0);
                    let h = y1.saturating_sub(y0);
                    if w == 0 || h == 0 {
                        continue;
                    }
                    let mut sub = Vec::with_capacity((w * h * 4) as usize);
                    for y in y0..y1 {
                        let start = ((y * PAGE_W + x0) * 4) as usize;
                        sub.extend_from_slice(&page.pixels[start..start + (w * 4) as usize]);
                    }
                    renderer.update_ui_texture(page.slot, x0, y0, w, h, &sub);
                }
            }
        }
    }
}

fn build_face(renderer: &mut Renderer, path: &str, label: &'static str) -> Result<FaceData> {
    let bytes = std::fs::read(path).with_context(|| format!("missing {path}"))?;
    let font = Font::from_bytes(bytes, fontdue::FontSettings::default())
        .map_err(|e| anyhow::anyhow!("font {path}: {e}"))?;
    let slots = (0..PAGES_PER_FACE)
        .map(|_| renderer.reserve_ui_texture(label))
        .collect();
    Ok(FaceData {
        font,
        label,
        slots,
        cache: RefCell::new(FaceCache::default()),
    })
}

/// Кандидаты CJK-шрифта: сначала то, что лежит рядом с игрой, потом системное.
const CJK_CANDIDATES: &[&str] = &[
    "data/fonts/NotoSansCJK-Regular.ttc",
    "data/fonts/NotoSansJP-Regular.otf",
    "/usr/share/fonts/google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/fonts-japanese-gothic.ttf",
    // Windows: системные CJK-шрифты (отсутствующие просто пропускаются)
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/msyhbd.ttc",
    "C:/Windows/Fonts/meiryo.ttc",
    "C:/Windows/Fonts/YuGothM.ttc",
    "C:/Windows/Fonts/msgothic.ttc",
    "C:/Windows/Fonts/msmincho.ttc",
];

/// Подключить CJK-гарнитуру (если найдётся): без неё японские/китайские
/// названия паков рисуются пустотой — «карта есть, а названия нет».
fn build_cjk(renderer: &mut Renderer) -> Option<FaceData> {
    for path in CJK_CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else { continue };
        // .ttc — коллекция: пробуем первые гарнитуры (0 у Noto CJK = JP)
        for index in 0..4u32 {
            let settings = fontdue::FontSettings {
                collection_index: index,
                ..Default::default()
            };
            let Ok(font) = Font::from_bytes(bytes.clone(), settings) else { continue };
            if font.lookup_glyph_index('\u{6c34}') == 0 || font.lookup_glyph_index('\u{524a}') == 0 {
                continue; // 水 / 削 — базовые ханьские иероглифы
            }
            let slots = (0..PAGES_PER_FACE)
                .map(|_| renderer.reserve_ui_texture("font-cjk"))
                .collect();
            println!("cjk fallback: {path} (index {index})");
            return Some(FaceData {
                font,
                label: "font-cjk",
                slots,
                cache: RefCell::new(FaceCache::default()),
            });
        }
    }
    eprintln!("cjk fallback: no CJK font found — Japanese/Chinese titles will not render");
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(name: &str) -> Font {
        // тесты запускаются из native/, а шрифты лежат в ../data/fonts
        let path = format!("{}/../data/fonts/{name}", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("missing {path}: {e}"));
        Font::from_bytes(bytes, fontdue::FontSettings::default()).expect("font")
    }

    #[test]
    fn deemo_fonts_cover_latin_and_fallback_to_body_for_cyrillic() {
        for name in [
            "Fantique-Snails.ttf",
            "Dragon-Serial-Regular.ttf",
            "NotoSans-Regular.ttf",
        ] {
            let f = font(name);
            for ch in "DEEMO Song Select 0123456789".chars() {
                if ch == ' ' {
                    continue;
                }
                assert_ne!(f.lookup_glyph_index(ch), 0, "{name}: missing glyph {ch:?}");
            }
        }
        // декоративные гарнитуры — латиница, кириллица приходит из Noto
        let display = font("Fantique-Snails.ttf");
        let serif = font("Dragon-Serial-Regular.ttf");
        let body = font("NotoSans-Regular.ttf");
        for ch in ('\u{0410}'..='\u{044f}').chain(std::iter::once('\u{0401}')).chain(std::iter::once('\u{0451}')) {
            assert_ne!(body.lookup_glyph_index(ch), 0, "NotoSans: missing {ch:?}");
        }
        assert_eq!(display.lookup_glyph_index('Д'), 0);
        assert_eq!(serif.lookup_glyph_index('Д'), 0);
    }

    /// Кегль растра должен совпадать с размером отрисовки, а не быть
    /// фиксированным: иначе мелкий текст «зернит», а крупный мылится.
    #[test]
    fn bitmap_size_follows_requested_pixel_size() {
        let f = font("NotoSans-Regular.ttf");
        let (small, _) = f.rasterize('H', px_for(13.0) as f32);
        let (large, _) = f.rasterize('H', px_for(72.0) as f32);
        assert_eq!(px_for(13.4), 13);
        assert_eq!(px_for(13.6), 14);
        assert_eq!(px_for(159.0), 159);
        assert!(
            large.height > small.height * 4,
            "72 px must be visibly larger than 13 px ({} vs {})",
            large.height,
            small.height
        );
    }

    /// Глифы разных кеглей живут в одном атласе и не перекрываются.
    #[test]
    fn page_packs_glyphs_side_by_side() {
        let f = font("NotoSans-Regular.ttf");
        let mut page = Page::new(0);
        let (m1, b1) = f.rasterize('M', 40.0);
        let g1 = page
            .place(&m1, &b1, m1.width as u32, m1.height as u32)
            .expect("first glyph");
        let (m2, b2) = f.rasterize('W', 12.0);
        let g2 = page
            .place(&m2, &b2, m2.width as u32, m2.height as u32)
            .expect("second glyph");
        assert_eq!(g1.tex, g2.tex);
        assert!(
            g2.uv[0] >= g1.uv[2],
            "the second glyph must sit right of the first"
        );
        assert!(g2.uv[1] < g1.uv[3]);
        assert!(page.dirty.is_some());
    }

    /// Японские названия паков (水蓮 / 削除) обязаны иметь глифы: без
    /// CJK-фолбэка строка рисуется пустотой, и сонг выглядит безымянным.
    #[test]
    fn cjk_fallback_covers_japanese_titles() {
        for name in ["Fantique-Snails.ttf", "Dragon-Serial-Regular.ttf", "NotoSans-Regular.ttf"] {
            assert_eq!(font(name).lookup_glyph_index('\u{6c34}'), 0, "{name}: unexpected CJK");
        }
        let mut found = None;
        for path in CJK_CANDIDATES {
            let Ok(bytes) = std::fs::read(path) else { continue };
            if let Ok(f) = Font::from_bytes(bytes, fontdue::FontSettings::default()) {
                let all = ['\u{6c34}', '\u{84ee}', '\u{524a}', '\u{9664}']
                    .iter()
                    .all(|c| f.lookup_glyph_index(*c) != 0);
                if all {
                    found = Some(*path);
                    break;
                }
            }
        }
        match found {
            Some(p) => println!("cjk test: using {p}"),
            None => eprintln!("cjk test: no system CJK font — skipped"),
        }
    }
}
