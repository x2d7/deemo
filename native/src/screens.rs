//! Экранные композиции меню: выбор песни («song select»), пауза, результат.
//!
//! Вся геометрия и рисование меню собраны здесь, чтобы их можно было вызвать
//! и из игрового цикла, и из headless-скриншотов (`--shot --screen ...`).
//! Геймплей (ноты, судейская линия, HUD) сюда не входит.

use std::collections::HashMap;

use crate::library::{Entry, Song};
use crate::online::{Online, OnlineRecord, ReplayEntry};
use crate::render::UiQuad;
use crate::results::{fmt_date, Results};
use crate::stars::StarMap;
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
    groups: &[(String, Vec<Entry>)],
    geom: &LibGeom,
    scroll: f32,
) -> (Vec<(Entry, [f32; 4])>, Vec<(String, f32)>, f32) {
    let s = geom.s;
    let mut y = geom.top_h + 18.0 * s;
    let mut cards = Vec::new();
    let mut heads = Vec::new();
    for (name, idxs) in groups {
        heads.push((name.clone(), y - scroll));
        y += 30.0 * s;
        for &e in idxs {
            cards.push((e, [geom.rx, y - scroll, geom.rw, geom.row_h]));
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

/// Всё, что нужно нарисовать выбор песни.
pub struct LibraryView<'a> {
    pub geom: &'a LibGeom,
    pub chrome: &'a LibChrome,
    pub songs: &'a [Song],
    pub results: &'a Results,
    pub online: &'a Online,
    /// кэш звёзд `song_id:diff` (пустой — подписи уровней)
    pub stars: &'a StarMap,
    pub cards: &'a [(Entry, [f32; 4])],
    pub headers: &'a [(String, f32)],
    /// выбранная строка-чарт (песня + сложность)
    pub selected: Option<Entry>,
    pub search: &'a str,
    pub sort_label: &'a str,
    pub group_label: &'a str,
    pub covers: &'a HashMap<usize, usize>,
    pub art: Option<usize>,
    /// песня под курсором (подсветка строки)
    pub hover: Option<Entry>,
    pub lb_online: bool,
    pub lb_pending: bool,
    pub lb_records: &'a [OnlineRecord],
    /// выбранная строка лидерборда и фокус клавиатуры на нём
    pub lb_sel: usize,
    pub lb_focus: bool,
    /// геометрия строк лидерборда (для отрисовки и кликов)
    pub lb_rows: &'a [(usize, [f32; 4])],
    /// строка под курсором (подсветка/подсказка pp · miss)
    pub lb_hover: Option<usize>,
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
    let real_notes = v.bg_t.filter(|_| !v.bg_notes.is_empty());
    // Поле падающих нот — ровно прямоугольник списка песен: ни шапки, ни
    // обложки, ни титульного блока, ни нижней плашки. Один и тот же rect и
    // для отрисовки, и для нитей слайдов.
    let field = [g.rx, g.top_h, g.rw, bottom_y - g.top_h];
    if let Some(t) = real_notes {
        theme::chart_fall(ui, white, field, v.bg_notes, t, 0.066, s, None, Some(field));
    } else {
        theme::falling_keys(
            ui,
            white,
            field[0],
            field[0] + field[2],
            field[1],
            field[1] + field[3],
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
        // звезда выбранной сложности на арте
        if let Some(e) = v.selected {
            let song = &v.songs[e.song];
            let star = crate::stars::star(v.stars, &song.id, song.chart_diff(e.diff));
            let chip = [a[0] + 18.0 * s, a[1] + 18.0 * s, 92.0 * s, 46.0 * s];
            theme::fill(ui, white, chip, [0.004, 0.004, 0.005, 0.72]);
            theme::frame(ui, white, chip, [0.84, 0.80, 0.72, 0.35], s);
            match star {
                Some(star) => {
                    text.draw_face(
                        Face::Serif,
                        &crate::stars::label(star),
                        chip[0] + 10.0 * s,
                        chip[1] + 15.0 * s,
                        20.0 * s,
                        INK_HI,
                        ui,
                    );
                }
                None => {
                    theme::label(text, ui, "LV", chip[0] + 10.0 * s, chip[1] + 8.0 * s, 8.0 * s, 2.2 * s, INK_DIM);
                    let lv = song.chart_level(e.diff);
                    text.draw_face(Face::Serif, lv, chip[0] + 10.0 * s, chip[1] + 19.0 * s, 20.0 * s, INK_HI, ui);
                }
            }
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
    let Some(e) = v.selected else {
        text.draw("Pick a chart", x, y, 14.0 * s, INK_DIM, ui);
        y += 22.0 * s;
        text.draw("from the list to see", x, y, 14.0 * s, INK_FAINT, ui);
        y += 18.0 * s;
        text.draw("its records.", x, y, 14.0 * s, INK_FAINT, ui);
        return;
    };
    let song = &v.songs[e.song];
    let chart = song.charts.get(e.diff);
    let title = fit_face(text, Face::Serif, &song.title, right - x, 17.0 * s, 0.2 * s);
    text.draw_face(Face::Serif, &title, x, y, 17.0 * s, INK_HI, ui);
    y += 24.0 * s;
    let comp = fit_simple(text, &song.composer, right - x, 11.0 * s);
    text.draw(&comp, x, y, 11.0 * s, INK_DIM, ui);

    // шапка выбранного чарта: имя сложности + звезда (без чипов)
    y = v.geom.top_h + 108.0 * s;
    theme::label(text, ui, "CHART", x, y - 16.0 * s, 8.5 * s, 2.2 * s, INK_FAINT);
    if let Some(chart) = chart {
        let name = diff_name(&chart.diff);
        text.draw_face(Face::Serif, name, x, y, 15.0 * s, INK_HI, ui);
        let rating = crate::library::entry_rating_label(v.songs, e, v.stars);
        let rw = text.measure_face(Face::Serif, &rating, 13.0 * s);
        text.draw_face(Face::Serif, &rating, right - rw, y + 1.0 * s, 13.0 * s, ACCENT_INK, ui);
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
        let diff_sh = chart.map(|c| diff_short(&c.diff)).unwrap_or("");
        format!("HISTORY · {diff_sh}")
    };
    theme::section(text, ui, white, &head, x, y, right, INK_FAINT, s);
    if is_online {
        // подсказка клавиатуры справа в шапке, как у списка реплеев раньше
        let hint = if v.lb_focus { "UP/DOWN · ENTER" } else { "TAB" };
        let hw = text.measure_tracked(Face::Serif, hint, 8.0 * s, 1.0 * s);
        text.draw_tracked(
            Face::Serif,
            hint,
            (right - hw).max(x),
            y + 2.0 * s,
            8.0 * s,
            1.0 * s,
            if v.lb_focus { ACCENT_INK } else { INK_FAINT },
            ui,
        );
    }
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
        draw_leader_rows(ctx, v, ui);
        return;
    }

    let sid = &song.id;
    let diff_name = chart.map(|c| c.diff.clone()).unwrap_or_default();
    if let Some(b) = v.results.best(sid, &diff_name) {
        let cap = format!("BEST {} · {:.2}%", b.rank, b.real_accuracy());
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
        draw_record_row(ctx, ui, y, x, right, &r.rank, r.real_accuracy(), &sub);
        y += 44.0 * s;
    }
}

/// Геометрия строк онлайн-лидерборда: одна формула для отрисовки и кликов.
/// Начинается под заголовком `LEADERS` (см. [`draw_records`]).
pub fn leaderboard_rows(g: &LibGeom, bottom_y: f32, count: usize) -> Vec<(usize, [f32; 4])> {
    let s = g.s;
    let row_h = 46.0 * s;
    let top = g.top_h + 210.0 * s;
    let fit = (((bottom_y - 10.0 * s - top) / row_h).floor().max(0.0)) as usize;
    (0..count.min(fit))
        .map(|i| {
            (
                i,
                [
                    g.margin,
                    top + i as f32 * row_h,
                    g.left_w - g.margin * 2.0,
                    row_h,
                ],
            )
        })
        .collect()
}

/// Строка лидерборда: ранг, ник, pp (главное число), настоящая точность,
/// промахи и комбо. Легаси-счёта нет. Маленький ▶ справа — приглашение
/// открыть детали плея.
fn draw_leader_rows(ctx: &Ctx, v: &LibraryView, ui: &mut Vec<UiQuad>) {
    let text = ctx.text;
    let white = ctx.white;
    let s = ctx.s;
    for &(i, r) in v.lb_rows {
        let Some(rec) = v.lb_records.get(i) else { continue };
        let selected = v.lb_focus && i == v.lb_sel;
        let hover = v.lb_hover == Some(i);
        if selected || hover {
            theme::fill(ui, white, r, PANEL_HI);
            if selected {
                theme::fill(ui, white, [r[0], r[1], 2.0 * s, r[3]], ACCENT);
            }
        }
        let x = r[0];
        let right = r[0] + r[2];
        let content_right = right - 24.0 * s;
        let rc = theme::rank_color(&rec.rank);
        text.draw_face(Face::Serif, &rec.rank, x + 8.0 * s, r[1] + 6.0 * s, 16.0 * s, rc, ui);
        let nick = fit_face(
            text,
            Face::Body,
            &rec.nickname,
            (content_right - (x + 42.0 * s) - 100.0 * s).max(30.0 * s),
            12.0 * s,
            0.0,
        );
        text.draw(&nick, x + 42.0 * s, r[1] + 5.0 * s, 12.0 * s, if selected || hover { INK_HI } else { INK }, ui);
        // pp — главное число строки; счёта нет
        let pp = crate::pp::tag(rec.pp);
        let pw = text.measure(&pp, 12.0 * s);
        text.draw(&pp, content_right - pw, r[1] + 5.0 * s, 12.0 * s, if selected || hover { ACCENT_INK } else { INK_HI }, ui);

        // вторая строка: промахи/комбо слева, hit accuracy справа
        let sub = format!("{} MISS · {}x", rec.miss, rec.max_combo);
        let sub = fit_simple(text, &sub, r[2] * 0.45, 8.5 * s);
        text.draw(&sub, x + 42.0 * s, r[1] + 24.0 * s, 8.5 * s, INK_FAINT, ui);
        let acc = format!("{:.2}%", rec.hit_accuracy());
        let aw = text.measure(&acc, 9.0 * s);
        let acc_col = if selected || hover { ACCENT_INK } else { INK_DIM };
        text.draw(&acc, content_right - aw, r[1] + 24.0 * s, 9.0 * s, acc_col, ui);

        // ▶ — открыть детали (мышью или Enter по выбранной строке)
        theme::play_glyph(
            ui,
            white,
            right - 15.0 * s,
            r[1] + (r[3] - 9.0 * s) * 0.5,
            9.0 * s,
            if selected || hover { ACCENT_INK } else { [0.84, 0.80, 0.72, 0.30] },
        );
        theme::hair_h(ui, white, x, r[1] + r[3] - theme::hairline(s), r[2], [0.84, 0.80, 0.72, 0.06], s);
    }
}

/// Строка локальной истории: ранг, hit accuracy и промахи/дата. Счёта в UI
/// больше нет — единственная метрика производительности приходит с сервера.
fn draw_record_row(
    ctx: &Ctx,
    ui: &mut Vec<UiQuad>,
    y: f32,
    x: f32,
    right: f32,
    rank: &str,
    acc: f32,
    sub: &str,
) {
    let text = ctx.text;
    let white = ctx.white;
    let s = ctx.s;
    let rc = theme::rank_color(rank);
    text.draw_face(Face::Serif, rank, x, y - 1.0 * s, 16.0 * s, rc, ui);
    let accs = format!("{acc:.2}%");
    let aw = text.measure(&accs, 10.5 * s);
    text.draw(&accs, right - aw, y + 1.0 * s, 10.5 * s, INK_DIM, ui);
    let sub = fit_simple(text, sub, right - x - 34.0 * s - aw - 8.0 * s, 9.5 * s);
    text.draw(&sub, x + 34.0 * s, y + 2.0 * s, 9.5 * s, INK_FAINT, ui);
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

    // строки: одна строка = один чарт (песня + сложность)
    for &(e, r) in v.cards {
        if r[1] < g.top_h || r[1] + r[3] > bottom_y {
            continue;
        }
        let Some(song) = v.songs.get(e.song) else { continue };
        let Some(chart) = song.charts.get(e.diff) else { continue };
        let is_sel = v.selected == Some(e);
        let is_hover = v.hover == Some(e);
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
        if let Some(&handle) = v.covers.get(&e.song) {
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
        text.draw(&title, tx, r[1] + 15.0 * s, 17.0 * s, title_col, ui);

        // имя сложности — отдельной строкой, до метаданных (osu!-стиль)
        let diff_cap = diff_name(&chart.diff);
        text.draw_tracked(
            Face::Serif,
            diff_cap,
            tx,
            r[1] + 38.0 * s,
            9.5 * s,
            2.0 * s,
            if is_sel { ACCENT_INK } else { INK_DIM },
            ui,
        );
        let comp_w = r[2] - (tx - r[0]) - right_pad
            - text.measure_tracked(Face::Serif, diff_cap, 9.5 * s, 2.0 * s)
            - 14.0 * s;
        let comp = fit_simple(text, &song.composer, comp_w.max(20.0 * s), 10.5 * s);
        text.draw(
            &comp,
            tx + text.measure_tracked(Face::Serif, diff_cap, 9.5 * s, 2.0 * s) + 14.0 * s,
            r[1] + 38.0 * s,
            10.5 * s,
            if is_sel { INK_DIM } else { INK_FAINT },
            ui,
        );

        let mins = (song.duration / 60.0).floor() as u32;
        let secs = (song.duration % 60.0) as u32;
        let rating = crate::library::entry_rating_label(v.songs, e, v.stars);
        let info = format!("{rating} · {} · {}:{:02}", song.album, mins, secs);
        let info = fit_simple(text, &info, r[2] - (tx - r[0]) - right_pad, 9.5 * s);
        text.draw(&info, tx, r[1] + 58.0 * s, 9.5 * s, if is_sel { INK_DIM } else { [0.84, 0.80, 0.72, 0.30] }, ui);

        // справа: лучший результат именно этой сложности
        if let Some(best) = v.results.best(&song.id, &chart.diff) {
            let rc = theme::rank_color(&best.rank);
            let rw = text.measure_face(Face::Display, &best.rank, 26.0 * s);
            text.draw_face(Face::Display, &best.rank, r[0] + r[2] - 18.0 * s - rw, r[1] + 18.0 * s, 26.0 * s, rc, ui);
            let acc = format!("{:.2}%", best.real_accuracy());
            let aw = text.measure(&acc, 11.0 * s);
            text.draw(&acc, r[0] + r[2] - 18.0 * s - aw, r[1] + 48.0 * s, 11.0 * s, INK_DIM, ui);
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

/// pp игрока из серверного лидерборда выбранной карты. Только при входе и
/// только значение `OnlineRecord.pp`; никакой локальной оценки.
fn player_pp(v: &LibraryView) -> Option<f64> {
    let nick = v.online.state.nickname.as_deref()?;
    if !v.online.logged_in() {
        return None;
    }
    v.lb_records
        .iter()
        .find(|r| r.nickname == nick)
        .and_then(|r| r.pp)
        .map(f64::from)
}

fn draw_bottom(ctx: &Ctx, v: &LibraryView, ui: &mut Vec<UiQuad>, by: f32, w: f32, h: f32) {
    let text = ctx.text;
    let white = ctx.white;
    let s = ctx.s;
    theme::fill(ui, white, [0.0, by, w, h - by], [0.006, 0.006, 0.0075, 0.94]);
    theme::hair_h(ui, white, 0.0, by, w, RULE_STRONG, s);

    let Some(e) = v.selected else {
        // главный экран без выбора: подсказки + статистика библиотеки
        text.draw_tracked(Face::Serif, "SONG SELECT", MARGIN * s, by + 22.0 * s, 9.0 * s, 3.0 * s, INK_FAINT, ui);
        text.draw("Click — select a chart and preview", MARGIN * s, by + 44.0 * s, 14.0 * s, INK_HI, ui);
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

    let Some(song) = v.songs.get(e.song) else { return };
    let Some(chart) = song.charts.get(e.diff) else { return };
    // шапка выбранного чарта: имя сложности + звезда (чипов больше нет)
    theme::fill(ui, white, [MARGIN * s, by + 21.0 * s, 5.0 * s, 5.0 * s], ACCENT);
    let sel_cap = format!("SELECTED · {}", diff_name(&chart.diff));
    text.draw_tracked(Face::Serif, &sel_cap, MARGIN * s + 12.0 * s, by + 17.0 * s, 9.0 * s, 2.6 * s, INK_DIM, ui);
    let cap_w = text.measure_tracked(Face::Serif, &sel_cap, 9.0 * s, 2.6 * s);
    let star = crate::stars::star(v.stars, &song.id, &chart.diff);
    let rating = match star {
        Some(star) => crate::stars::label(star),
        None => format!("LV {}", song.chart_level(e.diff)),
    };
    text.draw_face(Face::Serif, &rating, MARGIN * s + 24.0 * s + cap_w, by + 14.0 * s, 12.0 * s, ACCENT_INK, ui);
    let title = fit_face(text, Face::Display, &song.title, 460.0 * s, 30.0 * s, 0.0);
    text.draw_face(Face::Display, &title, MARGIN * s - 2.0 * s, by + 30.0 * s, 30.0 * s, INK_HI, ui);
    let comp = fit_simple(text, &song.composer, 420.0 * s, 11.0 * s);
    text.draw_tracked(Face::Serif, &comp, MARGIN * s, by + 72.0 * s, 11.0 * s, 1.6 * s, INK_DIM, ui);
    let mins = (song.duration / 60.0).floor() as u32;
    let secs = (song.duration % 60.0) as u32;
    let meta = format!("{} · {}:{:02}", song.album, mins, secs);
    let meta = fit_simple(text, &meta, 420.0 * s, 9.5 * s);
    text.draw(&meta, MARGIN * s, by + 92.0 * s, 9.5 * s, INK_FAINT, ui);

    // на месте прежних чипов — лучший результат именно этого чарта.
    // pp берём только из серверной записи игрока; локальный рекорд — только
    // hit accuracy, без придуманного pp и без легаси-счёта.
    let bx = MARGIN * s + 500.0 * s;
    theme::label(text, ui, "BEST ON THIS CHART", bx, by + 28.0 * s, 8.5 * s, 2.2 * s, INK_FAINT);
    let local_best = v.results.best(&song.id, &chart.diff);
    if let Some(pp) = player_pp(v) {
        let val = crate::pp::plain64(pp);
        text.draw_face(Face::Serif, &val, bx, by + 40.0 * s, 30.0 * s, ACCENT_INK, ui);
        let vw = text.measure_face(Face::Serif, &val, 30.0 * s);
        theme::label(text, ui, "PP", bx + vw + 6.0 * s, by + 54.0 * s, 9.0 * s, 2.0 * s, INK_DIM);
        if let Some(b) = local_best {
            let line = format!("{} · {:.2}%", b.rank, b.real_accuracy());
            text.draw(&line, bx, by + 78.0 * s, 9.5 * s, INK_FAINT, ui);
        }
    } else if let Some(b) = local_best {
        text.draw_face(
            Face::Serif,
            &format!("{:.2}%", b.real_accuracy()),
            bx,
            by + 40.0 * s,
            30.0 * s,
            INK,
            ui,
        );
        text.draw(
            &format!("{} · BEST ACCURACY", b.rank),
            bx,
            by + 78.0 * s,
            9.5 * s,
            INK_FAINT,
            ui,
        );
    } else {
        text.draw("Not played yet", bx, by + 48.0 * s, 13.0 * s, INK_FAINT, ui);
    }

    // PLAY
    let cs = v.chrome;
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

/// pp из ответа сервера для экрана результата. Отправка асинхронная, поэтому
/// строка живёт в трёх состояниях: нет данных / ждём очередь / готово.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ResultPp {
    /// Показывать нечего (не вошли в аккаунт — отправлять некуда).
    Hidden,
    /// Рекорд в очереди, ответа сервера ещё нет.
    Pending,
    /// Сервер посчитал pp этой попытки.
    Ready {
        pp: f64,
        /// None — поле отсутствовало в ответе сервера (покажем `—`).
        total_pp: Option<f64>,
        pp_tablet: Option<f64>,
        pp_mouse: Option<f64>,
        record: bool,
    },
}

/// Одна маленькая строка под статистикой; `None` — строку не рисовать.
/// Форматирование (и единственное место, где pp становится строкой) —
/// [`crate::pp::result_line`].
pub fn pp_line(pp: ResultPp) -> Option<String> {
    crate::pp::result_line(pp)
}

/// Крупное число pp на экране результата. `Ready` — значение из ack
/// сервера; `Pending` — `PP PENDING`; `Hidden` — «—» (не вошли или старый
/// сервер не прислал pp).
pub fn result_pp_text(pp: ResultPp) -> String {
    match pp {
        ResultPp::Ready { pp, .. } => crate::pp::signed64(pp),
        ResultPp::Pending => crate::pp::pending_text().to_string(),
        ResultPp::Hidden => "—".to_string(),
    }
}

pub struct ResultView<'a> {
    pub song_title: &'a str,
    pub artist: &'a str,
    pub diff: &'a str,
    /// звезда сложности из кэша сервера; None — звёзд ещё нет (оффлайн).
    pub star: Option<f32>,
    pub play: &'a crate::play::Play,
    pub rank: &'a str,
    pub is_record: bool,
    /// pp-ответ сервера по этой попытке (асинхронно из очереди отправки).
    pub pp: ResultPp,
    /// Рекомендация локального оффсета карты по этому плею (мс);
    /// None — у устройства не было тапов (в UI «—»).
    pub map_mouse: Option<f32>,
    pub map_tablet: Option<f32>,
    /// Уже применённые локальные оффсеты этой карты (мс).
    pub local_mouse: f32,
    pub local_tablet: f32,
    /// Глобальные оффсеты ввода из настроек (добавляются к локальным).
    pub global_mouse: f32,
    pub global_tablet: f32,
    pub mouse_errs: &'a [f32],
    pub tablet_errs: &'a [f32],
}

/// `+7` / `−3` / `—` (нет тапов в этом прохождении).
fn fmt_offset(ms: Option<f32>) -> String {
    match ms {
        Some(v) => format!("{:+}", v.round() as i32),
        None => "—".to_string(),
    }
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
    let sub = match v.star {
        Some(star) => format!("{} · {} · {}", v.artist, diff_short(v.diff), crate::stars::label(star)),
        None => format!("{} · {}", v.artist, diff_short(v.diff)),
    };
    let sub = fit_simple(text, &sub, w - 240.0 * k, z(10.0));
    text.draw_centered_tracked(Face::Serif, &sub, cx, y(88.0), z(10.0), 2.6 * k, INK_DIM, ui);

    let rank_col = theme::rank_color(v.rank);

    // ранг слева
    let rank_x = cx - 260.0 * k;
    text.draw_centered_face(Face::Display, v.rank, rank_x, y(146.0), z(118.0), rank_col, ui);
    theme::label(text, ui, "RANK", rank_x - 18.0 * k, y(278.0), z(9.0), 3.0 * k, INK_DIM);

    // справа — pp из ответа сервера: главное число результата.
    let sr = cx + 300.0 * k;
    let (pp_size, pp_col) = match v.pp {
        ResultPp::Ready { .. } => (58.0, INK_HI),
        ResultPp::Pending => (34.0, INK_DIM),
        ResultPp::Hidden => (58.0, INK_FAINT),
    };
    let pp_text = result_pp_text(v.pp);
    let pw = text.measure_face(Face::Display, &pp_text, z(pp_size));
    text.draw_face(Face::Display, &pp_text, sr - pw, y(148.0), z(pp_size), pp_col, ui);
    let pp_l = "PP";
    let pplw = text.measure_tracked(Face::Serif, pp_l, z(8.5), 3.0 * k);
    text.draw_tracked(Face::Serif, pp_l, sr - pplw, y(212.0), z(8.5), 3.0 * k, INK_FAINT, ui);
    // accuracy — настоящая hit accuracy, не achievement из счёта
    let acc = format!("{:.2}%", v.play.hit_accuracy());
    let aw = text.measure_face(Face::Serif, &acc, z(22.0));
    text.draw_face(Face::Serif, &acc, sr - aw, y(236.0), z(22.0), INK, ui);
    let acc_l = "ACCURACY";
    let alw = text.measure_tracked(Face::Serif, acc_l, z(9.0), 2.2 * k);
    text.draw_tracked(Face::Serif, acc_l, sr - alw, y(270.0), z(9.0), 2.2 * k, INK_DIM, ui);
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

    // Big pp already carries the server value/state; no score line anywhere.

    // гистограммы таймингов
    let hw = (w - MARGIN * k * 2.0).min(960.0 * k);
    let hx = (w - hw) * 0.5;
    let gap = 56.0 * k;
    let each = (hw - gap) * 0.5;
    draw_histogram(ctx, ui, v.mouse_errs, hx, each, y(400.0), z(110.0), "MOUSE · TAPS", k);
    draw_histogram(ctx, ui, v.tablet_errs, hx + each + gap, each, y(400.0), z(110.0), "TABLET · TAPS", k);

    // кнопка установки локального (на карту) оффсета
    let r = result_layout(w, h).apply_btn;
    theme::fill(ui, white, r, PANEL);
    theme::frame(ui, white, r, [0.69, 0.44, 0.12, 0.72], k);
    theme::corner_ticks(ui, white, r, ACCENT, 12.0 * k, k);
    let label = format!(
        "APPLY MAP OFFSET  mouse {} · tablet {} MS",
        fmt_offset(v.map_mouse),
        fmt_offset(v.map_tablet)
    );
    text.draw_centered_tracked(Face::Serif, &label, r[0] + r[2] * 0.5, r[1] + (r[3] - z(11.0)) * 0.5, z(11.0), 2.2 * k, INK_HI, ui);

    let line = format!(
        "local {:+.0}/{:+.0} · global {:+.0}/{:+.0} · this play {} / {} ms",
        v.local_mouse,
        v.local_tablet,
        v.global_mouse,
        v.global_tablet,
        fmt_offset(v.map_mouse),
        fmt_offset(v.map_tablet)
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

// ===== детали плея из лидерборда =====

/// Один онлайн-рекорд с публичным реплеем (если он есть у этого ника).
pub struct PlayDetailView<'a> {
    pub song_title: &'a str,
    pub artist: &'a str,
    pub diff: &'a str,
    pub star: Option<f32>,
    pub record: &'a OnlineRecord,
    /// Публичный реплей этого ника на ту же карту (None — смотреть нечего).
    pub replay: Option<&'a ReplayEntry>,
    /// Можно ли запустить просмотр (ник есть и реплей нашёлся).
    pub can_watch: bool,
    /// Короткая причина, почему просмотр недоступен.
    pub reason: &'a str,
}

pub struct PlayDetailUi {
    /// Кнопка «СМОТРЕТЬ РЕПЛЕЙ» (главное действие, Enter).
    pub watch_btn: [f32; 4],
    /// Крестик/надпись «ЗАКРЫТЬ» в правом верхнем углу.
    pub close_btn: [f32; 4],
}

/// Экран деталей плея: та же типографика и сетка, что у результата, но без
/// глобального хрома библиотеки. Рисуется поверх неё.
pub fn draw_play_detail(ctx: &Ctx, v: &PlayDetailView, ui: &mut Vec<UiQuad>) -> PlayDetailUi {
    let text = ctx.text;
    let white = ctx.white;
    let (w, h) = (ctx.w, ctx.h);
    let cx = w * 0.5;
    let (k, oy) = result_scale(w, h);
    let y = |v: f32| oy + v * k;
    let z = |v: f32| v * k;
    let rec = v.record;

    theme::fill(ui, white, [0.0, 0.0, w, h], theme::BG);
    theme::hair_h(ui, white, MARGIN * k, y(34.0), w - MARGIN * k * 2.0, RULE, k);
    theme::hair_h(ui, white, MARGIN * k, h - y(34.0), w - MARGIN * k * 2.0, RULE, k);
    theme::hair_v(ui, white, cx, y(138.0), z(138.0), RULE, k);

    // шапка
    text.draw_centered_tracked(Face::Serif, "PLAY DETAIL", cx, y(12.0), z(8.5), 4.0 * k, INK_DIM, ui);
    let title = fit_face(text, Face::Display, v.song_title, w - 240.0 * k, z(34.0), 0.0);
    text.draw_centered_face(Face::Display, &title, cx, y(46.0), z(34.0), INK_HI, ui);
    let star = v.star.map(crate::stars::label).unwrap_or_default();
    let mut sub = format!("{} · {} · {}", rec.nickname, v.artist, diff_short(v.diff));
    if !star.is_empty() {
        sub.push_str(" · ");
        sub.push_str(&star);
    }
    let sub = fit_simple(text, &sub, w - 240.0 * k, z(10.0));
    text.draw_centered_tracked(Face::Serif, &sub, cx, y(88.0), z(10.0), 2.6 * k, INK_DIM, ui);

    // закрыть: единственная вторая кнопка экрана
    let close_cap = "CLOSE";
    let cw = text.measure(close_cap, z(10.0));
    let close_rect = [w - MARGIN * k - cw - 10.0 * k, y(8.0), cw + 10.0 * k, z(22.0)];
    text.draw(close_cap, close_rect[0] + 5.0 * k, close_rect[1] + 3.0 * k, z(10.0), INK_FAINT, ui);
    theme::hair_h(ui, white, close_rect[0] + 5.0 * k, close_rect[1] + z(18.0), cw, RULE, k);

    // ранг слева
    let rank_col = theme::rank_color(&rec.rank);
    let rank_x = cx - 260.0 * k;
    text.draw_centered_face(Face::Display, &rec.rank, rank_x, y(146.0), z(118.0), rank_col, ui);
    theme::label(text, ui, "RANK", rank_x - 18.0 * k, y(278.0), z(9.0), 3.0 * k, INK_DIM);

    // pp и настоящая точность попаданий справа; счёта нет
    let sr = cx + 300.0 * k;
    let pp_text = crate::pp::signed(rec.pp);
    let pw = text.measure_face(Face::Display, &pp_text, z(52.0));
    text.draw_face(Face::Display, &pp_text, sr - pw, y(136.0), z(52.0), INK_HI, ui);
    let pp_l = "PP";
    let pplw = text.measure_tracked(Face::Serif, pp_l, z(8.5), 3.0 * k);
    text.draw_tracked(Face::Serif, pp_l, sr - pplw, y(118.0), z(8.5), 3.0 * k, INK_FAINT, ui);
    let acc = format!("{:.2}%", rec.hit_accuracy());
    let aw = text.measure_face(Face::Serif, &acc, z(26.0));
    text.draw_face(Face::Serif, &acc, sr - aw, y(200.0), z(26.0), INK, ui);
    let acc_l = "ACCURACY";
    let alw = text.measure_tracked(Face::Serif, acc_l, z(9.0), 2.2 * k);
    text.draw_tracked(Face::Serif, acc_l, sr - alw, y(236.0), z(9.0), 2.2 * k, INK_DIM, ui);

    // статистика: 4 колонки, как на результате
    let counts = [
        ("PERFECT", format!("{}", rec.perfect)),
        ("GOOD", format!("{}", rec.good)),
        ("MISS", format!("{}", rec.miss)),
        ("MAX COMBO", format!("{}", rec.max_combo)),
    ];
    let cw4 = (w - MARGIN * k * 2.0) / counts.len() as f32;
    for (i, (cap, val)) in counts.iter().enumerate() {
        let x0 = MARGIN * k + cw4 * i as f32;
        let xc = x0 + cw4 * 0.5;
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

    // pp — ровно серверное поле записи (см. crate::pp); реплей-запись может
    // дать только запасную подпись, если у записи pp нет вовсе
    let pp_line = crate::pp::detail_line(rec);
    text.draw_centered_tracked(Face::Serif, &pp_line, cx, y(354.0), z(10.0), 2.2 * k, ACCENT_INK, ui);
    if let Some(note) = crate::pp::replay_pp_note(rec, v.replay) {
        text.draw_centered_tracked(Face::Serif, &note, cx, y(368.0), z(9.0), 1.8 * k, INK_FAINT, ui);
    }
    // дата и скорость
    let ts = if rec.timestamp > 0 {
        rec.timestamp
    } else {
        v.replay.map(|r| r.timestamp).unwrap_or(0)
    };
    let date_speed = if rec.speed > 0.0 {
        format!("{} · SPEED {:.1}", fmt_date(ts), rec.speed)
    } else {
        fmt_date(ts)
    };
    text.draw_centered_tracked(Face::Serif, &date_speed, cx, y(378.0), z(9.5), 2.0 * k, INK_FAINT, ui);

    // главное действие — просмотр реплея
    let watch_btn = result_layout(w, h).apply_btn;
    if v.can_watch {
        theme::fill(ui, white, watch_btn, PANEL);
        theme::frame(ui, white, watch_btn, [0.69, 0.44, 0.12, 0.72], k);
        theme::corner_ticks(ui, white, watch_btn, ACCENT, 12.0 * k, k);
        text.draw_centered("WATCH REPLAY", watch_btn[0] + watch_btn[2] * 0.5, watch_btn[1] + (watch_btn[3] - z(13.0)) * 0.5, z(13.0), INK_HI, ui);
    } else {
        theme::fill(ui, white, watch_btn, [0.84, 0.80, 0.72, 0.02]);
        theme::frame(ui, white, watch_btn, RULE, k);
        text.draw_centered("WATCH REPLAY", watch_btn[0] + watch_btn[2] * 0.5, watch_btn[1] + (watch_btn[3] - z(13.0)) * 0.5, z(13.0), INK_FAINT, ui);
        let reason = fit_simple(text, v.reason, watch_btn[2], z(10.0));
        text.draw_centered(&reason, cx, watch_btn[1] + watch_btn[3] + z(14.0), z(10.0), INK_FAINT, ui);
    }

    let hint = if v.can_watch {
        "ESC / BACKSPACE — CLOSE · ENTER — WATCH"
    } else {
        "ESC / BACKSPACE — CLOSE"
    };
    text.draw_centered(hint, cx, h - oy - 56.0 * k, z(9.5), INK_FAINT, ui);

    PlayDetailUi { watch_btn, close_btn: close_rect }
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

/// Полное имя сложности для строки списка и шапки чарта.
pub fn diff_name(d: &str) -> &'static str {
    match d {
        "easy" => "EASY",
        "normal" => "NORMAL",
        "hard" => "HARD",
        "extra" => "EXTRA",
        "expert" => "EXPERT",
        _ => "?",
    }
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

/// `m:ss` для метки времени реплея.
pub fn fmt_clock(t: f32) -> String {
    let s = t.max(0.0).floor() as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Маленькая метка режима просмотра: `REPLAY · nick` и время `t / конец`.
/// Единственное, что отличает HUD реплея от обычной игры.
pub fn draw_replay_badge(text: &Text, ui: &mut Vec<UiQuad>, nick: &str, t: f32, end: f32) {
    let nick = fit_simple(text, nick, 360.0, 11.0);
    text.draw(&format!("REPLAY · {nick}"), 24.0, 66.0, 11.0, INK_DIM, ui);
    text.draw(
        &format!("{} / {}", fmt_clock(t), fmt_clock(end)),
        24.0,
        81.0,
        10.0,
        INK_FAINT,
        ui,
    );
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

#[cfg(test)]
mod tests {
    use super::fmt_offset;

    /// Подписи локального оффсета на результате: знак и «—» без тапов.
    #[test]
    fn map_offset_labels() {
        assert_eq!(fmt_offset(Some(7.0)), "+7");
        assert_eq!(fmt_offset(Some(-3.0)), "-3");
        assert_eq!(fmt_offset(Some(0.0)), "+0");
        assert_eq!(fmt_offset(None), "—");
    }

    /// Легаси-счёт 0..1,000,000 не должен иметь UI-пути: форматтер удалён,
    /// в screens/app нет его использования, HUD больше не печатает `{:08}`.
    /// Крупное число результата: pp из ack, ожидание или честный прочерк.
    #[test]
    fn result_big_number_is_pp_only() {
        assert_eq!(
            super::result_pp_text(super::ResultPp::Ready {
                pp: 86.2475,
                total_pp: None,
                pp_tablet: None,
                pp_mouse: None,
                record: false,
            }),
            "+86.2"
        );
        assert_eq!(
            super::result_pp_text(super::ResultPp::Pending),
            crate::pp::pending_text()
        );
        assert_eq!(super::result_pp_text(super::ResultPp::Hidden), "—");
    }

    #[test]
    fn legacy_score_is_not_rendered() {
        let screens = include_str!("screens.rs");
        let app = include_str!("app.rs");
        let formatter = ["fmt_", "score"].concat();
        let score_label = ["SC", "ORE"].concat();
        let hud_format = ["format!(\"", "{:08}", "\")"].concat();
        assert!(!screens.contains(&formatter), "score formatter still in UI");
        assert!(!screens.contains(&score_label), "legacy score label still in UI");
        assert!(!app.contains(&hud_format), "HUD still formats the legacy score");
    }
}
