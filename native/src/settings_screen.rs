//! Экран настроек (English UI): онлайн-аккаунт, геймплей, звук.
//!
//! Открывается поверх текущей сцены (библиотека или пауза), поэтому игровое
//! состояние не теряется. Все правки применяются сразу и пишутся в
//! data/settings.json (сервер — в data/auth.json); кнопка Revert возвращает
//! значения, которые были при открытии экрана.

use crate::online::Online;
use crate::render::UiQuad;
use crate::settings::Settings;
use crate::text::Text;
use crate::ui::TextInput;
use winit::keyboard::{Key, NamedKey};

const OK: [f32; 4] = [0.40, 0.72, 0.38, 1.0];
const ERR: [f32; 4] = [0.88, 0.38, 0.30, 1.0];

use crate::text::Face;
use crate::theme::{
    self, Btn, ACCENT, ACCENT_INK, INK_DIM, INK_FAINT, INK_HI, PANEL, PANEL_HI, RULE,
    RULE_STRONG,
};

/// Строка экрана настроек. Порядок вариантов = порядок обхода стрелками
/// (сначала онлайн-карточка, затем геймплей, звук и кнопки внизу).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Server,
    Nickname,
    Password,
    SignIn,
    Register,
    SignOut,
    /// «Download all packs» — скачать все ванильные паки сервера
    Download,
    /// «Install pack from file…» — родной диалог выбора (работает и на Wayland)
    Install,
    Speed,
    XTol,
    Guides,
    MouseAbs,
    LineCursor,
    RmbPen,
    Fullscreen,
    MouseOffset,
    TabletOffset,
    Zoom,
    Press,
    Volume,
    AudioOffset,
    ScrollSpeed,
    Defaults,
    Revert,
    Done,
}

impl Row {
    /// Индекс текстового поля (0..2), если строка — поле ввода.
    fn text_field(self) -> Option<usize> {
        match self {
            Row::Server => Some(0),
            Row::Nickname => Some(1),
            Row::Password => Some(2),
            _ => None,
        }
    }

    /// Строка с числовым значением, которое меняется стрелками.
    fn adjustable(self) -> bool {
        matches!(
            self,
            Row::Speed
                | Row::XTol
                | Row::MouseOffset
                | Row::TabletOffset
                | Row::Zoom
                | Row::Press
                | Row::Volume
                | Row::AudioOffset
                | Row::ScrollSpeed
        )
    }

    /// Строка-переключатель ON/OFF.
    fn toggle(self) -> bool {
        matches!(
            self,
            Row::Guides | Row::MouseAbs | Row::LineCursor | Row::RmbPen | Row::Fullscreen
        )
    }

    /// Кнопка (Enter активирует, значение не показывается).
    fn button(self) -> bool {
        matches!(
            self,
            Row::SignIn
                | Row::Register
                | Row::SignOut
                | Row::Install
                | Row::Defaults
                | Row::Revert
                | Row::Done
        )
    }

    fn label(self) -> &'static str {
        match self {
            Row::Server => "Server URL",
            Row::Nickname => "Nickname",
            Row::Password => "Password",
            Row::SignIn => "Sign in",
            Row::Register => "Create account",
            Row::SignOut => "Sign out",
            Row::Download => "Download all packs",
            Row::Install => "Install pack from file\u{2026}",
            Row::Speed => "Note speed",
            Row::XTol => "X tolerance",
            Row::Guides => "Note guides",
            Row::MouseAbs => "Absolute mouse",
            Row::LineCursor => "Line cursor",
            Row::RmbPen => "RMB = pen press",
            Row::Fullscreen => "Fullscreen",
            Row::MouseOffset => "Mouse offset",
            Row::TabletOffset => "Tablet offset",
            Row::Zoom => "Tablet zoom",
            Row::Press => "Stylus threshold",
            Row::Volume => "Music volume",
            Row::AudioOffset => "Audio offset",
            Row::ScrollSpeed => "Menu scroll speed",
            Row::Defaults => "Restore defaults",
            Row::Revert => "Revert changes",
            Row::Done => "Done",
        }
    }
}

/// Что делать приложению после клавиши/клика.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Close,
    Revert,
    Login,
    Register,
    Logout,
    /// Начать (или отменить/повторить — решает App) скачивание всех паков.
    DownloadPacks,
    /// Открыть родной диалог выбора пака (kdialog/zenity) в фоне.
    InstallPack,
}

/// Фаза строки «Download all packs».
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PacksPhase {
    /// ничего не качаем: показываем «сколько паков / сколько установлено»
    #[default]
    Idle,
    /// идёт фоновая проверка каталога сервера
    Checking,
    /// идёт загрузка
    Running,
    /// загрузка завершилась
    Done,
    /// загрузка отменена пользователем
    Cancelled,
}

/// Состояние строки «Download all packs».
///
/// Живёт в `App` (а не в `SettingsScreen`), чтобы идущая загрузка и прогресс
/// переживали закрытие/открытие экрана настроек; фоновые потоки и опрос канала
/// тоже на стороне `App`. Экран только форматирует и рисует.
#[derive(Clone, Default)]
pub struct PacksUi {
    pub phase: PacksPhase,
    /// сколько ванильных паков в каталоге сервера
    pub total: Option<usize>,
    /// сколько из них уже лежит в data/imported
    pub installed: usize,
    /// id паков каталога — для пересчёта `installed` без сети
    pub ids: Vec<String>,
    /// последний прогресс загрузки
    pub prog: crate::pack_download::Progress,
    /// ошибка проверки каталога
    pub note: Option<String>,
    /// отмена запрошена, но текущий пак ещё докачивается
    pub cancelling: bool,
}

impl PacksUi {
    pub fn running(&self) -> bool {
        self.phase == PacksPhase::Running
    }

    /// Пересчитать `installed` по локальным папкам (без сети).
    pub fn refresh_installed(&mut self) {
        self.installed = crate::pack_download::installed_ids(
            &self.ids,
            std::path::Path::new(crate::pack::IMPORT_DIR),
        );
    }

    /// Значение строки (справа).
    pub fn value(&self) -> String {
        let p = &self.prog;
        match self.phase {
            PacksPhase::Checking => "checking...".into(),
            PacksPhase::Running => {
                if p.total == 0 {
                    "starting...".into()
                } else {
                    format!("{} / {} · {:.0}%", p.done + p.skipped, p.total, p.percent())
                }
            }
            PacksPhase::Done => format!(
                "{} installed · {} failed",
                p.done + p.skipped,
                p.failed.len()
            ),
            PacksPhase::Cancelled => format!("cancelled · {} installed", p.done + p.skipped),
            PacksPhase::Idle => match (self.total, &self.note) {
                (Some(t), _) => format!("{t} packs · {} installed", self.installed),
                (None, Some(_)) => "catalog unavailable".into(),
                (None, None) => "press ENTER to check".into(),
            },
        }
    }

    /// Вторая, мелкая строка: текущий пак, причина ошибки, подсказка.
    pub fn detail(&self) -> String {
        let p = &self.prog;
        match self.phase {
            PacksPhase::Running => {
                let mb = p.bytes as f64 / (1024.0 * 1024.0);
                if p.current.is_empty() {
                    format!("{mb:.1} MB")
                } else {
                    format!("{} · {mb:.1} MB", p.current)
                }
            }
            PacksPhase::Done | PacksPhase::Cancelled => match p.failed.first() {
                Some((id, why)) => format!("{id}: {why}"),
                None => String::new(),
            },
            PacksPhase::Idle => match &self.note {
                Some(e) => e.clone(),
                None => "vanilla packs only · installed folders are skipped".into(),
            },
            PacksPhase::Checking => String::new(),
        }
    }

    /// Зафиксировать последний прогресс: загрузка закончилась (Done) или
    /// была отменена (Cancelled).
    pub fn finish(&mut self, prog: crate::pack_download::Progress) {
        let cancelled = prog.cancelled;
        self.prog = prog;
        self.cancelling = false;
        self.phase = if cancelled {
            PacksPhase::Cancelled
        } else {
            PacksPhase::Done
        };
    }

    /// Прогресс полосы 0..100 (для Done — полная).
    pub fn bar_percent(&self) -> f32 {
        match self.phase {
            PacksPhase::Running | PacksPhase::Cancelled => self.prog.percent(),
            PacksPhase::Done => 100.0,
            _ => 0.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Row(usize),
    Dec(usize),
    Inc(usize),
}

pub struct SettingsScreen {
    /// редактируемые значения (копия живых настроек)
    pub draft: Settings,
    /// значения на момент открытия (для Revert)
    snapshot: Settings,
    /// URL сервера на момент открытия (Revert возвращает и его)
    server0: String,
    pub server: TextInput,
    pub nickname: TextInput,
    pub password: TextInput,
    /// выбранная строка (индекс в rows)
    pub row: usize,
    /// текстовое поле в фокусе: 0=server, 1=nickname, 2=password
    pub focus: Option<usize>,
    /// сообщение о статусе: (текст, успех?)
    pub status: Option<(String, bool)>,
    /// значения изменились и ждут применения к приложению
    pub dirty: bool,
    rows: Vec<Row>,
    row_rect: Vec<[f32; 4]>,
    hit: Vec<(Hit, [f32; 4])>,
    cards: Vec<([f32; 4], &'static str)>,
}

impl SettingsScreen {
    pub fn new(current: Settings, online: &Online) -> SettingsScreen {
        let mut draft = current;
        draft.sanitize();
        SettingsScreen {
            draft,
            snapshot: draft,
            server0: online.state.server.clone(),
            server: TextInput::new(online.state.server.clone(), 160),
            nickname: TextInput::new("", 32),
            password: TextInput::new("", 64),
            row: 0,
            focus: None,
            status: None,
            dirty: false,
            rows: Vec::new(),
            row_rect: Vec::new(),
            hit: Vec::new(),
            cards: Vec::new(),
        }
    }

    pub fn server_text(&self) -> &str {
        &self.server.text
    }

    pub fn nickname_text(&self) -> &str {
        &self.nickname.text
    }

    pub fn password_text(&self) -> &str {
        &self.password.text
    }

    /// Сбросить значения к снимку на момент открытия (кнопка Revert).
    pub fn revert(&mut self) -> Settings {
        self.draft = self.snapshot;
        self.server.text = self.server0.clone();
        self.server.end();
        self.dirty = false;
        self.status = Some(("Reverted to the values from opening Settings".into(), true));
        self.draft
    }

    /// Выбрать строку (для self-test / программного открытия).
    pub fn select(&mut self, row: Row, logged_in: bool) {
        self.rebuild_rows(logged_in);
        if let Some(i) = self.rows.iter().position(|&r| r == row) {
            self.row = i;
        }
        self.focus = None;
    }

    fn rebuild_rows(&mut self, logged_in: bool) {
        self.rows.clear();
        self.rows.extend([
            Row::Server,
            Row::Nickname,
            Row::Password,
            Row::SignIn,
            Row::Register,
            Row::Download,
            Row::Install,
        ]);
        if logged_in {
            self.rows.push(Row::SignOut);
        }
        self.rows.extend([
            Row::Speed,
            Row::XTol,
            Row::Guides,
            Row::MouseAbs,
            Row::LineCursor,
            Row::RmbPen,
            Row::Fullscreen,
            Row::MouseOffset,
            Row::TabletOffset,
            Row::Zoom,
            Row::Press,
            Row::Volume,
            Row::AudioOffset,
            Row::ScrollSpeed,
            Row::Defaults,
            Row::Revert,
            Row::Done,
        ]);
        if self.row >= self.rows.len() {
            self.row = self.rows.len().saturating_sub(1);
        }
    }

    fn place(&mut self, row: Row, rect: [f32; 4]) -> usize {
        if let Some(i) = self.rows.iter().position(|&r| r == row) {
            self.row_rect[i] = rect;
            self.hit.push((Hit::Row(i), rect));
            i
        } else {
            usize::MAX
        }
    }

    fn place_value_row(&mut self, row: Row, rect: [f32; 4], s: f32) {        let i = self.place(row, rect);
        if i == usize::MAX {
            return;
        }
        if row.adjustable() {
            self.hit.push((
                Hit::Dec(i),
                [rect[0] + rect[2] - 130.0 * s, rect[1] + 3.0 * s, 28.0 * s, 28.0 * s],
            ));
            self.hit.push((
                Hit::Inc(i),
                [rect[0] + rect[2] - 40.0 * s, rect[1] + 3.0 * s, 28.0 * s, 28.0 * s],
            ));
        }
    }

    /// Геометрия строки после последнего `draw`/`layout_for_test` (self-test).
    pub fn row_rect(&self, row: Row) -> Option<[f32; 4]> {
        let i = self.rows.iter().position(|&r| r == row)?;
        self.row_rect.get(i).copied()
    }

    /// Прямоугольник группы по заголовку (self-test).
    pub fn card_rect(&self, title: &str) -> Option<[f32; 4]> {
        self.cards
            .iter()
            .find(|(_, t)| *t == title)
            .map(|(r, _)| *r)
    }

    /// Пересчитать геометрию без отрисовки (self-test проверяет раскладку).
    pub fn layout_for_test(&mut self, w: f32, h: f32, logged_in: bool) {
        self.layout(w, h, logged_in);
    }

    fn layout(&mut self, w: f32, h: f32, logged_in: bool) {
        self.rebuild_rows(logged_in);
        self.row_rect.clear();
        self.row_rect.resize(self.rows.len(), [0.0; 4]);
        self.hit.clear();
        self.cards.clear();

        let s = theme::ui_scale(w, h);
        let m = 32.0 * s;
        let top = 92.0 * s;
        // широкие экраны (планшет/4K): страница центрируется и не растягивается
        // шире опорных 1280 px, промасштабированных глобальным s
        let page_w = w.min(theme::BASE_W * s);
        let ox = (w - page_w) * 0.5;
        let lw = (page_w * 0.36).clamp(340.0 * s, 440.0 * s);
        let lx = ox + m;
        let rx = lx + lw + 24.0 * s;
        let rw = (ox + page_w - m - rx).max(320.0 * s);

        // ---- ONLINE ----
        let oy = top;
        let oh = 352.0 * s;
        self.cards.push(([lx, oy, lw, oh], "ONLINE"));
        let fw = lw - 32.0 * s;
        let fx = lx + 16.0 * s;
        let mut fy = oy + 40.0 * s;
        for row in [Row::Server, Row::Nickname, Row::Password] {
            self.place(row, [fx, fy, fw, 52.0 * s]);
            fy += 52.0 * s;
        }
        let bw = (fw - 12.0 * s) * 0.5;
        self.place(Row::SignIn, [fx, oy + 200.0 * s, bw, 32.0 * s]);
        self.place(Row::Register, [fx + bw + 12.0 * s, oy + 200.0 * s, bw, 32.0 * s]);
        // строка «скачать все паки»: подпись + счётчик + полоса прогресса
        self.place(Row::Download, [fx, oy + 234.0 * s, fw, 44.0 * s]);
        // строка «поставить пак из файла»: подпись + подсказка + хоткей F7
        self.place(Row::Install, [fx, oy + 280.0 * s, fw, 40.0 * s]);
        if logged_in {
            self.place(Row::SignOut, [fx + fw - 110.0 * s, oy + 322.0 * s, 110.0 * s, 24.0 * s]);
        }

        // ---- GAMEPLAY ----
        let gy = top;
        // окна судейства и сырые X планшетов зашиты в игру и здесь не показываются
        let gh = 340.0 * s;
        self.cards.push(([rx, gy, rw, gh], "GAMEPLAY"));
        let colw = (rw - 48.0 * s) * 0.5;
        let x1 = rx + 16.0 * s;
        let x2 = rx + 32.0 * s + colw;
        let col1 = [
            Row::Speed,
            Row::XTol,
            Row::Guides,
            Row::MouseAbs,
            Row::LineCursor,
            Row::RmbPen,
        ];
        for (i, row) in col1.iter().enumerate() {
            let y = gy + 52.0 * s + i as f32 * 42.0 * s;
            self.place_value_row(*row, [x1, y, colw, 34.0 * s], s);
        }
        let col2 = [
            Row::MouseOffset,
            Row::TabletOffset,
            Row::Zoom,
            Row::Press,
            Row::Fullscreen,
        ];
        for (i, row) in col2.iter().enumerate() {
            let y = gy + 52.0 * s + i as f32 * 42.0 * s;
            self.place_value_row(*row, [x2, y, colw, 34.0 * s], s);
        }

        // ---- AUDIO ----
        let ay = oy + oh + 16.0 * s;
        let ah = 176.0 * s;
        self.cards.push(([lx, ay, lw, ah], "AUDIO"));
        for (i, row) in [Row::Volume, Row::AudioOffset, Row::ScrollSpeed]
            .iter()
            .enumerate()
        {
            let y = ay + 48.0 * s + i as f32 * 42.0 * s;
            self.place_value_row(*row, [lx + 16.0 * s, y, lw - 32.0 * s, 34.0 * s], s);
        }

        // ---- HELP / DEFAULT-кнопка ----
        let hy = gy + gh + 16.0 * s;
        let hh = 160.0 * s;
        self.cards.push(([rx, hy, rw, hh], "HELP"));
        self.place(
            Row::Defaults,
            [rx + rw - 206.0 * s, hy + hh - 52.0 * s, 190.0 * s, 34.0 * s],
        );

        // ---- нижние кнопки (сразу под контентом) ----
        let content_bottom = (hy + hh).max(ay + ah);
        let by = (content_bottom + 16.0 * s).min(h - 64.0 * s);
        let page_right = ox + page_w - m;
        let done = [page_right - 120.0 * s, by, 120.0 * s, 40.0 * s];
        let revert = [done[0] - 12.0 * s - 170.0 * s, by, 170.0 * s, 40.0 * s];
        self.place(Row::Revert, revert);
        self.place(Row::Done, done);
    }

    fn move_row(&mut self, delta: i32) {
        let n = self.rows.len();
        if n == 0 {
            return;
        }
        let i = self.row.min(n - 1);
        self.row = (i as i32 + delta).rem_euclid(n as i32) as usize;
    }

    fn focus_next(&mut self, dir: i32) {
        let n = self.rows.len();
        if n == 0 {
            return;
        }
        let mut i = self.row;
        for _ in 0..n {
            i = (i as i32 + dir).rem_euclid(n as i32) as usize;
            if let Some(f) = self.rows[i].text_field() {
                self.row = i;
                self.focus = Some(f);
                return;
            }
        }
    }

    fn field_mut(&mut self, idx: usize) -> &mut TextInput {
        match idx {
            1 => &mut self.nickname,
            2 => &mut self.password,
            _ => &mut self.server,
        }
    }

    fn field_ref(&self, idx: usize) -> &TextInput {
        match idx {
            1 => &self.nickname,
            2 => &self.password,
            _ => &self.server,
        }
    }

    /// Изменить числовое значение строки на один шаг.
    fn step(&mut self, row: Row, dir: i32) {
        let d = dir as f32;
        let s = &mut self.draft;
        match row {
            Row::Speed => s.speed += 0.1 * d,
            Row::XTol => s.x_tolerance += 0.1 * d,
            Row::MouseOffset => s.mouse_offset_ms += 5.0 * d,
            Row::TabletOffset => s.tablet_offset_ms += 5.0 * d,
            Row::Zoom => s.tablet_zoom += 0.1 * d,
            Row::Press => s.tablet_press_threshold += dir,
            Row::Volume => s.volume += 0.05 * d,
            Row::AudioOffset => s.audio_offset_ms += 5.0 * d,
            Row::ScrollSpeed => s.scroll_speed += 0.25 * d,
            _ => return,
        }
        self.draft.sanitize();
        self.dirty = true;
    }

    fn toggle(&mut self, row: Row) {
        match row {
            Row::Guides => self.draft.note_guides = !self.draft.note_guides,
            Row::MouseAbs => self.draft.mouse_absolute = !self.draft.mouse_absolute,
            Row::LineCursor => self.draft.line_cursor = !self.draft.line_cursor,
            Row::RmbPen => self.draft.rmb_pen_press = !self.draft.rmb_pen_press,
            Row::Fullscreen => self.draft.fullscreen = !self.draft.fullscreen,
            _ => return,
        }
        self.dirty = true;
    }

    /// Enter/Space по выбранной строке без фокуса.
    fn activate(&mut self) -> Option<Action> {
        let row = *self.rows.get(self.row)?;
        match row {
            Row::Server | Row::Nickname | Row::Password => {
                self.focus = row.text_field();
                None
            }
            Row::SignIn => Some(Action::Login),
            Row::Register => Some(Action::Register),
            Row::SignOut => Some(Action::Logout),
            Row::Download => Some(Action::DownloadPacks),
            Row::Install => Some(Action::InstallPack),
            Row::Defaults => {
                self.draft = Settings::default();
                self.draft.sanitize();
                self.dirty = true;
                self.status = Some((
                    "Defaults restored - press Revert changes to undo".into(),
                    true,
                ));
                None
            }
            Row::Revert => Some(Action::Revert),
            Row::Done => Some(Action::Close),
            r if r.toggle() => {
                self.toggle(r);
                None
            }
            _ => None,
        }
    }

    fn click_row(&mut self, i: usize) -> Option<Action> {
        if i >= self.rows.len() {
            return None;
        }
        self.row = i;
        let row = self.rows[i];
        if row.text_field().is_none() {
            // клик по не-текстовой строке завершает редактирование поля
            self.focus = None;
        }
        match row {
            Row::Server | Row::Nickname | Row::Password => {
                self.focus = row.text_field();
                None
            }
            Row::SignIn => Some(Action::Login),
            Row::Register => Some(Action::Register),
            Row::SignOut => Some(Action::Logout),
            Row::Download => Some(Action::DownloadPacks),
            Row::Install => Some(Action::InstallPack),
            Row::Defaults => {
                self.draft = Settings::default();
                self.draft.sanitize();
                self.dirty = true;
                self.status = Some((
                    "Defaults restored - press Revert changes to undo".into(),
                    true,
                ));
                None
            }
            Row::Revert => Some(Action::Revert),
            Row::Done => Some(Action::Close),
            r if r.toggle() => {
                self.toggle(r);
                None
            }
            r if r.adjustable() => {
                self.step(r, 1);
                None
            }
            _ => None,
        }
    }

    fn adjust_current(&mut self, dir: i32) {
        let Some(&row) = self.rows.get(self.row) else {
            return;
        };
        if row.adjustable() {
            self.step(row, dir);
        } else if row.toggle() {
            self.toggle(row);
        }
    }

    /// Обработка клавиши. `ctrl` — зажат Control (чтобы Ctrl+V и т.п. не
    /// печатали буквы в поле).
    pub fn on_key(&mut self, key: &Key, logged_in: bool, ctrl: bool) -> Option<Action> {
        self.rebuild_rows(logged_in);
        if let Some(f) = self.focus {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.focus = None;
                    return None;
                }
                Key::Named(NamedKey::Enter) => {
                    self.focus = None;
                    self.move_row(1);
                    return None;
                }
                Key::Named(NamedKey::Tab) => {
                    self.focus_next(1);
                    return None;
                }
                Key::Named(NamedKey::Backspace) => {
                    self.field_mut(f).backspace();
                    return None;
                }
                Key::Named(NamedKey::Delete) => {
                    self.field_mut(f).delete();
                    return None;
                }
                Key::Named(NamedKey::Space) => {
                    if !ctrl {
                        self.field_mut(f).insert(" ");
                    }
                    return None;
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    self.field_mut(f).left();
                    return None;
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.field_mut(f).right();
                    return None;
                }
                Key::Named(NamedKey::Home) => {
                    self.field_mut(f).home();
                    return None;
                }
                Key::Named(NamedKey::End) => {
                    self.field_mut(f).end();
                    return None;
                }
                Key::Character(c) => {
                    if !ctrl {
                        self.field_mut(f).insert(c.as_str());
                    }
                    return None;
                }
                _ => return None,
            }
        }
        match key {
            Key::Named(NamedKey::Escape) => Some(Action::Close),
            Key::Named(NamedKey::ArrowUp) => {
                self.move_row(-1);
                None
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.move_row(1);
                None
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.adjust_current(-1);
                None
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.adjust_current(1);
                None
            }
            Key::Named(NamedKey::Tab) => {
                self.move_row(1);
                None
            }
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => self.activate(),
            Key::Character(c) => {
                // печать сразу в выбранное текстовое поле
                if !ctrl {
                    let row = self.rows.get(self.row).copied();
                    if let Some(f) = row.and_then(|r| r.text_field()) {
                        self.focus = Some(f);
                        self.field_mut(f).insert(c.as_str());
                    }
                }
                None
            }
            _ => None,
        }
    }

    pub fn on_click(&mut self, x: f32, y: f32) -> Option<Action> {
        let hit = |r: [f32; 4]| -> bool {
            x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3]
        };
        // сначала маленькие кнопки -/+, они лежат поверх строки
        for (h, r) in self.hit.iter().rev() {
            if !hit(*r) {
                continue;
            }
            match *h {
                Hit::Dec(i) => {
                    if let Some(&row) = self.rows.get(i) {
                        self.row = i;
                        self.step(row, -1);
                    }
                    return None;
                }
                Hit::Inc(i) => {
                    if let Some(&row) = self.rows.get(i) {
                        self.row = i;
                        self.step(row, 1);
                    }
                    return None;
                }
                _ => {}
            }
        }
        for (h, r) in self.hit.iter() {
            if !hit(*r) {
                continue;
            }
            if let Hit::Row(i) = *h {
                return self.click_row(i);
            }
        }
        None
    }

    /// Подпись значения строки.
    fn value(&self, row: Row) -> String {
        let s = &self.draft;
        match row {
            Row::Speed => format!("{:.1}", s.speed),
            Row::XTol => format!("{:.2}", s.x_tolerance),
            Row::Guides => on_off(s.note_guides),
            Row::MouseAbs => on_off(s.mouse_absolute),
            Row::LineCursor => on_off(s.line_cursor),
            Row::RmbPen => on_off(s.rmb_pen_press),
            Row::Fullscreen => on_off(s.fullscreen),
            Row::MouseOffset => format!("{:+.0} ms", s.mouse_offset_ms),
            Row::TabletOffset => format!("{:+.0} ms", s.tablet_offset_ms),
            Row::Zoom => format!("{:.2}x", s.tablet_zoom),
            Row::Press => format!("{}", s.tablet_press_threshold),
            Row::Volume => format!("{:.0}%", s.volume * 100.0),
            Row::AudioOffset => format!("{:+.0} ms", s.audio_offset_ms),
            Row::ScrollSpeed => format!("{:.2}x", s.scroll_speed),
            _ => String::new(),
        }
    }

    /// Нарисовать экран. Вызывает layout (он же пересчитывает hit-прямоугольники).
    /// `packs` — состояние строки «Download all packs» (живёт в App).
    pub fn draw(
        &mut self,
        text: &Text,
        ui: &mut Vec<UiQuad>,
        white: usize,
        w: f32,
        h: f32,
        online: &Online,
        packs: &PacksUi,
    ) {
        let logged_in = online.logged_in();
        self.layout(w, h, logged_in);
        let s = theme::ui_scale(w, h);

        // фон и заголовок
        theme::fill(ui, white, [0.0, 0.0, w, h], theme::BG);
        theme::falling_keys(ui, white, 0.0, w, 0.0, h, 0.0, 0x1b56_c4e9, 0.0022, s);

        let page_w = w.min(theme::BASE_W * s);
        let page_x = (w - page_w) * 0.5;
        text.draw_face(Face::Display, "Settings", page_x + 32.0 * s, 14.0 * s, 44.0 * s, INK_HI, ui);
        theme::label(text, ui, "SYSTEM · CONFIG", page_x + 34.0 * s, 66.0 * s, 8.5 * s, 3.2 * s, INK_FAINT);
        let hint = "changes apply and save immediately";
        let tw = text.measure_tracked(Face::Serif, hint, 9.0 * s, 2.0 * s);
        text.draw_tracked(Face::Serif, hint, page_x + page_w - 32.0 * s - tw, 36.0 * s, 9.0 * s, 2.0 * s, INK_FAINT, ui);

        // группы: тонкая линейка + капс-заголовок (без карточек)
        let cards = self.cards.clone();
        for (r, title) in &cards {
            theme::hair_h(ui, white, r[0], r[1], r[2], RULE, s);
            theme::label(text, ui, title, r[0], r[1] + 10.0 * s, 10.0 * s, 3.0 * s, ACCENT_INK);
        }

        // счётчик активного аккаунта в первой группе (под строкой загрузки;
        // когда залогинен — Sign out занимает правый край, текст короче)
        if let Some((r, _)) = cards.first() {
            let acct = if logged_in {
                format!("SIGNED IN · {}", online.state.nickname.as_deref().unwrap_or("?"))
            } else {
                "NOT SIGNED IN".to_string()
            };
            let text_w = r[2] - 32.0 * s - if logged_in { 116.0 * s } else { 0.0 };
            let acct = fit(text, &acct, text_w, 10.0 * s);
            text.draw_tracked(
                Face::Serif,
                &acct,
                r[0],
                r[1] + 326.0 * s,
                10.0 * s,
                1.8 * s,
                if logged_in { ACCENT_INK } else { INK_FAINT },
                ui,
            );
            if let Some((msg, ok)) = &self.status {
                let col = if msg.ends_with("...") {
                    INK_DIM
                } else if *ok {
                    OK
                } else {
                    ERR
                };
                let msg = fit(text, msg, text_w, 10.0 * s);
                text.draw(&msg, r[0], r[1] + 338.0 * s, 10.0 * s, col, ui);
            }
        }

        // подсказки HELP
        if let Some((r, _)) = cards.get(3) {
            let lines = [
                "UP/DOWN select    LEFT/RIGHT adjust    ENTER edit or activate",
                "ESC back (clears field focus first)    TAB next field",
                "Changes save immediately to data/settings.json.",
                "Judgement windows (perfect/good) and tablet X ranges are fixed in code.",
            ];
            let mut y = r[1] + 44.0 * s;
            for line in lines {
                text.draw(line, r[0], y, 10.5 * s, INK_DIM, ui);
                y += 17.0 * s;
            }
        }

        // строки
        for i in 0..self.rows.len() {
            let row = self.rows[i];
            let r = self.row_rect[i];
            if r[2] <= 0.0 {
                continue;
            }
            let selected = i == self.row;
            if row == Row::Download {
                self.draw_download(text, ui, white, r, selected, packs, s);
                continue;
            }
            if row == Row::Install {
                self.draw_install(text, ui, white, r, selected, s);
                continue;
            }
            if row.text_field().is_some() {
                if selected {
                    theme::fill(ui, white, r, PANEL_HI);
                    theme::fill(ui, white, [r[0], r[1], 2.0 * s, r[3]], ACCENT);
                }
                theme::label(text, ui, &row.label().to_uppercase(), r[0] + 4.0 * s, r[1] + 2.0 * s, 8.5 * s, 2.2 * s, INK_FAINT);
                let f = row.text_field().unwrap();
                let focused = self.focus == Some(f);
                self.draw_field(
                    text,
                    ui,
                    white,
                    [r[0] + 4.0 * s, r[1] + 16.0 * s, r[2] - 8.0 * s, 32.0 * s],
                    f,
                    focused,
                    s,
                );
            } else if row.button() {
                self.draw_button(text, ui, white, r, row, selected, s);
            } else {
                if selected {
                    theme::fill(ui, white, r, PANEL_HI);
                    theme::fill(ui, white, [r[0], r[1], 2.0 * s, r[3]], ACCENT);
                }
                text.draw(&row.label(), r[0] + 10.0 * s, r[1] + 9.0 * s, 13.0 * s, if selected { INK_HI } else { INK_DIM }, ui);
                let val = self.value(row);
                let vw = text.measure_face(Face::Serif, &val, 13.0 * s);
                let vx = r[0] + r[2] - 48.0 * s - vw;
                let vcol = if row.toggle() {
                    if val == "ON" { ACCENT_INK } else { INK_FAINT }
                } else {
                    INK_HI
                };
                text.draw_face(Face::Serif, &val, vx, r[1] + 8.0 * s, 13.0 * s, vcol, ui);
                if row.adjustable() {
                    let dec = [r[0] + r[2] - 130.0 * s, r[1] + 3.0 * s, 28.0 * s, 28.0 * s];
                    let inc = [r[0] + r[2] - 40.0 * s, r[1] + 3.0 * s, 28.0 * s, 28.0 * s];
                    for (b, sym) in [(dec, "-"), (inc, "+")] {
                        theme::frame(ui, white, b, RULE, s);
                        text.draw_centered_face(Face::Serif, sym, b[0] + b[2] * 0.5, b[1] + 7.0 * s, 12.0 * s, INK_DIM, ui);
                    }
                }
            }
        }

        // нижняя подсказка
        text.draw_tracked(
            Face::Serif,
            "ONLINE ROWS ARE SAVED TO data/auth.json AFTER SIGN IN / CREATE ACCOUNT",
            page_x + 32.0 * s,
            h - 18.0 * s,
            7.0 * s,
            1.4 * s,
            [0.84, 0.80, 0.72, 0.22],
            ui,
        );
    }

    /// Строка «Install pack from file…»: подпись, подсказка и хоткей F7.
    /// Enter, клик и F7 открывают родной диалог (на Wayland — единственный путь,
    /// кроме папки data/inbox).
    fn draw_install(
        &self,
        text: &Text,
        ui: &mut Vec<UiQuad>,
        white: usize,
        r: [f32; 4],
        selected: bool,
        s: f32,
    ) {
        if selected {
            theme::fill(ui, white, r, PANEL_HI);
            theme::fill(ui, white, [r[0], r[1], 2.0 * s, r[3]], ACCENT);
        }
        text.draw(
            Row::Install.label(),
            r[0] + 10.0 * s,
            r[1] + 6.0 * s,
            13.0 * s,
            if selected { INK_HI } else { INK_DIM },
            ui,
        );
        let key = "F7";
        let kw = text.measure_tracked(Face::Serif, key, 8.5 * s, 1.8 * s);
        text.draw_tracked(
            Face::Serif,
            key,
            r[0] + r[2] - 10.0 * s - kw,
            r[1] + 7.0 * s,
            8.5 * s,
            1.8 * s,
            ACCENT_INK,
            ui,
        );
        let hint = fit(text, ".deemopack or a pack folder", r[2] - 34.0 * s - kw, 9.5 * s);
        text.draw(
            &hint,
            r[0] + 10.0 * s,
            r[1] + 23.0 * s,
            9.5 * s,
            if selected { INK_DIM } else { INK_FAINT },
            ui,
        );
    }

    fn draw_field(
        &self,
        text: &Text,
        ui: &mut Vec<UiQuad>,
        white: usize,
        r: [f32; 4],
        field: usize,
        focused: bool,
        s: f32,
    ) {
        theme::fill(ui, white, r, if focused { PANEL } else { [0.0, 0.0, 0.0, 0.0] });
        theme::hair_h(
            ui,
            white,
            r[0],
            r[1] + r[3] - theme::hairline(s),
            r[2],
            if focused { ACCENT } else { RULE_STRONG },
            s,
        );
        let input = self.field_ref(field);
        let masked = field == 2;
        let shown_src: String = if masked {
            "*".repeat(input.len_chars())
        } else {
            input.text.clone()
        };
        let inner = r[2] - 20.0 * s;
        let (shown, caret_px) = field_view(text, &shown_src, input.caret, inner, 14.0 * s);
        text.draw(&shown, r[0] + 6.0 * s, r[1] + 8.0 * s, 14.0 * s, INK_HI, ui);
        if focused {
            let cx = r[0] + 6.0 * s + caret_px;
            theme::fill(ui, white, [cx, r[1] + 7.0 * s, 1.5 * s, 18.0 * s], ACCENT_INK);
        }
    }

    fn draw_button(
        &self,
        text: &Text,
        ui: &mut Vec<UiQuad>,
        white: usize,
        r: [f32; 4],
        row: Row,
        selected: bool,
        s: f32,
    ) {
        let primary = matches!(row, Row::SignIn | Row::Done);
        let caption = row.label().to_uppercase();
        let kind = if primary { Btn::Primary } else { Btn::Ghost };
        theme::button(
            text,
            ui,
            white,
            r,
            &caption,
            kind,
            if selected { Some("ENTER") } else { None },
            s,
        );
    }

    /// Строка «Download all packs»: подпись, счётчик/прогресс, вторая строка
    /// (текущий пак или причина ошибки) и тонкая полоса прогресса.
    fn draw_download(
        &self,
        text: &Text,
        ui: &mut Vec<UiQuad>,
        white: usize,
        r: [f32; 4],
        selected: bool,
        packs: &PacksUi,
        s: f32,
    ) {
        if selected {
            theme::fill(ui, white, r, PANEL_HI);
            theme::fill(ui, white, [r[0], r[1], 2.0 * s, r[3]], ACCENT);
        }
        text.draw(
            "Download all packs",
            r[0] + 10.0 * s,
            r[1] + 8.0 * s,
            13.0 * s,
            if selected { INK_HI } else { INK_DIM },
            ui,
        );
        let val = packs.value();
        let vw = text.measure_face(Face::Serif, &val, 13.0 * s);
        text.draw_face(
            Face::Serif,
            &val,
            r[0] + r[2] - 10.0 * s - vw,
            r[1] + 7.0 * s,
            13.0 * s,
            INK_HI,
            ui,
        );

        // справа на второй строке — подсказка отмены, пока идёт загрузка
        // (отмена кооперативная: текущий пак докачивается, поэтому «cancelling»)
        let hint = if packs.running() {
            if packs.cancelling { "CANCELLING..." } else { "ESC CANCELS" }
        } else {
            ""
        };
        let hint_w = if hint.is_empty() {
            0.0
        } else {
            text.measure_tracked(Face::Serif, hint, 8.5 * s, 1.8 * s) + 14.0 * s
        };
        let detail = packs.detail();
        if !detail.is_empty() {
            let d = fit(text, &detail, r[2] - 20.0 * s - hint_w, 9.5 * s);
            let col = if packs.running() { INK_DIM } else { INK_FAINT };
            text.draw(&d, r[0] + 10.0 * s, r[1] + 24.0 * s, 9.5 * s, col, ui);
        }
        if !hint.is_empty() {
            let hw = text.measure_tracked(Face::Serif, hint, 8.5 * s, 1.8 * s);
            text.draw_tracked(
                Face::Serif,
                hint,
                r[0] + r[2] - 10.0 * s - hw,
                r[1] + 24.0 * s,
                8.5 * s,
                1.8 * s,
                ACCENT_INK,
                ui,
            );
        }

        // прогресс: тонкий трек во всю ширину строки + янтарная заливка
        let pct = packs.bar_percent();
        if packs.running() || pct > 0.0 {
            let track = [r[0] + 10.0 * s, r[1] + r[3] - 4.0 * s, r[2] - 20.0 * s, 2.0 * s];
            theme::fill(ui, white, track, RULE);
            if pct > 0.0 {
                let fw = (track[2] * (pct / 100.0).clamp(0.0, 1.0)).max(2.0 * s);
                theme::fill(ui, white, [track[0], track[1], fw, track[3]], ACCENT);
            }
        }
    }
}

fn on_off(v: bool) -> String {
    if v { "ON".into() } else { "OFF".into() }
}

/// Обрезать строку по ширине (для статуса/подписей).
fn fit(text: &Text, s: &str, max_w: f32, size: f32) -> String {
    if text.measure(s, size) <= max_w {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let mut cand = out.clone();
        cand.push(ch);
        if text.measure(&cand, size) > max_w - text.measure("..", size) {
            break;
        }
        out.push(ch);
    }
    out.push_str("..");
    out
}

/// Видимое окно однострочного поля: хвост строки, влезающий в max_w, с
/// кареткой внутри. Возвращает (видимый текст, x каретки от начала поля).
fn field_view(text: &Text, s: &str, caret: usize, max_w: f32, size: f32) -> (String, f32) {
    let chars: Vec<char> = s.chars().collect();
    let caret = caret.min(chars.len());
    let mut start = 0usize;
    let mut caret_px = text.measure(&chars[..caret].iter().collect::<String>(), size);
    while start < caret && caret_px > max_w {
        start += 1;
        caret_px = text.measure(&chars[start..caret].iter().collect::<String>(), size);
    }
    let mut shown: String = chars[start..].iter().collect();
    while !shown.is_empty() && text.measure(&shown, size) > max_w {
        shown.pop();
    }
    (shown, caret_px)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, NamedKey};

    /// Новые переключатели достижимы стрелками и включаются Space, как и
    /// остальные строки. Заодно проверяем порядок строк (рядом с MouseAbs).
    #[test]
    fn line_cursor_and_rmb_are_keyboard_reachable() {
        let on = crate::online::Online {
            state: crate::online::AuthState::default(),
        };
        let mut sc = SettingsScreen::new(Settings::default(), &on);
        let down = Key::Named(NamedKey::ArrowDown);
        let space = Key::Named(NamedKey::Space);
        for _ in 0..11 {
            let _ = sc.on_key(&down, false, false);
        }
        assert_eq!(sc.rows[sc.row], Row::LineCursor, "row {row:?}", row = sc.row);
        let _ = sc.on_key(&space, false, false);
        assert!(sc.draft.line_cursor);
        let _ = sc.on_key(&down, false, false);
        assert_eq!(sc.rows[sc.row], Row::RmbPen);
        let _ = sc.on_key(&space, false, false);
        assert!(sc.draft.rmb_pen_press);
    }
}
