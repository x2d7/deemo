//! Экранные композиции меню: выбор песни («song select»), пауза, результат.
//!
//! Вся геометрия и рисование меню собраны здесь, чтобы их можно было вызвать
//! и из игрового цикла, и из headless-скриншотов (`--shot --screen ...`).
//! Геймплей (ноты, судейская линия, HUD) сюда не входит.

use std::collections::HashMap;

use crate::library::Song;
use crate::online::{Online, OnlineRecord};
use crate::render::UiQuad;
use crate::results::{fmt_date, Results};
use crate::text::{Face, Text};
use crate::theme::{self, Btn, ACCENT, ACCENT_INK, COOL, GHOST, INK, INK_DIM, INK_FAINT, INK_HI, ON_INK, PANEL, PANEL_HI, RULE, RULE_STRONG};

pub const TOP_H: f32 = 76.0;
pub const BOT_H: f32 = 132.0;
pub const LEFT_W: f32 = 272.0;
pub const MARGIN: f32 = 28.0;
pub const ROW_H: f32 = 84.0;

// ===== общий контекст =====

/// Неизменяемые ресурсы для рисования UI.
pub struct Ctx<'a> {
    pub text: &'a Text,
    pub white: usize,
    pub fade_x: usize,
    pub fade_y: usize,
    pub w: f32,
    pub h: f32,
    /// Глобальный масштаб меню; см. [`theme::ui_scale`].
    pub s: f32,
}

// ===== выбор песни =====

/// Геометрия левой колонки, списка и арта.
#[derive(Clone)]
pub struct LibGeom {
    /// глобальный масштаб (1280×720 → 1.0)
    pub s: f32,
    pub top_h: f32,
    pub left_w: f32,
    pub bot_h: f32,
    pub margin: f32,
    pub row_h: f32,
    pub rx: f32,
    pub rw: f32,
    pub art: Option<[f32; 4]>,
}

pub fn lib_geom(w: f32, h: f32, art_aspect: Option<f32>) -> LibGeom {
    let s = theme::ui_scale(w, h);
    let top_h = TOP_H * s;
    let bot_h = BOT_H * s;
    let left_w = LEFT_W * s;
    let m = MARGIN * s;
    let avail_h = (h - top_h - bot_h).max(160.0 * s);
    let art = art_aspect.map(|aspect| {
        let aspect = aspect.clamp(0.5, 2.0);
        let mut ah = avail_h;
        let mut aw = ah * aspect;
        let max_aw = (w - left_w - m - 430.0 * s).max(220.0 * s);
        if aw > max_aw {
            aw = max_aw;
            ah = aw / aspect;
        }
        [w - aw, top_h + (avail_h - ah) * 0.5, aw, ah]
    });
    let rx = left_w + m;
    let right_edge = art.map(|a| a[0] - m).unwrap_or(w - m);
    let rw_full = (right_edge - rx).max(240.0 * s);
    // без выбранной песни список уже, справа — титульный блок (main-состояние)
    let rw = if art.is_some() { rw_full } else { rw_full.min(620.0 * s) };
    LibGeom {
        s,
        top_h,
        left_w,
        bot_h,
        margin: m,
        row_h: ROW_H * s,
        rx,
        rw,
        art,
    }
}

/// Кнопки/поля верхней панели и нижней полосы (общие для layout и кликов).
pub struct LibChrome {
    pub settings_btn: [f32; 4],
    pub sort_btn: [f32; 4],
    pub group_btn: [f32; 4],
    pub search_rect: [f32; 4],
    pub play_btn: [f32; 4],
    pub login_btn: [f32; 4],
    pub lb_toggle_rect: [f32; 4],
}

pub fn lib_chrome(w: f32, _h: f32, geom: &LibGeom, selected: bool, logged_in: bool) -> LibChrome {
    let s = geom.s;
    let settings_btn = [w - geom.margin - 110.0 * s, 22.0 * s, 110.0 * s, 32.0 * s];
    let group_btn = [settings_btn[0] - 12.0 * s - 148.0 * s, 22.0 * s, 148.0 * s, 32.0 * s];
    let sort_btn = [group_btn[0] - 12.0 * s - 148.0 * s, 22.0 * s, 148.0 * s, 32.0 * s];
    let sx = geom.left_w + geom.margin;
    let search_rect = [sx, 12.0 * s, (sort_btn[0] - 16.0 * s - sx).max(120.0 * s), 48.0 * s];
    let play_btn = [w - geom.margin - 176.0 * s, _h - geom.bot_h + 34.0 * s, 176.0 * s, 64.0 * s];
    let login_btn = [geom.left_w - geom.margin - 104.0 * s, geom.top_h + 12.0 * s, 104.0 * s, 22.0 * s];
    let lb_toggle_rect = if logged_in && selected {
        [
            geom.margin,
            geom.top_h + 146.0 * s,
            geom.left_w - geom.margin * 2.0,
            24.0 * s,
        ]
    } else {
        [0.0; 4]
    };
    LibChrome {
        settings_btn,
        sort_btn,
        group_btn,
        search_rect,
        play_btn,
        login_btn,
        lb_toggle_rect,
    }
}

/// Позиции карточек/заголовков групп: `scroll` уже вычтен.
pub fn library_rows(
    groups: &[(String, Vec<usize>)],
    geom: &LibGeom,
    scroll: f32,
) -> (Vec<(usize, [f32; 4])>, Vec<(String, f32)>, f32) {
    let s = geom.s;
    let mut y = geom.top_h + 18.0 * s;
    let mut cards = Vec::new();
    let mut heads = Vec::new();
    for (name, idxs) in groups {
        heads.push((name.clone(), y - scroll));
        y += 30.0 * s;
        for &si in idxs {
            cards.push((si, [geom.rx, y - scroll, geom.rw, geom.row_h]));
            y += geom.row_h;
        }
        y += 20.0 * s;
    }
    (cards, heads, y - geom.top_h)
}

pub struct DropdownItem<'a> {
    pub rect: [f32; 4],
    pub label: &'a str,
    pub active: bool,
}

pub struct Dropdown<'a> {
    pub items: &'a [DropdownItem<'a>],
}

/// Прямоугольники чипов сложности в нижней полосе.
pub fn diff_chip_rects(
    _w: f32,
    h: f32,
    geom: &LibGeom,
    chrome: &LibChrome,
    n: usize,
) -> Vec<(usize, [f32; 4])> {
    let s = geom.s;
    let chip_w = 116.0 * s;
    let gap = 10.0 * s;
    let chip_h = 78.0 * s;
    if n == 0 {
        return Vec::new();
    }
    let total = n as f32 * chip_w + (n as f32 - 1.0) * gap;
    let area_x = geom.margin + 500.0 * s;
    let area_w = (chrome.play_btn[0] - 20.0 * s) - area_x;
    let x0 = area_x + (area_w - total).max(0.0) * 0.5;
    (0..n)
        .map(|d| {
            (
                d,
                [
                    x0 + d as f32 * (chip_w + gap),
                    h - geom.bot_h + 27.0 * s,
                    chip_w,
                    chip_h,
                ],
            )
        })
        .collect()
}

/// Всё, что нужно нарисовать выбор песни.
pub struct LibraryView<'a> {
    pub geom: &'a LibGeom,
    pub chrome: &'a LibChrome,
    pub songs: &'a [Song],
    pub results: &'a Results,
    pub online: &'a Online,
    pub cards: &'a [(usize, [f32; 4])],
    pub headers: &'a [(String, f32)],
    pub selected: Option<usize>,
    pub diff: usize,
    pub search: &'a str,
    pub sort_label: &'a str,
    pub group_label: &'a str,
    pub covers: &'a HashMap<usize, usize>,
    pub art: Option<usize>,
    /// песня под курсором (подсветка строки)
    pub hover: Option<usize>,
    pub lb_online: bool,
    pub lb_pending: bool,
    pub lb_records: &'a [OnlineRecord],
    pub hist_rects: &'a [(usize, [f32; 4])],
    pub diff_rects: &'a [(usize, [f32; 4])],
    pub dropdown: Option<Dropdown<'a>>,
    pub total_h: f32,
    pub scroll: f32,
    pub t: f32,
    /// настоящие ноты выбранной песни (живое падающее поле) для фона меню
    pub bg_notes: &'a [crate::bg::BgNote],
    /// часы поля; None — превью не играет, фон случайный
    pub bg_t: Option<f32>,
}

pub fn draw_library(ctx: &Ctx, v: &LibraryView, ui: &mut Vec<UiQuad>) {
    let text = ctx.text;
    let white = ctx.white;
    let (w, h) = (ctx.w, ctx.h);
    let s = ctx.s;
    let g = v.geom;
    let bottom_y = h - g.bot_h;

    // фон: ночная комната + падающие клавиши (настоящие ноты, если играет превью)
    theme::fill(ui, white, [0.0, 0.0, w, h], theme::BG);
    let bg = [
        g.left_w + 12.0 * s,
        g.top_h,
        (w - 8.0 * s) - (g.left_w + 12.0 * s),
        bottom_y - g.top_h,
    ];
    let real_notes = v.bg_t.filter(|_| !v.bg_notes.is_empty());
    if let Some(t) = real_notes {
        // Мини-поле не заходит на арт и не трогает верхнюю панель: в
        // библиотеке оно живёт только в колонке списка, в main — в фоне
        // списка/титульного блока, но всегда ниже top_h.
        let (field, dim) = if g.art.is_some() {
            ([g.rx, g.top_h, g.rw, bottom_y - g.top_h], None)
        } else if v.selected.is_none() {
            // main: поле живёт по всему фону, но под титульным блоком
            // приглушается, чтобы вордмарк оставался главным; нити слайдов
            // ограничены колонкой списка (link_clip ниже)
            let r = title_block_rect(g, w, bottom_y, s);
            let pad = 20.0 * s;
            (
                [
                    g.rx,
                    g.top_h,
                    (w - g.margin - g.rx).max(200.0 * s),
                    bottom_y - g.top_h,
                ],
                Some((
                    [r[0] - pad, r[1] - pad, r[2] + pad * 2.0, r[3] + pad * 2.0],
                    0.35,
                )),
            )
        } else {
            (bg, None)
        };
        theme::chart_fall(
            ui,
            white,
            field,
            v.bg_notes,
            t,
            0.066,
            s,
            dim,
            Some([g.rx, g.top_h, g.rw, bottom_y - g.top_h]),
        );
    } else {
        theme::falling_keys(
            ui,
            white,
            bg[0],
            bg[0] + bg[2],
            bg[1],
            bg[1] + bg[3],
            v.t,
            0x9e37_79b9,
            0.0016,
            s,
        );
    }
    // тонкая вертикальная линия слева (как корешок нотной книги)
    theme::hair_v(ui, white, g.left_w, g.top_h, bottom_y - g.top_h, RULE, s);

    // === арт выбранной песни справа ===
    if let (Some(a), Some(handle)) = (g.art, v.art) {
        ui.push(UiQuad {
            tex: handle,
            rect: a,
            uv: [0.0, 0.0, 1.0, 1.0],
            color: [0.86, 0.86, 0.87, 1.0],
        });
        // плавный фейд левого края в фон (градиентная маска, без полос)
        theme::fade_x(
            ui,
            ctx.fade_x,
            [a[0], a[1], a[2] * 0.36, a[3]],
            [theme::BG[0], theme::BG[1], theme::BG[2], 1.0],
        );
        // мягкое затемнение низа арта
        theme::fade_y(
            ui,
            ctx.fade_y,
            [a[0], a[1] + a[3] * 0.62, a[2], a[3] * 0.38],
            [0.0, 0.0, 0.0, 0.55],
        );
        // рамка-видоискатель
        theme::frame(ui, white, a, RULE, s);
        theme::corner_ticks(ui, white, a, [0.84, 0.80, 0.72, 0.55], 18.0 * s, s);
        // уровень выбранной сложности на арте
        if let Some(si) = v.selected {
            let song = &v.songs[si];
            let lv = song.levels.get(v.diff).cloned().unwrap_or_default();
            let chip = [a[0] + 18.0 * s, a[1] + 18.0 * s, 92.0 * s, 46.0 * s];
            theme::fill(ui, white, chip, [0.004, 0.004, 0.005, 0.72]);
            theme::frame(ui, white, chip, [0.84, 0.80, 0.72, 0.35], s);
            theme::label(text, ui, "LV", chip[0] + 10.0 * s, chip[1] + 8.0 * s, 8.0 * s, 2.2 * s, INK_DIM);
            text.draw_face(Face::Serif, &lv, chip[0] + 10.0 * s, chip[1] + 19.0 * s, 20.0 * s, INK_HI, ui);
        }
    } else if real_notes.is_none() {
        // без арта — широкое поле для заголовка; когда падает настоящий чарт,
        // второй случайный мотив не подмешиваем
        theme::falling_keys(
            ui,
            white,
            g.rx,
            w - g.margin,
            g.top_h,
            bottom_y,
            v.t,
            0x51ed_270b,
            0.0016,
            s,
        );
    }

    // мягкая подложка списка: полосы не должны спорить с названиями песен.
    // Очень низкая альфа — это фон, а не карточка.
    theme::fill(
        ui,
        white,
        [g.rx, g.top_h, g.rw, bottom_y - g.top_h],
        [0.0, 0.0, 0.0, 0.32],
    );

    // === левая колонка: рекорды ===
    draw_records(ctx, v, ui, bottom_y);

    // === титульный блок, когда песня не выбрана (main) ===
    if v.selected.is_none() {
        draw_title_block(ctx, v, ui, bottom_y);
    }

    // === список песен (перекрывается панелями сверху и снизу) ===
    draw_song_list(ctx, v, ui, bottom_y);

    // === верхняя панель ===
    theme::fill(ui, white, [0.0, 0.0, w, g.top_h], [theme::BG[0], theme::BG[1], theme::BG[2], 0.94]);
    theme::hair_h(ui, white, 0.0, g.top_h - theme::hairline(s), w, RULE, s);
    draw_topbar(ctx, v, ui);

    // === нижняя полоса ===
    draw_bottom(ctx, v, ui, bottom_y, w, h);

    // === dropdown ===
    if let Some(dd) = &v.dropdown {
        for item in dd.items {
            theme::fill(
                ui,
                white,
                [item.rect[0] + 4.0 * s, item.rect[1] + 5.0 * s, item.rect[2], item.rect[3]],
                [0.0, 0.0, 0.0, 0.55],
            );
        }
        for item in dd.items {
            let r = item.rect;
            theme::fill(ui, white, r, if item.active { [0.075, 0.070, 0.060, 1.0] } else { [0.018, 0.018, 0.022, 1.0] });
            theme::frame(ui, white, r, RULE, s);
            if item.active {
                theme::fill(ui, white, [r[0], r[1], 2.0 * s, r[3]], ACCENT);
            }
            text.draw_tracked(Face::Body, item.label, r[0] + 12.0 * s, r[1] + 8.0 * s, 13.0 * s, 0.4 * s, if item.active { INK_HI } else { INK_DIM }, ui);
        }
    }
}

/// Прямоугольник титульного блока main-состояния (вордмарк, девиз, подсказка).
/// Один и тот же для отрисовки и для приглушения падающего поля под ним.
fn title_block_rect(g: &LibGeom, w: f32, bottom_y: f32, s: f32) -> [f32; 4] {
    let area_x = g.rx + g.rw + 48.0 * s;
    let area_w = (w - g.margin - area_x).max(200.0 * s);
    let panel_w = area_w.min(440.0 * s);
    let panel_x = area_x + (area_w - panel_w) * 0.5;
    let cy = g.top_h + (bottom_y - g.top_h) * 0.5;
    [panel_x, cy - 170.0 * s, panel_w, 260.0 * s]
}

/// Титульный блок главного состояния: вордмарк, девиз и подсказка.
fn draw_title_block(ctx: &Ctx, v: &LibraryView, ui: &mut Vec<UiQuad>, bottom_y: f32) {
    let text = ctx.text;
    let white = ctx.white;
    let s = ctx.s;
    let g = v.geom;
    let r = title_block_rect(g, ctx.w, bottom_y, s);
    let cx = r[0] + r[2] * 0.5;
    let cy = r[1] + r[3] * 0.5;
    text.draw_centered_face(Face::Display, "DEEMO", cx, cy - 150.0 * s, 72.0 * s, INK_HI, ui);
    text.draw_centered_tracked(Face::Serif, "PIANO · MEMORY · A GIRL", cx, cy - 62.0 * s, 9.0 * s, 3.4 * s, INK_DIM, ui);
    theme::hair_h(ui, white, cx - 70.0 * s, cy - 32.0 * s, 140.0 * s, RULE_STRONG, s);
    text.draw_centered_tracked(Face::Serif, "PRESS A SONG", cx, cy - 6.0 * s, 10.0 * s, 3.4 * s, ACCENT_INK, ui);
    text.draw_centered("Pick a song from the list", cx, cy + 22.0 * s, 12.0 * s, INK_DIM, ui);
    text.draw_centered("The preview starts on its own", cx, cy + 42.0 * s, 11.0 * s, INK_FAINT, ui);
    theme::corner_ticks(ui, white, r, [0.84, 0.80, 0.72, 0.18], 14.0 * s, s);
}

fn draw_topbar(ctx: &Ctx, v: &LibraryView, ui: &mut Vec<UiQuad>) {
    let text = ctx.text;
    let white = ctx.white;
    let w = ctx.w;
    let s = ctx.s;
    let c = v.chrome;

    // вордмарк
    text.draw_face(Face::Display, "DEEMO", MARGIN * s, 12.0 * s, 38.0 * s, INK_HI, ui);
    theme::hair_v(ui, white, 162.0 * s, 20.0 * s, 36.0 * s, RULE, s);
    theme::label(text, ui, "SONG SELECT", 178.0 * s, 32.0 * s, 10.0 * s, 3.4 * s, INK_DIM);

    // поиск: подпись + строка + линейка
    let sr = c.search_rect;
    theme::label(text, ui, "SEARCH", sr[0] + 1.0 * s, sr[1] + 2.0 * s, 8.5 * s, 2.4 * s, INK_FAINT);
    let typed = if v.search.is_empty() {
        text.draw_face(Face::Body, "title · artist · album", sr[0] + 1.0 * s, sr[1] + 17.0 * s, 14.5 * s, INK_FAINT, ui);
        String::new()
    } else {
        v.search.to_string()
    };
    if !typed.is_empty() {
        let shown = fit_simple(text, &typed, sr[2] - 18.0 * s, 14.5 * s);
        text.draw(&shown, sr[0] + 1.0 * s, sr[1] + 17.0 * s, 14.5 * s, INK_HI, ui);
        let tw = text.measure(&shown, 14.5 * s);
        let blink = ((v.t * 1.6) as i32) % 2 == 0;
        if blink {
            theme::fill(ui, white, [sr[0] + 2.0 * s + tw, sr[1] + 19.0 * s, 1.5 * s, 15.0 * s], ACCENT_INK);
        }
    }
    theme::hair_h(ui, white, sr[0], sr[1] + sr[3] - 3.0 * s, sr[2], RULE_STRONG, s);

    // сортировка/группировка
    let sort_cap = format!("SORT · {}", v.sort_label.to_uppercase());
    theme::button(text, ui, white, c.sort_btn, &sort_cap, Btn::Ghost, None, s);
    let group_cap = format!("GROUP · {}", v.group_label.to_uppercase());
    theme::button(text, ui, white, c.group_btn, &group_cap, Btn::Ghost, None, s);
    theme::button(text, ui, white, c.settings_btn, "SETTINGS", Btn::Ghost, None, s);
    let _ = w;
}

fn draw_records(ctx: &Ctx, v: &LibraryView, ui: &mut Vec<UiQuad>, bottom_y: f32) {
    let text = ctx.text;
    let white = ctx.white;
    let s = ctx.s;
    let lw = v.geom.left_w;
    let x = v.geom.margin;
    let right = lw - v.geom.margin;
    let c = v.chrome;

    // account
    let acct = v
        .online
        .state
        .nickname
        .clone()
        .unwrap_or_else(|| "SIGN IN".into());
    let acct = fit_face(text, Face::Serif, &acct, c.login_btn[2], 9.0 * s, 1.8 * s);
    let aw = text.measure_tracked(Face::Serif, &acct, 9.0 * s, 1.8 * s);
    let acc_col = if v.online.logged_in() { ACCENT_INK } else { INK_DIM };
    text.draw_tracked(Face::Serif, &acct, c.login_btn[0] + c.login_btn[2] - aw, c.login_btn[1] + 5.0 * s, 9.0 * s, 1.8 * s, acc_col, ui);
    theme::hair_h(ui, white, c.login_btn[0] + c.login_btn[2] - aw, c.login_btn[1] + 19.0 * s, aw, RULE, s);

    let rec_right = if v.chrome.login_btn[2] > 0.0 {
        (v.chrome.login_btn[0] - 12.0 * s).max(x + 90.0 * s)
    } else {
        right
    };
    theme::section(text, ui, white, "RECORDS", x, v.geom.top_h + 18.0 * s, rec_right, INK_DIM, s);

    let mut y = v.geom.top_h + 48.0 * s;
    let Some(si) = v.selected else {
        text.draw("Pick a song", x, y, 14.0 * s, INK_DIM, ui);
        y += 22.0 * s;
        text.draw("from the list to see", x, y, 14.0 * s, INK_FAINT, ui);
        y += 18.0 * s;
        text.draw("its records.", x, y, 14.0 * s, INK_FAINT, ui);
        return;
    };
    let song = &v.songs[si];
    let title = fit_face(text, Face::Serif, &song.title, right - x, 17.0 * s, 0.2 * s);
    text.draw_face(Face::Serif, &title, x, y, 17.0 * s, INK_HI, ui);
    y += 24.0 * s;
    let comp = fit_simple(text, &song.composer, right - x, 11.0 * s);
    text.draw(&comp, x, y, 11.0 * s, INK_DIM, ui);

    // табы сложностей
    y = v.geom.top_h + 108.0 * s;
    theme::label(text, ui, "DIFFICULTY", x, y - 16.0 * s, 8.5 * s, 2.2 * s, INK_FAINT);
    for &(d, r) in v.hist_rects {
        let Some(chart) = song.charts.get(d) else {
            continue;
        };
        let is_sel = d == v.diff;
        let cap = diff_short(&chart.diff);
        let tw = text.measure_tracked(Face::Serif, cap, 10.0 * s, 1.6 * s);
        text.draw_tracked(
            Face::Serif,
            cap,
            r[0] + (r[2] - tw) * 0.5,
            r[1] + 6.0 * s,
            10.0 * s,
            1.6 * s,
            if is_sel { INK_HI } else { INK_FAINT },
            ui,
        );
        if is_sel {
            theme::fill(ui, white, [r[0] + 4.0 * s, r[1] + r[3] - 2.0 * s, r[2] - 8.0 * s, 2.0 * s], ACCENT);
        }
    }

    // тумблер локально/онлайн
    if v.online.logged_in() {
        let tr = c.lb_toggle_rect;
        let cap = if v.lb_online { "ONLINE · LEADERS" } else { "LOCAL · HISTORY" };
        let tw = text.measure_tracked(Face::Serif, cap, 8.5 * s, 1.8 * s);
        let tx = tr[0] + (tr[2] - tw) * 0.5;
        text.draw_tracked(Face::Serif, cap, tx, tr[1] + 5.0 * s, 8.5 * s, 1.8 * s, if v.lb_online { ACCENT_INK } else { INK_DIM }, ui);
        theme::hair_h(ui, white, tx, tr[1] + 18.0 * s, tw, if v.lb_online { ACCENT } else { RULE }, s);
    }

    y = v.geom.top_h + 182.0 * s;
    let is_online = v.lb_online && v.online.logged_in();
    let head = if is_online {
        "LEADERS".to_string()
    } else {
        let diff_name = song.charts.get(v.diff).map(|c| c.diff.clone()).unwrap_or_default();
        format!("HISTORY · {}", diff_short(&diff_name))
    };
    theme::section(text, ui, white, &head, x, y, right, INK_FAINT, s);
    y += 28.0 * s;

    if is_online {
        if v.lb_pending {
            text.draw("loading…", x, y, 12.0 * s, INK_DIM, ui);
            return;
        }
        if v.lb_records.is_empty() {
            text.draw("No records yet", x, y, 12.0 * s, INK_DIM, ui);
            text.draw("on the server.", x, y + 18.0 * s, 12.0 * s, INK_FAINT, ui);
            return;
        }
        for (i, r) in v.lb_records.iter().enumerate() {
            if y > bottom_y - 48.0 * s {
                break;
            }
            draw_record_row(ctx, ui, y, x, right, &format!("{}", i + 1), &r.rank, r.score, r.accuracy, &r.nickname);
            y += 44.0 * s;
        }
        return;
    }

    let sid = &song.id;
    let diff_name = song
        .charts
        .get(v.diff)
        .map(|c| c.diff.clone())
        .unwrap_or_default();
    if let Some(b) = v.results.best(sid, &diff_name) {
        let cap = format!("BEST {} · {:.2}%", b.rank, b.accuracy);
        text.draw_tracked(Face::Serif, &cap, x, y, 9.0 * s, 1.6 * s, ACCENT_INK, ui);
        y += 22.0 * s;
    }
    let hist = v.results.history_for_diff(sid, &diff_name, 40);
    if hist.is_empty() {
        text.draw("Play this difficulty", x, y, 12.0 * s, INK_DIM, ui);
        text.draw("to start a history.", x, y + 18.0 * s, 12.0 * s, INK_FAINT, ui);
        return;
    }
    for r in hist {
        if y > bottom_y - 48.0 * s {
            break;
        }
        let sub = format!("{} MISS · {}", r.miss, fmt_date(r.timestamp));
        draw_record_row(ctx, ui, y, x, right, &fmt_score_local(r.score, true), &r.rank, r.score, r.accuracy, &sub);
        y += 44.0 * s;
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_record_row(
    ctx: &Ctx,
    ui: &mut Vec<UiQuad>,
    y: f32,
    x: f32,
    right: f32,
    score: &str,
    rank: &str,
    _score_raw: u64,
    acc: f32,
    sub: &str,
) {
    let text = ctx.text;
    let white = ctx.white;
    let s = ctx.s;
    let rc = theme::rank_color(rank);
    text.draw_face(Face::Serif, rank, x, y - 1.0 * s, 16.0 * s, rc, ui);
    text.draw_face(Face::Body, score, x + 34.0 * s, y, 13.0 * s, INK, ui);
    let accs = format!("{acc:.1}%");
    let aw = text.measure(&accs, 10.5 * s);
    text.draw(&accs, right - aw, y + 1.0 * s, 10.5 * s, INK_DIM, ui);
    let sub = fit_simple(text, sub, right - x - 34.0 * s, 9.5 * s);
    text.draw(&sub, x + 34.0 * s, y + 18.0 * s, 9.5 * s, INK_FAINT, ui);
    theme::hair_h(ui, white, x, y + 38.0 * s, right - x, [0.84, 0.80, 0.72, 0.07], s);
}

fn draw_song_list(ctx: &Ctx, v: &LibraryView, ui: &mut Vec<UiQuad>, bottom_y: f32) {
    let text = ctx.text;
    let white = ctx.white;
    let s = ctx.s;
    let g = v.geom;

    if v.cards.is_empty() {
        text.draw_tracked(Face::Serif, "NOTHING FOUND", g.rx, g.top_h + 52.0 * s, 10.5 * s, 3.0 * s, INK_DIM, ui);
        text.draw("Adjust your search", g.rx, g.top_h + 76.0 * s, 12.0 * s, INK_FAINT, ui);
        return;
    }

    // заголовки групп
    for (name, y) in v.headers {
        if *y < g.top_h - 4.0 * s || *y > bottom_y - 20.0 * s {
            continue;
        }
        theme::fill(ui, white, [g.rx, *y + 4.0 * s, 6.0 * s, 6.0 * s], ACCENT);
        let up = name.to_uppercase();
        text.draw_tracked(Face::Serif, &up, g.rx + 16.0 * s, *y + 1.0 * s, 10.0 * s, 2.4 * s, INK_DIM, ui);
    }

    // строки
    for &(si, r) in v.cards {
        if r[1] < g.top_h || r[1] + r[3] > bottom_y {
            continue;
        }
        let song = &v.songs[si];
        let is_sel = v.selected == Some(si);
        let is_hover = v.hover == Some(si);
        if is_sel {
            theme::fill(ui, white, r, PANEL_HI);
            theme::fill(ui, white, [r[0], r[1], 2.0 * s, r[3]], ACCENT);
        } else if is_hover {
            theme::fill(ui, white, r, PANEL);
        }
        theme::hair_h(ui, white, r[0], r[1] + r[3] - theme::hairline(s), r[2], [0.84, 0.80, 0.72, 0.10], s);

        // обложка
        let tsize = 56.0 * s;
        let tr = [r[0] + 6.0 * s, r[1] + (r[3] - tsize) * 0.5, tsize, tsize];
        if let Some(&handle) = v.covers.get(&si) {
            ui.push(UiQuad {
                tex: handle,
                rect: tr,
                uv: [0.0, 0.0, 1.0, 1.0],
                color: [1.0, 1.0, 1.0, 1.0],
            });
            theme::frame(ui, white, tr, if is_sel { [0.84, 0.80, 0.72, 0.55] } else { [0.84, 0.80, 0.72, 0.16] }, s);
        } else {
            theme::fill(ui, white, tr, GHOST);
            theme::frame(ui, white, tr, RULE, s);
        }

        let tx = tr[0] + tsize + 16.0 * s;
        let right_pad = 116.0 * s;
        let title = fit_face(text, Face::Body, &song.title, r[2] - (tx - r[0]) - right_pad, 17.0 * s, 0.0);
        let title_col = if is_sel || is_hover { INK_HI } else { INK };
        text.draw(&title, tx, r[1] + 17.0 * s, 17.0 * s, title_col, ui);
        let comp = fit_simple(text, &song.composer, r[2] - (tx - r[0]) - right_pad, 11.0 * s);
        text.draw(&comp, tx, r[1] + 41.0 * s, 11.0 * s, if is_sel { INK_DIM } else { INK_FAINT }, ui);

        let mins = (song.duration / 60.0).floor() as u32;
        let secs = (song.duration % 60.0) as u32;
        let info = format!("{} · {}:{:02} · {}", song.album, mins, secs, song.levels.join("/"));
        let info = fit_simple(text, &info, r[2] - (tx - r[0]) - right_pad, 9.5 * s);
        text.draw(&info, tx, r[1] + 59.0 * s, 9.5 * s, [0.84, 0.80, 0.72, 0.26], ui);

        // справа: ранг / точность / счёт
        if let Some(best) = v.results.best_song(&song.id) {
            let rc = theme::rank_color(&best.rank);
            let rw = text.measure_face(Face::Display, &best.rank, 26.0 * s);
            text.draw_face(Face::Display, &best.rank, r[0] + r[2] - 18.0 * s - rw, r[1] + 18.0 * s, 26.0 * s, rc, ui);
            let acc = format!("{:.2}%", best.accuracy);
            let aw = text.measure(&acc, 11.0 * s);
            text.draw(&acc, r[0] + r[2] - 18.0 * s - aw, r[1] + 48.0 * s, 11.0 * s, INK_DIM, ui);
            let sc = fmt_score_local(best.score, false);
            let sw = text.measure(&sc, 9.5 * s);
            text.draw(&sc, r[0] + r[2] - 18.0 * s - sw, r[1] + 63.0 * s, 9.5 * s, INK_FAINT, ui);
        }
    }

    // индикатор прокрутки
    if let Some((track, thumb)) = scrollbar(g, bottom_y - g.top_h, v.total_h, v.scroll) {
        theme::fill(ui, white, track, [0.84, 0.80, 0.72, 0.05]);
        theme::fill(ui, white, thumb, [0.84, 0.80, 0.72, 0.28]);
    }
}

/// Дорожка и ползунок скроллбара списка песен.
///
/// Одна формула и для отрисовки, и для попадания мышью: иначе тянуть
/// нечего — ползунок рисовался, но не ловил мышь.
pub fn scrollbar(
    g: &LibGeom,
    view_h: f32,
    total_h: f32,
    scroll: f32,
) -> Option<([f32; 4], [f32; 4])> {
    if total_h <= view_h || view_h <= 0.0 {
        return None;
    }
    let s = g.s;
    let track = [g.rx + g.rw + 10.0 * s, g.top_h + 10.0 * s, 2.0 * s, view_h - 20.0 * s];
    let frac = (view_h / total_h).clamp(0.05, 1.0);
    let thumb_h = (track[3] * frac).max(28.0 * s).min(track[3]);
    let max_scroll = (total_h - view_h).max(1.0);
    let ty = track[1] + (track[3] - thumb_h) * (scroll / max_scroll).clamp(0.0, 1.0);
    Some((track, [track[0], ty, 2.0 * s, thumb_h]))
}

fn draw_bottom(ctx: &Ctx, v: &LibraryView, ui: &mut Vec<UiQuad>, by: f32, w: f32, h: f32) {
    let text = ctx.text;
    let white = ctx.white;
    let s = ctx.s;
    theme::fill(ui, white, [0.0, by, w, h - by], [0.006, 0.006, 0.0075, 0.94]);
    theme::hair_h(ui, white, 0.0, by, w, RULE_STRONG, s);

    let Some(si) = v.selected else {
        // главный экран без выбора: подсказки + статистика библиотеки
        text.draw_tracked(Face::Serif, "SONG SELECT", MARGIN * s, by + 22.0 * s, 9.0 * s, 3.0 * s, INK_FAINT, ui);
        text.draw("Click — select a song and preview", MARGIN * s, by + 44.0 * s, 14.0 * s, INK_HI, ui);
        text.draw("Type — search title, artist, album", MARGIN * s, by + 68.0 * s, 10.5 * s, INK_DIM, ui);
        text.draw("F7 — install a pack", MARGIN * s, by + 90.0 * s, 10.5 * s, INK_FAINT, ui);
        let stats = [
            ("SONGS", v.songs.len().to_string()),
            ("CHARTS", format!("{}", v.songs.iter().map(|s| s.charts.len()).sum::<usize>())),
            ("RECORDS", format!("{}", v.results.map.len())),
        ];
        let mut x = w - MARGIN * s;
        let col_w = 150.0 * s;
        for (cap, val) in stats.iter().rev() {
            x -= col_w;
            let vw = text.measure_face(Face::Serif, val, 26.0 * s);
            text.draw_face(Face::Serif, val, x + col_w - vw, by + 30.0 * s, 26.0 * s, INK, ui);
            let cw = text.measure_tracked(Face::Serif, cap, 8.5 * s, 2.4 * s);
            text.draw_tracked(Face::Serif, cap, x + col_w - cw, by + 66.0 * s, 8.5 * s, 2.4 * s, INK_FAINT, ui);
        }
        return;
    };

    let song = &v.songs[si];
    theme::fill(ui, white, [MARGIN * s, by + 21.0 * s, 5.0 * s, 5.0 * s], ACCENT);
    text.draw_tracked(Face::Serif, "SELECTED", MARGIN * s + 12.0 * s, by + 17.0 * s, 9.0 * s, 2.6 * s, INK_DIM, ui);
    let title = fit_face(text, Face::Display, &song.title, 460.0 * s, 30.0 * s, 0.0);
    text.draw_face(Face::Display, &title, MARGIN * s - 2.0 * s, by + 30.0 * s, 30.0 * s, INK_HI, ui);
    let comp = fit_simple(text, &song.composer, 420.0 * s, 11.0 * s);
    text.draw_tracked(Face::Serif, &comp, MARGIN * s, by + 72.0 * s, 11.0 * s, 1.6 * s, INK_DIM, ui);
    let mins = (song.duration / 60.0).floor() as u32;
    let secs = (song.duration % 60.0) as u32;
    let meta = format!("{} · {}:{:02}", song.album, mins, secs);
    let meta = fit_simple(text, &meta, 420.0 * s, 9.5 * s);
    text.draw(&meta, MARGIN * s, by + 92.0 * s, 9.5 * s, INK_FAINT, ui);

    // чипы сложностей
    let cs = v.chrome;
    let n = v.diff_rects.len();
    if n > 0 {
        for &(d, r) in v.diff_rects {
            let Some(chart) = song.charts.get(d) else {
                continue;
            };
            let is_sel = d == v.diff;
            if is_sel {
                theme::fill(ui, white, r, INK);
            } else {
                theme::fill(ui, white, r, PANEL);
                theme::frame(ui, white, r, [0.84, 0.80, 0.72, 0.20], s);
            }
            let (fg, fg_dim) = if is_sel {
                (ON_INK, [ON_INK[0], ON_INK[1], ON_INK[2], 0.55])
            } else {
                (INK_HI, INK_FAINT)
            };
            let cap = diff_short(&chart.diff);
            text.draw_centered_tracked(Face::Serif, cap, r[0] + r[2] * 0.5, r[1] + 10.0 * s, 8.5 * s, 2.2 * s, if is_sel { fg_dim } else { INK_DIM }, ui);
            let lv = song.levels.get(d).cloned().unwrap_or_default();
            text.draw_centered_face(Face::Serif, &lv, r[0] + r[2] * 0.5, r[1] + 26.0 * s, 26.0 * s, fg, ui);
            if let Some(b) = v.results.best(&song.id, &chart.diff) {
                text.draw_centered_face(Face::Body, &format!("{} {:.1}%", b.rank, b.accuracy), r[0] + r[2] * 0.5, r[1] + 58.0 * s, 9.0 * s, if is_sel { fg_dim } else { INK_FAINT }, ui);
            }
        }
    }

    // PLAY
    let pb = cs.play_btn;
    theme::fill(ui, white, pb, INK);
    theme::play_glyph(ui, white, pb[0] + 22.0 * s, pb[1] + (pb[3] - 13.0 * s) * 0.5, 13.0 * s, ON_INK);
    let cap_x = pb[0] + 48.0 * s;
    text.draw_tracked(Face::Serif, "PLAY", cap_x, pb[1] + (pb[3] - 14.0 * s) * 0.5, 14.0 * s, 3.4 * s, ON_INK, ui);
    let key = "ENTER";
    let kw = text.measure_tracked(Face::Serif, key, 8.0 * s, 1.4 * s);
    text.draw_tracked(Face::Serif, key, pb[0] + pb[2] - 14.0 * s - kw, pb[1] + pb[3] - 18.0 * s, 8.0 * s, 1.4 * s, [ON_INK[0], ON_INK[1], ON_INK[2], 0.45], ui);
}

// ===== пауза =====

pub struct PauseUi {
    pub panel: [f32; 4],
    pub buttons: Vec<(u8, [f32; 4])>,
}

pub fn pause_layout(w: f32, h: f32) -> PauseUi {
    let s = theme::ui_scale(w, h);
    let pw = 520.0 * s;
    let ph = 372.0 * s;
    let px = (w - pw) * 0.5;
    let py = (h - ph) * 0.5 - 8.0 * s;
    let bw = 356.0 * s;
    let bh = 46.0 * s;
    let bx = (w - bw) * 0.5;
    let mut buttons = Vec::new();
    for (i, id) in [0u8, 1, 2].iter().enumerate() {
        buttons.push((*id, [bx, py + 168.0 * s + i as f32 * 58.0 * s, bw, bh]));
    }
    PauseUi {
        panel: [px, py, pw, ph],
        buttons,
    }
}

pub fn draw_pause(ctx: &Ctx, ui: &mut Vec<UiQuad>, song_title: &str, diff: &str, buttons: &[(u8, [f32; 4])]) {
    let text = ctx.text;
    let white = ctx.white;
    let (w, h) = (ctx.w, ctx.h);
    let s = ctx.s;
    let p = pause_layout(w, h);
    // затемнение игры
    theme::fill(ui, white, [0.0, 0.0, w, h], [0.0, 0.0, 0.0, 0.62]);
    let r = p.panel;
    theme::fill(ui, white, r, [0.006, 0.006, 0.0078, 1.0]);
    theme::frame(ui, white, r, RULE, s);
    theme::corner_ticks(ui, white, r, [0.84, 0.80, 0.72, 0.30], 16.0 * s, s);

    text.draw_centered_face(Face::Display, "PAUSED", w * 0.5, r[1] + 24.0 * s, 52.0 * s, INK_HI, ui);
    let sub = format!("{} · {}", song_title, diff.to_uppercase());
    let sub = fit_simple(text, &sub, r[2] - 80.0 * s, 10.0 * s);
    text.draw_centered_tracked(Face::Serif, &sub, w * 0.5, r[1] + 92.0 * s, 11.0 * s, 2.6 * s, INK_DIM, ui);
    theme::hair_h(ui, white, r[0] + 40.0 * s, r[1] + 124.0 * s, r[2] - 80.0 * s, RULE, s);

    for &(id, br) in buttons {
        let (cap, kind, key) = match id {
            0 => ("RESUME", Btn::Primary, Some("SPACE")),
            1 => ("SETTINGS", Btn::Ghost, Some("S")),
            _ => ("QUIT TO MENU", Btn::Ghost, Some("ESC")),
        };
        theme::button(text, ui, white, br, cap, kind, key, s);
    }
    text.draw_centered_tracked(
        Face::Serif,
        "SPACE RESUME · S SETTINGS · ESC QUIT",
        w * 0.5,
        r[1] + r[3] - 34.0 * s,
        8.5 * s,
        2.0 * s,
        INK_FAINT,
        ui,
    );
}

// ===== результат =====

pub struct ResultUi {
    pub apply_btn: [f32; 4],
}

/// Масштаб композиции результата: глобальный [`theme::ui_scale`] плюс
/// вертикальное центрирование опорных 720 px на высоких экранах.
/// При 1280×720 возвращает ровно (1.0, 0.0) — композиция не меняется.
pub fn result_scale(w: f32, h: f32) -> (f32, f32) {
    let s = theme::ui_scale(w, h);
    let oy = ((h - theme::BASE_H * s) * 0.5).max(0.0);
    (s, oy)
}

pub fn result_layout(w: f32, h: f32) -> ResultUi {
    let (k, oy) = result_scale(w, h);
    ResultUi {
        apply_btn: [
            w * 0.5 - 230.0 * k,
            oy + 556.0 * k,
            460.0 * k,
            44.0 * k,
        ],
    }
}

pub struct ResultView<'a> {
    pub song_title: &'a str,
    pub artist: &'a str,
    pub diff: &'a str,
    pub play: &'a crate::play::Play,
    pub rank: &'a str,
    pub is_record: bool,
    pub recommended: f32,
    pub mouse_rec: f32,
    pub tablet_rec: f32,
    pub mouse_off: f32,
    pub tablet_off: f32,
    pub mouse_errs: &'a [f32],
    pub tablet_errs: &'a [f32],
}

pub fn draw_result(ctx: &Ctx, v: &ResultView, ui: &mut Vec<UiQuad>) {
    let text = ctx.text;
    let white = ctx.white;
    let (w, h) = (ctx.w, ctx.h);
    let cx = w * 0.5;
    let (k, oy) = result_scale(w, h);
    let y = |v: f32| oy + v * k;
    let z = |v: f32| v * k;

    theme::fill(ui, white, [0.0, 0.0, w, h], theme::BG);
    theme::hair_h(ui, white, MARGIN * k, y(34.0), w - MARGIN * k * 2.0, RULE, k);
    theme::hair_h(ui, white, MARGIN * k, h - y(34.0), w - MARGIN * k * 2.0, RULE, k);

    // шапка
    text.draw_centered_tracked(Face::Serif, "RESULT", cx, y(12.0), z(8.5), 4.0 * k, INK_DIM, ui);
    let title = fit_face(text, Face::Display, v.song_title, w - 240.0 * k, z(34.0), 0.0);
    text.draw_centered_face(Face::Display, &title, cx, y(46.0), z(34.0), INK_HI, ui);
    let sub = format!("{} · {}", v.artist, diff_short(v.diff));
    let sub = fit_simple(text, &sub, w - 240.0 * k, z(10.0));
    text.draw_centered_tracked(Face::Serif, &sub, cx, y(88.0), z(10.0), 2.6 * k, INK_DIM, ui);

    let rank_col = theme::rank_color(v.rank);

    // ранг слева
    let rank_x = cx - 260.0 * k;
    text.draw_centered_face(Face::Display, v.rank, rank_x, y(146.0), z(118.0), rank_col, ui);
    theme::label(text, ui, "RANK", rank_x - 18.0 * k, y(278.0), z(9.0), 3.0 * k, INK_DIM);

    // счёт справа
    let score = fmt_score_local(v.play.score, true);
    let sr = cx + 300.0 * k;
    let sw = text.measure_face(Face::Display, &score, z(58.0));
    text.draw_face(Face::Display, &score, sr - sw, y(148.0), z(58.0), INK_HI, ui);
    let acc = format!("{:.2}%", v.play.accuracy());
    let aw = text.measure_face(Face::Serif, &acc, z(22.0));
    text.draw_face(Face::Serif, &acc, sr - aw, y(220.0), z(22.0), INK, ui);
    let score_l = "SCORE";
    let slw = text.measure_tracked(Face::Serif, score_l, z(8.5), 3.0 * k);
    text.draw_tracked(Face::Serif, score_l, sr - slw, y(252.0), z(8.5), 3.0 * k, INK_FAINT, ui);
    if v.is_record {
        let tag = "NEW RECORD";
        let tw = text.measure_tracked(Face::Serif, tag, z(9.0), 3.0 * k);
        text.draw_tracked(Face::Serif, tag, sr - tw, y(120.0), z(9.0), 3.0 * k, ACCENT_INK, ui);
        theme::fill(ui, white, [sr - tw - 14.0 * k, y(124.0), 6.0 * k, 6.0 * k], ACCENT_INK);
    }
    theme::hair_v(ui, white, cx, y(138.0), z(138.0), RULE, k);

    // статистика: 4 колонки
    let counts = [
        ("PERFECT", format!("{}", v.play.counts[0])),
        ("GOOD", format!("{}", v.play.counts[1])),
        ("MISS", format!("{}", v.play.counts[2])),
        ("MAX COMBO", format!("{}", v.play.max_combo)),
    ];
    let cw = (w - MARGIN * k * 2.0) / counts.len() as f32;
    for (i, (cap, val)) in counts.iter().enumerate() {
        let x0 = MARGIN * k + cw * i as f32;
        let xc = x0 + cw * 0.5;
        let col = match i {
            0 => ACCENT_INK,
            2 => [0.86, 0.42, 0.34, 1.0],
            _ => INK_HI,
        };
        text.draw_centered_face(Face::Serif, val, xc, y(300.0), z(30.0), col, ui);
        text.draw_centered_tracked(Face::Serif, cap, xc, y(338.0), z(9.0), 2.6 * k, INK_DIM, ui);
        if i > 0 {
            theme::hair_v(ui, white, x0, y(304.0), z(44.0), RULE, k);
        }
    }

    // гистограммы таймингов
    let hw = (w - MARGIN * k * 2.0).min(960.0 * k);
    let hx = (w - hw) * 0.5;
    let gap = 56.0 * k;
    let each = (hw - gap) * 0.5;
    draw_histogram(ctx, ui, v.mouse_errs, hx, each, y(400.0), z(110.0), "MOUSE · TAPS", k);
    draw_histogram(ctx, ui, v.tablet_errs, hx + each + gap, each, y(400.0), z(110.0), "TABLET · TAPS", k);

    // кнопка установки оффсета
    let r = result_layout(w, h).apply_btn;
    theme::fill(ui, white, r, PANEL);
    theme::frame(ui, white, r, [0.69, 0.44, 0.12, 0.72], k);
    theme::corner_ticks(ui, white, r, ACCENT, 12.0 * k, k);
    let label = format!("ACCEPT OFFSET  {:+} MS", v.recommended.round() as i32);
    text.draw_centered_tracked(Face::Serif, &label, r[0] + r[2] * 0.5, r[1] + (r[3] - z(11.0)) * 0.5, z(11.0), 2.2 * k, INK_HI, ui);

    let line = format!(
        "split (F3): mouse {:+.0} · tablet {:+.0} ms   [now {:+.0}/{:+.0}]",
        v.mouse_rec, v.tablet_rec, v.mouse_off, v.tablet_off
    );
    text.draw_centered(&line, cx, r[1] + r[3] + z(12.0), z(10.0), INK_DIM, ui);

    text.draw_centered_tracked(
        Face::Serif,
        "ENTER OR CLICK — TO LIBRARY",
        cx,
        h - oy - 56.0 * k,
        z(9.0),
        2.2 * k,
        INK_FAINT,
        ui,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_histogram(
    ctx: &Ctx,
    ui: &mut Vec<UiQuad>,
    errs: &[f32],
    x0: f32,
    hw: f32,
    y0: f32,
    hh: f32,
    label: &str,
    k: f32,
) {
    let text = ctx.text;
    let white = ctx.white;
    let nb = 20usize;
    let mut bins = [0u32; 20];
    for &ms in errs {
        let idx = ((ms + 200.0) / 20.0).floor() as i32;
        if (0..nb as i32).contains(&idx) {
            bins[idx as usize] += 1;
        }
    }
    let maxc = *bins.iter().max().unwrap_or(&1).max(&1);
    let bw = hw / nb as f32;
    let tw = text.measure_tracked(Face::Serif, label, 9.0 * k, 2.2 * k);
    text.draw_tracked(Face::Serif, label, x0 + (hw - tw) * 0.5, y0 - 26.0 * k, 9.0 * k, 2.2 * k, INK_DIM, ui);
    // базовая линия и нулевая ось
    theme::hair_h(ui, white, x0, y0 + hh, hw, RULE, k);
    let zero_x = x0 + hw * 0.5;
    theme::hair_v(ui, white, zero_x, y0, hh, [0.84, 0.80, 0.72, 0.22], k);
    for (i, &c) in bins.iter().enumerate() {
        if c == 0 {
            continue;
        }
        let bh = hh * (c as f32 / maxc as f32);
        let bx = x0 + i as f32 * bw;
        let center_ms = (i as f32 * 20.0) - 200.0 + 10.0;
        let col = if center_ms.abs() <= 50.0 {
            [ACCENT[0], ACCENT[1], ACCENT[2], 0.9]
        } else {
            [COOL[0], COOL[1], COOL[2], 0.85]
        };
        theme::fill(ui, white, [bx + 1.0 * k, y0 + hh - bh, bw - 2.0 * k, bh], col);
    }
    if !errs.is_empty() {
        let mean = errs.iter().sum::<f32>() / errs.len() as f32;
        let mx = x0 + ((mean + 200.0) / 400.0).clamp(0.0, 1.0) * hw;
        theme::fill(ui, white, [mx - 1.0 * k, y0 - 4.0 * k, 2.0 * k, hh + 4.0 * k], ACCENT_INK);
        text.draw_centered(&format!("{mean:+.0}"), mx, y0 + hh + 6.0 * k, 10.0 * k, ACCENT_INK, ui);
    }
    text.draw_centered("-200", x0 + 14.0 * k, y0 + hh + 6.0 * k, 9.0 * k, INK_FAINT, ui);
    text.draw_centered("+200", x0 + hw - 14.0 * k, y0 + hh + 6.0 * k, 9.0 * k, INK_FAINT, ui);
}

// ===== вход =====

pub struct LoginView<'a> {
    pub server: &'a str,
    pub nickname: &'a str,
    pub password_len: usize,
    pub focus: u8,
    pub register: bool,
    pub error: Option<&'a str>,
}

/// Рисует экран входа и возвращает прямоугольники интерактивных зон:
/// 0 — никнейм, 1 — пароль, 2 — отправить, 3 — переключить режим.
pub fn draw_login(ctx: &Ctx, v: &LoginView, ui: &mut Vec<UiQuad>) -> Vec<(u8, [f32; 4])> {
    let text = ctx.text;
    let white = ctx.white;
    let (w, h) = (ctx.w, ctx.h);
    let s = ctx.s;
    let cx = w * 0.5;
    theme::fill(ui, white, [0.0, 0.0, w, h], theme::BG);
    // на высоких экранах (планшет) форма опускается к оптическому центру
    let oy = ((h - theme::BASE_H * s) * 0.5).max(0.0);

    text.draw_centered_face(Face::Display, "DEEMO", cx, 70.0 * s + oy, 64.0 * s, INK_HI, ui);
    theme::label(text, ui, "ACCOUNT", cx - 22.0 * s, 148.0 * s + oy, 8.5 * s, 3.6 * s, INK_FAINT);

    let cw = 420.0 * s;
    let x0 = cx - cw * 0.5;
    let y0 = 200.0 * s + oy;
    theme::hair_h(ui, white, x0, y0, cw, RULE_STRONG, s);

    let mut rects = Vec::new();
    let field = |ctx: &Ctx, ui: &mut Vec<UiQuad>, label: &str, val: &str, y: f32, focused: bool| -> [f32; 4] {
        let text = ctx.text;
        theme::label(text, ui, label, x0, y, 8.5 * s, 2.4 * s, INK_FAINT);
        let r = [x0, y + 18.0 * s, cw, 40.0 * s];
        theme::fill(ui, white, r, if focused { PANEL_HI } else { [0.0, 0.0, 0.0, 0.0] });
        let shown = fit_simple(text, val, cw - 24.0 * s, 15.0 * s);
        text.draw(&shown, x0 + 10.0 * s, y + 28.0 * s, 15.0 * s, INK_HI, ui);
        if focused {
            let tw = text.measure(&shown, 15.0 * s);
            theme::fill(ui, white, [x0 + 11.0 * s + tw, y + 28.0 * s, 1.5 * s, 17.0 * s], ACCENT_INK);
        }
        theme::hair_h(ui, white, x0, r[1] + r[3] - theme::hairline(s), cw, if focused { ACCENT } else { RULE }, s);
        r
    };
    let nr = field(ctx, ui, "NICKNAME", v.nickname, y0 + 22.0 * s, v.focus == 0);
    let masked: String = "•".repeat(v.password_len);
    let pr = field(ctx, ui, "PASSWORD", &masked, y0 + 92.0 * s, v.focus == 1);
    rects.push((0, nr));
    rects.push((1, pr));

    let toggle = if v.register { "Have an account? Sign in" } else { "No account? Create one" };
    let tw = text.measure(toggle, 12.0 * s);
    let tr = [cx - tw * 0.5, y0 + 162.0 * s, tw, 22.0 * s];
    text.draw(&toggle, cx - tw * 0.5, y0 + 164.0 * s, 12.0 * s, ACCENT_INK, ui);
    rects.push((3, tr));

    let sr = [x0, y0 + 202.0 * s, cw, 50.0 * s];
    theme::button(text, ui, white, sr, if v.register { "CREATE ACCOUNT" } else { "SIGN IN" }, Btn::Primary, Some("ENTER"), s);
    rects.push((2, sr));

    if let Some(e) = v.error {
        let e = fit_simple(text, e, cw, 12.0 * s);
        text.draw_centered(&e, cx, y0 + 260.0 * s, 12.0 * s, theme::DANGER_INK, ui);
    }
    let srv = fit_simple(text, v.server, cw, 9.5 * s);
    text.draw_centered(&srv, cx, y0 + 292.0 * s, 9.5 * s, INK_FAINT, ui);
    theme::hair_h(ui, white, cx - 70.0 * s, y0 + 366.0 * s, 140.0 * s, RULE, s);
    text.draw_centered_tracked(Face::Serif, "PIANO · MEMORY · A GIRL", cx, y0 + 380.0 * s, 9.0 * s, 3.4 * s, INK_FAINT, ui);
    text.draw_centered_tracked(Face::Serif, "TAB FIELD · ENTER SUBMIT · ESC BACK", cx, h - oy - 52.0 * s, 9.0 * s, 2.0 * s, INK_FAINT, ui);
    rects
}

// ===== утилиты =====

pub fn diff_short(d: &str) -> &'static str {
    match d {
        "easy" => "EASY",
        "normal" => "NORM",
        "hard" => "HARD",
        "extra" => "EXTRA",
        "expert" => "EXP",
        _ => "?",
    }
}

pub fn fmt_score(s: u64, spaced: bool) -> String {
    let digits = s.to_string();
    if !spaced {
        return digits;
    }
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn fmt_score_local(s: u64, spaced: bool) -> String {
    fmt_score(s, spaced)
}

/// Обрезка строки с «…» под выбранную гарнитуру.
pub fn fit_face(text: &Text, face: Face, s: &str, max_w: f32, size: f32, tracking: f32) -> String {
    if text.measure_tracked(face, s, size, tracking) <= max_w {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let mut cand = out.clone();
        cand.push(ch);
        cand.push('…');
        if text.measure_tracked(face, &cand, size, tracking) > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// Обрезка строки основным шрифтом.
pub fn fit_simple(text: &Text, s: &str, max_w: f32, size: f32) -> String {
    fit_face(text, Face::Body, s, max_w, size, 0.0)
}

/// Всплывающее уведомление (сохранение реплея, обновление библиотеки).
pub fn draw_toast(ctx: &Ctx, ui: &mut Vec<UiQuad>, msg: &str, alpha: f32) {
    let text = ctx.text;
    let white = ctx.white;
    let w = ctx.w;
    let s = ctx.s;
    let tw = text.measure(msg, 12.0 * s) + 46.0 * s;
    let r = [(w - tw) * 0.5, 6.0 * s, tw, 32.0 * s];
    theme::fill(ui, white, r, [0.006, 0.006, 0.008, 0.92 * alpha]);
    theme::frame(ui, white, r, [0.84, 0.80, 0.72, 0.28 * alpha], s);
    theme::fill(ui, white, [r[0], r[1], 2.0 * s, r[3]], [ACCENT[0], ACCENT[1], ACCENT[2], alpha]);
    text.draw_centered(msg, w * 0.5, r[1] + 8.0 * s, 11.5 * s, [0.84, 0.80, 0.72, alpha], ui);
}
