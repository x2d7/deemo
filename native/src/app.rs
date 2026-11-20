//! Окно + игровой цикл: библиотека → игра (звук + судейство) → результат.

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::audio::MusicPlayer;
use crate::chart::Chart;
use crate::clock::Clock;
use crate::library::{self, Entry, Song};
use crate::play::{Judge, Play};
use crate::render::{Renderer, ScreenQuad, UiQuad};
use crate::scene::Scene;
use crate::screens::{self, ResultPp};
use crate::settings::Settings;
use crate::settings_screen::{Action as SettingsAction, PacksPhase, PacksUi, SettingsScreen};
use crate::stage::{active_ahead_time, Tex};
use crate::text::Text;
use crate::Gpu;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

struct Gfx {
    _instance: wgpu::Instance,
    _adapter: wgpu::Adapter,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    music: MusicPlayer,
}

enum Mode {
    Library {
        scroll: f32,
        /// выбранный чарт (песня + сложность); список плоский, osu!-стиль
        selected: Option<Entry>,
    },
    Play {
        song: usize,
        diff: usize,
        chart: Chart,
        play: Play,
        clock: Clock,
        music_on: bool,
        paused: bool,
        /// Some — не игра, а просмотр записанного реплея: ввод выключен,
        /// ноты применяются драйвером, результат не отправляется.
        driver: Option<crate::replay::ReplayDriver>,
    },
    Result {
        song: usize,
        diff: usize,
        play: Play,
    },
    Login {
        nickname: String,
        password: String,
        focus: u8, // 0=никнейм, 1=пароль
        register: bool,
        error: Option<String>,
    },
}

/// Открытые детали одного онлайнового плея (снимок на момент клика/Enter):
/// лидерборд в фоне мог обновиться, но экран не должен моргать.
#[derive(Clone)]
pub struct PlayDetail {
    pub record: crate::online::OnlineRecord,
    pub replay: Option<crate::online::ReplayEntry>,
    pub star: Option<f32>,
}

impl PlayDetail {
    pub fn can_watch(&self) -> bool {
        !self.record.nickname.trim().is_empty() && self.replay.is_some()
    }

    /// Короткая причина, почему «СМОТРЕТЬ РЕПЛЕЙ» выключена.
    pub fn watch_reason(&self) -> &'static str {
        if self.record.nickname.trim().is_empty() {
            "This record has no nickname — no replay to find"
        } else {
            "No replay from this player on the server"
        }
    }
}

/// Запись в списке лидерборда, у которой нашёлся публичный реплей.
pub fn detail_for_record(
    rec: &crate::online::OnlineRecord,
    replays: &[crate::online::ReplayEntry],
    star: Option<f32>,
) -> PlayDetail {
    let nick = rec.nickname.trim();
    let replay = replays
        .iter()
        .find(|r| !nick.is_empty() && r.nick.trim() == nick)
        .cloned();
    PlayDetail { record: rec.clone(), replay, star }
}

/// Предел длины поискового запроса в библиотеке (в символах).
pub const SEARCH_MAX_CHARS: usize = 40;

/// Какие клавиши в библиотеке считаются вводом текста.
///
/// Библиотека — это экран с единственным текстовым полем (поиск песен), поэтому
/// весь печатный ввод должен попадать туда, а не в игровые хоткеи. Пробел
/// приходит как `NamedKey::Space` (а не `Character(" ")`), поэтому его надо
/// забрать отдельно — иначе он уходит в `toggle_pause` и в поиск не попадает.
/// Печатные символы (`!`, `#`, `(`, `[`, `]`, `%`, …) приходят как `Character`.
pub fn library_search_input(key: &Key) -> Option<String> {
    match key {
        Key::Named(NamedKey::Space) => Some(" ".to_string()),
        Key::Character(c) => Some(c.to_string()),
        _ => None,
    }
}

/// Дописывает текст в поисковую строку библиотеки.
///
/// Возвращает `true`, если клавиша была текстовым вводом и её уже обработали
/// (даже когда лимит длины исчерпан): глобальные хоткеи не должны получать
/// нажатие, которое пользователь набрал в поиске. Управляющие символы
/// (Enter, Esc, Tab, Backspace — они приходят как `NamedKey`) отбрасываются.
pub fn push_search_text(search: &mut String, text: &str) -> bool {
    let mut consumed = false;
    for ch in text.chars() {
        if ch.is_control() {
            continue;
        }
        consumed = true;
        if search.chars().count() < SEARCH_MAX_CHARS {
            search.push(ch);
        }
    }
    consumed
}

/// Обрабатывает клавишу как ввод в поисковую строку библиотеки.
///
/// `true` — клавиша была текстом и её уже поглотили: глобальные игровые хоткеи
/// (Space → пауза, «[»/«]» → скорость прокрутки) не должны её видеть. Вне
/// библиотеки возвращает `false`, чтобы обычные хоткеи работали как раньше.
pub fn library_search_key(search: &mut String, key: &Key, in_library: bool) -> bool {
    if !in_library {
        return false;
    }
    match library_search_input(key) {
        Some(text) => push_search_text(search, &text),
        None => false,
    }
}

/// Что клавиша делает со списком онлайн-лидерборда в библиотеке.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LeaderboardKey {
    /// Tab — войти в список или вернуться к песням.
    ToggleFocus,
    Up,
    Down,
    /// Enter — открыть детали выбранного плея.
    Open,
    /// Esc/Backspace — снять фокус (без выхода из библиотеки).
    Back,
}

/// Раскладка клавиш лидерборда: без записей список не перехватывает ничего,
/// поэтому Tab/Space продолжают работать как раньше.
pub fn leaderboard_action(key: &Key, focused: bool, count: usize) -> Option<LeaderboardKey> {
    if count == 0 {
        return None;
    }
    match key {
        Key::Named(NamedKey::Tab) => Some(LeaderboardKey::ToggleFocus),
        Key::Named(NamedKey::ArrowDown) if focused => Some(LeaderboardKey::Down),
        Key::Named(NamedKey::ArrowUp) if focused => Some(LeaderboardKey::Up),
        Key::Named(NamedKey::Enter) if focused => Some(LeaderboardKey::Open),
        Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Backspace) if focused => {
            Some(LeaderboardKey::Back)
        }
        _ => None,
    }
}

/// Фронт нажатия стилуса: `true` ровно один раз на переход hover→press.
///
/// Повторные press-репорты при удержании дают `false` (тап не повторяется),
/// release-репорты обнуляют флаг и тоже дают `false`. Флаг — на каждый планшет
/// свой (Gaomon/Parblo), поэтому нажатие на одном не гасит фронт другого.
pub fn tap_edge(prev: &mut bool, press: bool) -> bool {
    let edge = press && !*prev;
    *prev = press;
    edge
}

/// Текст тоста по итогу прохода очереди. `still_queued` — запись только что
/// сыгранного результата осталась в очереди (отправка не удалась).
pub fn queue_toast(immediate: bool, uploaded: usize, still_queued: bool) -> Option<&'static str> {
    if immediate {
        Some(if still_queued {
            "Score queued — will retry"
        } else {
            "Score uploaded"
        })
    } else if uploaded > 0 {
        Some("Queued score uploaded")
    } else {
        None
    }
}

/// Скрывать ли системный курсор: в игре — всегда при Hide cursor, иначе
/// только в относительном режиме (как раньше). В меню — показывать.
pub fn hide_system_cursor(play: bool, line_cursor: bool, mouse_absolute: bool) -> bool {
    play && (line_cursor || !mouse_absolute)
}

/// Какая рука и какая X у нажатия.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PressDevice {
    Mouse,
    Pen,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PressAim {
    pub device: PressDevice,
    pub x: f32,
    /// Нажатие использует планшетный оффсет (рука пера).
    pub tablet_offset: bool,
}

fn mouse_aim(x: f32) -> PressAim {
    PressAim {
        device: PressDevice::Mouse,
        x,
        tablet_offset: false,
    }
}

fn pen_aim(x: f32) -> PressAim {
    PressAim {
        device: PressDevice::Pen,
        x,
        tablet_offset: true,
    }
}

/// Два курсора, упорядоченные по X: первый — левый (LMB), второй — правый
/// (RMB). При равном X левым считается мышь. Без пера оба — мышь.
pub fn cursor_sides(mouse_x: f32, pen_x: Option<f32>) -> (PressAim, PressAim) {
    match pen_x {
        Some(px) if px < mouse_x => (pen_aim(px), mouse_aim(mouse_x)),
        Some(px) => (mouse_aim(mouse_x), pen_aim(px)),
        None => (mouse_aim(mouse_x), mouse_aim(mouse_x)),
    }
}

/// Нажатие курсора на выбранную руку. Используется и тапом, и руками для
/// статистики; LMB/RMB в RMB-режиме передают сюда левый/правый [`PressAim`].
pub fn apply_cursor_tap(
    play: &mut crate::play::Play,
    chart: &crate::chart::Chart,
    t: f32,
    aim: PressAim,
    hands: &mut crate::play::Hands,
) -> Option<Judge> {
    let judge = play.tap(chart, t, aim.x);
    if let Some(j) = judge {
        match aim.device {
            PressDevice::Pen => hands.tablet.add(j),
            PressDevice::Mouse => hands.mouse.add(j),
        }
    }
    judge
}

/// Press-состояние маркеров. В обычном режиме мышь — по LMB, перо — по
/// tip-press. В RMB-режиме маркеры отражают только кнопки, привязанные к их
/// стороне; касание пера само по себе press не даёт.
pub fn cursor_marker_press(
    rmb_pen_press: bool,
    mouse_x: f32,
    pen_x: Option<f32>,
    left_held: bool,
    right_held: bool,
    pen_tip: bool,
) -> (bool, bool) {
    if !rmb_pen_press {
        return (left_held, pen_x.is_some() && pen_tip);
    }
    let (left, right) = cursor_sides(mouse_x, pen_x);
    let mouse = (left.device == PressDevice::Mouse && left_held)
        || (right.device == PressDevice::Mouse && right_held);
    let pen = (left.device == PressDevice::Pen && left_held)
        || (right.device == PressDevice::Pen && right_held);
    (mouse, pen)
}

/// Первый позиционный аргумент — файл пака (`.deemopack`/`.zip`), который
/// надо поставить при старте, как drag & drop. Флаги (`-…`) и чужие пути
/// игнорируются молча: `deemo --x11 song.chart.json` остаётся обычным запуском.
pub fn pack_path_from_args(args: &[String]) -> Option<std::path::PathBuf> {
    let arg = args.iter().skip(1).find(|a| !a.starts_with('-'))?;
    let path = if arg.starts_with("file://") {
        file_uri_to_path(arg)?
    } else {
        std::path::PathBuf::from(arg)
    };
    if !path.is_file() {
        return None;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "deemopack" || ext == "zip" {
        Some(path)
    } else {
        None
    }
}

/// `file:///path%20with%20spaces` → локальный путь. Для `file://localhost/...`
/// хост отбрасывается; на Windows ведущий `/` перед `C:` снимается.
pub fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    #[allow(unused_mut)]
    let mut decoded = percent_decode(rest);
    #[cfg(windows)]
    {
        let b = decoded.as_bytes();
        if b.first() == Some(&b'/')
            && b.get(1).is_some_and(|c| c.is_ascii_alphabetic())
            && b.get(2) == Some(&b':')
        {
            decoded.remove(0);
        }
    }
    Some(std::path::PathBuf::from(decoded))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| -> Option<u8> {
                match c {
                    b'0'..=b'9' => Some(c - b'0'),
                    b'a'..=b'f' => Some(c - b'a' + 10),
                    b'A'..=b'F' => Some(c - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Тост об успешной загрузке: с pp, если сервер его прислал
/// (`Score uploaded · +172.6 pp`), иначе прежний текст.
pub fn upload_toast(ack: Option<&crate::online_queue::ScoreAck>) -> String {
    match ack {
        Some(a) => format!("Score uploaded · {}", crate::pp::ack_tag(a.pp)),
        None => "Score uploaded".to_string(),
    }
}

/// Ответ сервера по рекорду → состояние строки pp на экране результата.
pub fn result_pp_from_ack(ack: &crate::online_queue::ScoreAck) -> ResultPp {
    ResultPp::Ready {
        pp: ack.pp,
        total_pp: ack.total_pp,
        pp_tablet: ack.pp_tablet,
        pp_mouse: ack.pp_mouse,
        record: ack.record,
    }
}

/// Найти в ответах прохода очередь ответ именно для показанного результата:
/// в одном проходе могут уйти и старые записи из очереди.
pub fn ack_for_result<'a>(
    want: &crate::online_queue::PendingScore,
    acks: &'a [crate::online_queue::UploadedScore],
) -> Option<&'a crate::online_queue::ScoreAck> {
    acks.iter()
        .find(|u| &u.rec == want)
        .and_then(|u| u.ack.as_ref())
}

/// Геометрия меню выбора песен.

pub struct App {
    /// Wayland DnD: winit 0.30 не говорит на wl_data_device, поэтому файлы
    /// принимаем через отдельный data-device (guth-wayland-dnd). Объявлено
    /// ДО `gfx`, чтобы воркер умер раньше окна.
    wayland_dnd: Option<guth_wayland_dnd::FileDnd>,
    gfx: Option<Gfx>,
    songs: Vec<Song>,
    search: String,
    view_groups: Vec<(String, Vec<Entry>)>,
    sort_by: crate::library::SortBy,
    group_by: crate::library::GroupBy,
    /// открытый dropdown: 0=сортировка, 1=группировка
    open_dropdown: Option<u8>,
    results: crate::results::Results,
    online: crate::online::Online,
    /// экран настроек поверх текущей сцены (библиотека или пауза)
    settings: Option<SettingsScreen>,
    /// кнопка SETTINGS в топбаре библиотеки
    settings_btn: [f32; 4],
    /// кнопки панели паузы (0=продолжить, 1=настройки, 2=выход)
    pause_rects: Vec<(u8, [f32; 4])>,
    /// асинхронный вход/регистрация: результат + состояние аккаунта
    auth_rx: Option<Receiver<(std::result::Result<String, String>, crate::online::AuthState)>>,
    auth_pending: bool,
    /// инбокс data/inbox: события фонового наблюдателя (файл сам поставился)
    inbox_rx: Option<Receiver<crate::pack_import::InboxEvent>>,
    /// выбранный в диалоге пак: сам диалог идёт в фоновом потоке, UI не стоит
    pick_rx: Option<Receiver<std::path::PathBuf>>,
    pick_pending: bool,
    /// «скачать все паки»: состояние строки в настройках (живёт вне экрана,
    /// чтобы загрузка переживала его закрытие)
    packs: PacksUi,
    /// фоновая проверка каталога сервера (сколько паков / сколько установлено)
    packs_check_rx: Option<Receiver<std::result::Result<Vec<crate::pack_download::CatalogEntry>, String>>>,
    /// прогресс фоновой загрузки паков
    packs_rx: Option<Receiver<crate::pack_download::Progress>>,
    /// флаг отмены текущей загрузки паков
    packs_cancel: Option<Arc<AtomicBool>>,
    /// зажатые модификаторы (чтобы Ctrl+V не печатал «v» в текстовом поле)
    key_mods: winit::keyboard::ModifiersState,
    login_btn: [f32; 4],
    login_rects: Vec<(u8, [f32; 4])>,
    /// онлайн-лидерборд: тумблер, кэш, асинхронная загрузка
    lb_online: bool,
    lb_records: Vec<crate::online::OnlineRecord>,
    lb_key: String,
    lb_rx: Option<std::sync::mpsc::Receiver<(String, Vec<crate::online::OnlineRecord>, Vec<crate::online::ReplayEntry>)>>,
    lb_pending: Option<String>,
    lb_toggle_rect: [f32; 4],
    /// выбранная строка лидерборда, фокус клавиатуры и геометрия строк
    lb_sel: usize,
    lb_focus: bool,
    lb_rows: Vec<(usize, [f32; 4])>,
    /// открытые детали плея (режим поверх библиотеки)
    lb_detail: Option<PlayDetail>,
    detail_watch_btn: [f32; 4],
    detail_close_btn: [f32; 4],
    /// публичные реплеи выбранной карты (ник → pp/miss и признак «есть реплей»)
    rp_records: Vec<crate::online::ReplayEntry>,
    rp_key: String,
    /// фоновая загрузка одного реплея (DMR1/DMR2/DMR3): song, diff, nick, blob
    rp_blob_rx: Option<Receiver<(usize, usize, String, std::result::Result<Vec<u8>, String>)>>,
    rp_blob_pending: Option<(usize, usize, String)>,
    /// кэш звёзд сложностей (`song_id:diff` → star) и его фоновое обновление
    stars: crate::stars::StarMap,
    stars_rx: Option<Receiver<crate::stars::StarMap>>,
    /// сигнатура папки data/imported (для авто-подхвата новых папок)
    imported_sig: u64,
    last_import_scan: Instant,
    covers: HashMap<String, usize>,
    white_ui: usize,
    /// маски плавных фейдов (см. theme::make_fades)
    fade_x_ui: usize,
    fade_y_ui: usize,
    /// растровый текст (шрифты) — вне Gfx, чтобы headless-скриншоты рисовали
    /// те же экраны без окна
    text: Option<Text>,
    /// старт анимации мотива (падающие клавиши)
    anim_start: Instant,
    /// размер «окна» в headless-режиме
    headless_size: (u32, u32),
    /// геометрия выбора песни, посчитанная в layout()
    lib_geom: Option<screens::LibGeom>,
    lib_headers: Vec<(String, f32)>,
    search_rect: [f32; 4],
    mode: Mode,
    speed: f32,
    scroll_speed: f32,
    fullscreen: bool,
    tablet_zoom: f32,
    audio_offset_ms: f32,
    /// последний результат — новый рекорд?
    result_is_record: bool,
    /// ошибки тапов по устройствам (без слайдеров) для раздельной калибровки
    mouse_tap_errors: Vec<f32>,
    tablet_tap_errors: Vec<f32>,
    /// судейства по рукам за текущий плей: мышь (правая) и перо/планшет (левая).
    /// Авто-Miss добавляются из [`Play::auto_miss`] при показе результата.
    hands: crate::play::Hands,
    /// pp-ответ сервера по результату, который сейчас на экране.
    result_pp: ResultPp,
    /// Запись, отправленная за этот результат: по ней строка pp находит свой
    /// ответ в событиях очереди (а не чужой рекорд из того же прохода).
    result_rec: Option<crate::online_queue::PendingScore>,
    /// локальные оффсеты ввода на карту (song:diff), раздельно мышь/планшет
    map_offsets: crate::map_offsets::MapOffsets,
    /// ключ текущей карты (id песни, имя сложности) для map_offsets
    play_key: (String, String),
    /// рекомендации локального оффсета по этому плею (кнопка результата / F3);
    /// None — у устройства не было тапов, значение карты не меняется
    result_map_mouse: Option<f32>,
    result_map_tablet: Option<f32>,
    /// горизонтальный допуск тапа (мировые ед.)
    x_tolerance: f32,
    /// мышь: абсолютная позиция курсора вместо относительного delta
    mouse_absolute: bool,
    /// «Hide cursor (1D aim)»: скрыть системный курсор в игре; без лучей
    line_cursor: bool,
    /// «RMB = pen press»: правая кнопка ведёт левую руку
    rmb_pen_press: bool,
    /// раздельные оффсеты ввода (мс): + сдвигает тап позже
    mouse_offset_ms: f32,
    tablet_offset_ms: f32,
    /// порог нажатия стилуса Parblo
    tablet_press_threshold: i32,
    /// громкость музыки 0..1 (настройка) и текущий множитель сцены (игра/превью)
    volume: f32,
    music_gain: f32,
    /// фоновая загрузка полноразмерного арта выбранной песни
    full_rx: Option<Receiver<(String, u32, u32, Vec<u8>)>>,
    full_pending: Option<usize>,
    full_loaded: Option<String>,
    /// родные размеры арта песни (для натурального аспекта), кэш
    full_dims: HashMap<usize, (u32, u32)>,
    note_guides: bool,
    tablets: Option<crate::tablet::Tablets>,
    /// press-edge Gaomon/Parblo раздельно: у каждого планшета свой флаг, иначе
    /// второй планшет «съедает» нажатие первого.
    gaomon_was_pressed: bool,
    parblo_was_pressed: bool,
    /// очередь отправки рекордов (файл + фоновый поток)
    queue: crate::online_queue::OnlineQueue,
    /// номера команд очереди для только что сыгранных результатов
    score_reports: Vec<(u64, crate::online_queue::PendingScore)>,
    /// когда последний раз просили фоновую отправку очереди
    last_queue_flush: Instant,
    music_ended_at: Option<Instant>,
    left_held: bool,
    /// правая кнопка: источник левой руки при `rmb_pen_press`
    right_held: bool,
    /// игровой курсор мыши (мировой X на линии), двигается по delta X
    mouse_x: f32,
    cursor_grabbed: bool,
    cursor: [f32; 2],
    frames: u32,
    fps_last: Instant,
    next_frame: Instant,
    cover_rx: Option<Receiver<(String, u32, u32, Vec<u8>)>>,
    auto_shot: Option<(String, Instant)>,
    auto_shot_delay: f32,
    pending_play: Option<(usize, usize)>,
    /// `.deemopack` из первого аргумента: ставим после открытия окна, чтобы
    /// тост «Installed …» не истёк за время инициализации GPU.
    startup_pack: Option<std::path::PathBuf>,
    card_rects: Vec<(Entry, [f32; 4])>,
    play_btn: [f32; 4],
    sort_btn: [f32; 4],
    group_btn: [f32; 4],
    dropdown_rects: Vec<(u8, [f32; 4])>,
    total_h: f32,
    last_selected: Option<Entry>,
    /// фон меню: миниатюрная падающая сцена выбранного чарта
    bg_field: crate::bg::BgField,
    /// ключ (песня, сложность) чарта, который сейчас пишет в поле
    bg_key: Option<(String, String)>,
    bg_rx: Option<Receiver<((String, String), Option<crate::bg::BgSource>)>>,
    /// прошлый кадр для дельты часов фона
    bg_last: Option<Instant>,
    /// фиксированное время для headless-демо (`--demo`): превью в шоте не играет
    bg_t_demo: Option<f32>,
    /// сохранённая позиция библиотеки (восстанавливается после игры)
    saved_scroll: f32,
    /// геометрия скроллбара (дорожка, ползунок) и захват при перетаскивании
    sbar_track: Option<[f32; 4]>,
    sbar_thumb: Option<[f32; 4]>,
    sbar_drag: Option<f32>, // смещение курсора внутри ползунка (px)
    /// сырая позиция системного курсора (может двигаться пером); для
    /// фильтра pen-emulated кликов, но не для прицеливания/UI
    system_cursor: [f32; 2],
    saved_selected: Option<Entry>,
    /// запись реплея (сырой ввод) во время игры
    replay: Option<crate::replay::Replay>,
    /// всплывающее уведомление (текст, момент показа)
    toast: Option<(String, Instant)>,
}

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const DIM: [f32; 4] = [1.0, 1.0, 1.0, 0.6];
/// Хвост после последней ноты в просмотре реплея (сек): эффект попадания и
/// затухание успевают доиграть, результат не показывается.
const REPLAY_TAIL: f32 = 1.5;

impl App {
    fn new() -> Result<App> {
        let songs = library::load()?;
        let pending_play = std::env::var("DEEMO_PLAY").ok().and_then(|v| {
            let (id, diff) = v.split_once(':')?;
            let si = songs.iter().position(|s| s.id == id)?;
            let di = songs[si].charts.iter().position(|c| c.diff == diff).unwrap_or(0);
            Some((si, di))
        });
        let settings = crate::settings::load();
        let preselect = std::env::var("DEEMO_SELECT")
            .ok()
            .and_then(|id| {
                songs
                    .iter()
                    .position(|s| s.id == id)
                    .map(|si| Entry::new(si, 0))
            })
            .or_else(|| {
                songs
                    .iter()
                    .position(|s| !s.charts.is_empty())
                    .map(|si| Entry::new(si, 0))
            });
        Ok(App {
            wayland_dnd: None,
            gfx: None,
            songs,
            search: std::env::var("DEEMO_SEARCH").unwrap_or_default(),
            view_groups: Vec::new(),
            sort_by: crate::library::SortBy::Title,
            group_by: crate::library::GroupBy::Album,
            open_dropdown: None,
            results: crate::results::Results::load(),
            online: crate::online::Online::load(),
            settings: None,
            settings_btn: [0.0; 4],
            pause_rects: Vec::new(),
            auth_rx: None,
            auth_pending: false,
            inbox_rx: None,
            pick_rx: None,
            pick_pending: false,
            packs: PacksUi::default(),
            packs_check_rx: None,
            packs_rx: None,
            packs_cancel: None,
            key_mods: winit::keyboard::ModifiersState::empty(),
            login_btn: [0.0; 4],
            login_rects: Vec::new(),
            lb_online: false,
            lb_records: Vec::new(),
            lb_key: String::new(),
            lb_rx: None,
            lb_pending: None,
            lb_toggle_rect: [0.0; 4],
            lb_sel: 0,
            lb_focus: false,
            lb_rows: Vec::new(),
            lb_detail: None,
            detail_watch_btn: [0.0; 4],
            detail_close_btn: [0.0; 4],
            rp_records: Vec::new(),
            rp_key: String::new(),
            rp_blob_rx: None,
            rp_blob_pending: None,
            stars: crate::stars::load(),
            stars_rx: None,
            imported_sig: 0,
            last_import_scan: Instant::now(),
            covers: HashMap::new(),
            white_ui: 0,
            fade_x_ui: 0,
            fade_y_ui: 0,
            text: None,
            anim_start: Instant::now(),
            headless_size: (1280, 720),
            lib_geom: None,
            lib_headers: Vec::new(),
            search_rect: [0.0; 4],
            mode: Mode::Library {
                scroll: 0.0,
                selected: preselect,
            },
            speed: settings.speed.clamp(0.5, 9.5),
            scroll_speed: settings.scroll_speed.clamp(0.25, 5.0),
            fullscreen: settings.fullscreen,
            tablet_zoom: settings.tablet_zoom,
            audio_offset_ms: settings.audio_offset_ms,
            result_is_record: false,
            mouse_tap_errors: Vec::new(),
            tablet_tap_errors: Vec::new(),
            hands: crate::play::Hands::default(),
            result_pp: ResultPp::Hidden,
            result_rec: None,
            map_offsets: crate::map_offsets::MapOffsets::load(),
            play_key: (String::new(), String::new()),
            result_map_mouse: None,
            result_map_tablet: None,
            full_rx: None,
            full_pending: None,
            full_loaded: None,
            full_dims: HashMap::new(),
            note_guides: settings.note_guides,
            x_tolerance: settings.x_tolerance,
            mouse_absolute: settings.mouse_absolute,
            line_cursor: settings.line_cursor,
            rmb_pen_press: settings.rmb_pen_press,
            mouse_offset_ms: settings.mouse_offset_ms,
            tablet_offset_ms: settings.tablet_offset_ms,
            tablet_press_threshold: settings.tablet_press_threshold,
            volume: settings.volume,
            music_gain: 1.0,
            tablets: None,
            gaomon_was_pressed: false,
            parblo_was_pressed: false,
            queue: crate::online_queue::OnlineQueue::open(crate::online_queue::QUEUE_PATH),
            score_reports: Vec::new(),
            last_queue_flush: Instant::now(),
            music_ended_at: None,
            left_held: false,
            right_held: false,
            mouse_x: 0.0,
            cursor_grabbed: false,
            pending_play,
            startup_pack: None,
            cursor: [0.0, 0.0],
            auto_shot: std::env::var("DEEMO_AUTOSHOT").ok().map(|p| (p, Instant::now())),
            auto_shot_delay: std::env::var("DEEMO_AUTOSHOT_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(1.5),
            frames: 0,
            fps_last: Instant::now(),
            next_frame: Instant::now(),
            cover_rx: None,
            card_rects: Vec::new(),
            play_btn: [0.0; 4],
            sort_btn: [0.0; 4],
            group_btn: [0.0; 4],
            dropdown_rects: Vec::new(),
            total_h: 0.0,
            last_selected: None,
            bg_field: crate::bg::BgField::default(),
            bg_key: None,
            bg_rx: None,
            bg_last: None,
            bg_t_demo: None,
            saved_scroll: 0.0,
            sbar_track: None,
            sbar_thumb: None,
            sbar_drag: None,
            system_cursor: [0.0, 0.0],
            saved_selected: None,
            replay: None,
            toast: None,
        })
    }

    fn size(&self) -> (u32, u32) {
        self.gfx
            .as_ref()
            .map(|g| (g.config.width, g.config.height))
            .unwrap_or(self.headless_size)
    }

    /// Время для медленной анимации мотива меню (сек).
    fn anim_t(&self) -> f32 {
        self.anim_start.elapsed().as_secs_f32()
    }

    fn thumb_path(&self, song: usize) -> Option<String> {
        let p = self.songs.get(song)?.cover.as_ref()?;
        Some(p.replace("data/covers/", "data/covers_thumb/"))
    }

    /// Обложка из кэша (декодирование идёт в фоне, см. spawn_cover_preloader).
    fn cover(&self, song: usize) -> Option<usize> {
        let path = self.thumb_path(song)?;
        self.covers.get(&path).copied()
    }

    /// Полноразмерный арт песни для задника (ленивая фоновая загрузка).
    fn full_cover(&mut self, song: usize) -> Option<usize> {
        let path = self.songs.get(song)?.cover.clone()?;
        if let Some(&h) = self.covers.get(&path) {
            return Some(h);
        }
        if self.full_pending == Some(song) {
            return None;
        }
        self.full_pending = Some(song);
        let (tx, rx) = std::sync::mpsc::channel();
        self.full_rx = Some(rx);
        std::thread::spawn(move || {
            if let Ok(img) = image::open(&path) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let _ = tx.send((path, w, h, rgba.into_raw()));
            }
        });
        None
    }

    /// Фоновое декодирование миниатюр обложек, чтобы скролл не лагал.
    /// Миниатюры, которых ещё нет в кэше (новые паки добавляются на лету).
    fn missing_thumb_paths(&self) -> Vec<String> {
        (0..self.songs.len())
            .filter_map(|i| self.thumb_path(i))
            .filter(|p| !self.covers.contains_key(p))
            .collect()
    }

    fn spawn_cover_preloader(&mut self) {
        let paths = self.missing_thumb_paths();
        if paths.is_empty() {
            return;
        }
        println!("thumbs: loading {} missing", paths.len());
        let (tx, rx) = std::sync::mpsc::channel();
        self.cover_rx = Some(rx);
        std::thread::spawn(move || {
            for path in paths {
                let Ok(img) = image::open(&path) else {
                    continue;
                };
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                if tx.send((path, w, h, rgba.into_raw())).is_err() {
                    break;
                }
            }
        });
    }

    /// Загрузить в GPU немного готовых обложек за кадр (без блокировок).
    fn drain_covers(&mut self) {
        // миниатюры (потоковая предзагрузка)
        let mut batch = Vec::new();
        if let Some(rx) = &self.cover_rx {
            for _ in 0..16 {
                match rx.try_recv() {
                    Ok(item) => batch.push(item),
                    Err(_) => break,
                }
            }
        }
        // полноразмерный арт задника — НЕ зависит от миниатюр (иначе после
        // окончания предзагрузки batch пуст и арт никогда не догружается)
        let full = self.full_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if batch.is_empty() && full.is_none() {
            return;
        }
        let g = self.gfx.as_mut().unwrap();
        for (path, w, h, data) in batch {
            let handle = g.renderer.add_ui_texture(&path, w, h, &data);
            self.covers.insert(path, handle);
        }
        if let Some((path, w, h, data)) = full {
            let handle = g.renderer.add_ui_texture(&path, w, h, &data);
            if let Some(old) = self.full_loaded.take() {
                if old != path {
                    self.covers.remove(&old);
                }
            }
            if let Some(si) = self.full_pending {
                self.full_dims.insert(si, (w, h));
            }
            self.covers.insert(path.clone(), handle);
            self.full_loaded = Some(path);
            self.full_pending = None;
            self.full_rx = None;
        }
    }

    fn start_play(&mut self, song: usize, diff: usize) {
        let _ = self.start_play_impl(song, diff, None);
    }

    /// Запуск карты. `replay` — просмотр записанного DMR1/DMR2/DMR3 вместо игры: драйвер
    /// проверяет, что блоб описывает ровно этот чарт, и играет его без ввода.
    fn start_play_impl(
        &mut self,
        song: usize,
        diff: usize,
        replay: Option<(&str, &crate::replay::ReplayBlob)>,
    ) -> std::result::Result<(), String> {
        // запоминаем позицию библиотеки, чтобы вернуть её после игры
        if let Mode::Library { scroll, selected } = &self.mode {
            self.saved_scroll = *scroll;
            self.saved_selected = *selected;
        }
        let Some(c) = self.songs[song].charts.get(diff) else {
            return Err("Chart is not available".into());
        };
        let chart_path = c.path.clone();
        let chart_diff = c.diff.clone();
        let chart = Chart::load(&chart_path)
            .map_err(|_| "Chart is not available".to_string())?;
        let song_id = self.songs[song].id.clone();
        // драйвер строится до смены режима: несовпадение числа нот оставляет
        // игрока в библиотеке с короткой ошибкой, а не в чужом чарте
        let driver = match replay {
            Some((nick, blob)) => {
                // decoded note_count обязан совпасть с чартом: иначе это блоб
                // от другой версии карты и играть его нельзя
                if blob.note_count() != chart.notes.len() {
                    return Err("Replay does not match this chart".to_string());
                }
                Some(
                    crate::replay::ReplayDriver::new(&chart, blob, nick)
                        .map_err(|_| "Replay does not match this chart".to_string())?,
                )
            }
            None => None,
        };
        // локальные оффсеты карты относятся к song:diff — фиксируем ключ на всё
        // время плея (результат и кнопка на нём используют тот же ключ)
        self.play_key = (song_id.clone(), chart_diff.clone());
        let mut play = Play::new(&chart);
        play.set_x_tolerance(self.x_tolerance);
        // lead-in: ноты успевают упасть сверху до первой ноты
        let lead_in = active_ahead_time(self.speed) + 0.5;
        let clock = Clock::new(lead_in, self.audio_offset_ms / 1000.0);
        // реплей: пишем сырой ввод всей сессии (сохранение по F2). В просмотре
        // реплея писать нечего — ввода нет.
        self.replay = if driver.is_some() {
            None
        } else {
            Some(crate::replay::Replay::new(
                &song_id,
                &chart_diff,
                &self.songs[song].title,
                &chart,
                self.speed,
                self.audio_offset_ms,
                self.tablet_zoom,
                self.x_tolerance,
            ))
        };
        let (input_mouse, input_tablet) = self.input_offsets();
        if let Some(r) = &mut self.replay {
            r.set_input_offsets(input_mouse, input_tablet);
        }
        self.music_gain = 1.0;
        if let Some(g) = self.gfx.as_mut() {
            if let Some(audio) = &self.songs[song].audio {
                if g.music.load(audio).is_ok() {
                    g.music.set_volume(self.volume);
                    g.music.seek(0.0);
                    g.music.pause();
                }
            }
        }
        self.mode = Mode::Play {
            song,
            diff,
            chart,
            play,
            clock,
            music_on: false,
            paused: false,
            driver,
        };
        // детали плея/фокус лидерборда не должны остаться поверх игры
        self.lb_detail = None;
        self.detail_watch_btn = [0.0; 4];
        self.detail_close_btn = [0.0; 4];
        self.mouse_x = 0.0;
        self.music_ended_at = None;
        self.mouse_tap_errors.clear();
        self.tablet_tap_errors.clear();
        self.hands = crate::play::Hands::default();
        self.result_pp = ResultPp::Hidden;
        self.result_rec = None;
        // у каждого планшета свой press-edge; иначе зажатый стилус с прошлой
        // песни давал бы фантомный тап в начале новой
        self.gaomon_was_pressed = false;
        self.parblo_was_pressed = false;
        self.right_held = false;
        self.set_play_cursor(true);
        Ok(())
    }

    /// Скачанный блоб → просмотр. Ошибки — коротким тостом, без выхода из
    /// библиотеки: в чужой чарт или пустой экран игрок не попадает.
    fn start_replay_play(&mut self, song: usize, diff: usize, nick: &str, bytes: &[u8]) {
        let blob = match crate::replay::ReplayBlob::decode(bytes) {
            Ok(b) => b,
            Err(_) => {
                self.toast = Some(("Replay unavailable".into(), Instant::now()));
                return;
            }
        };
        if let Err(msg) = self.start_play_impl(song, diff, Some((nick, &blob))) {
            self.toast = Some((msg, Instant::now()));
        }
    }

    /// Запросить блоб реплея этого ника для выбранного чарта (сеть — в фоне,
    /// кадр не стоит). Путь тот же, что был у строки списка реплеев.
    fn start_replay_download(&mut self, nick: &str) {
        let (si, diff) = match &self.mode {
            Mode::Library { selected: Some(e), .. } => (e.song, e.diff),
            _ => return,
        };
        let nick = nick.trim().to_string();
        if nick.is_empty() {
            return;
        }
        let key = (si, diff, nick.clone());
        if self.rp_blob_pending.as_ref() == Some(&key) {
            return;
        }
        let Some(song_id) = self.songs.get(si).map(|s| s.id.clone()) else { return };
        let diff_name = self
            .songs
            .get(si)
            .and_then(|s| s.charts.get(diff))
            .map(|c| c.diff.clone())
            .unwrap_or_default();
        let online = self.online.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.rp_blob_rx = Some(rx);
        self.rp_blob_pending = Some(key);
        std::thread::spawn(move || {
            let res = online.replay_blob(&song_id, &diff_name, &nick);
            let _ = tx.send((si, diff, nick, res));
        });
    }

    /// «СМОТРЕТЬ РЕПЛЕЙ» в деталях: только если сервер отдал публичный реплей
    /// этого ника на эту карту.
    fn watch_detail_replay(&mut self) {
        let Some(detail) = &self.lb_detail else { return };
        if !detail.can_watch() {
            return;
        }
        let nick = detail.record.nickname.clone();
        self.start_replay_download(&nick);
    }

    /// Открыть детали плея по строке лидерборда. Реплей ищется по нику в
    /// публичном списке карты; его отсутствие — не ошибка, кнопка просто
    /// выключена.
    fn open_play_detail(&mut self, idx: usize) {
        let Some(rec) = self.lb_records.get(idx).cloned() else { return };
        let star = match &self.mode {
            Mode::Library { selected: Some(e), .. } => {
                let song = match self.songs.get(e.song) {
                    Some(s) => s,
                    None => return,
                };
                crate::stars::star(&self.stars, &song.id, song.chart_diff(e.diff))
            }
            _ => None,
        };
        self.lb_sel = idx;
        self.lb_focus = true;
        self.lb_detail = Some(detail_for_record(&rec, &self.rp_records, star));
    }

    fn close_play_detail(&mut self) {
        self.lb_detail = None;
        self.detail_watch_btn = [0.0; 4];
        self.detail_close_btn = [0.0; 4];
        // возвращаем фокус лидерборду: клавиатурный путь не теряется
        self.lb_focus = true;
    }

    /// Клавиши открытых деталей: Esc/Backspace — закрыть, Enter — смотреть.
    /// true — клавиша обработана.
    fn detail_key(&mut self, key: &winit::keyboard::Key) -> bool {
        let Some(detail) = &self.lb_detail else { return false };
        match key {
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            | winit::keyboard::Key::Named(winit::keyboard::NamedKey::Backspace) => {
                self.close_play_detail();
                true
            }
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter) => {
                if detail.can_watch() {
                    self.watch_detail_replay();
                    true
                } else {
                    true // Enter не проваливается в запуск карты под оверлеем
                }
            }
            _ => false,
        }
    }

    /// Клавиши лидерборда (см. [`leaderboard_action`]). Активны только когда
    /// список действительно показан: онлайн-режим + вход + записи.
    fn leaderboard_key(&mut self, key: &winit::keyboard::Key) -> bool {
        if !self.lb_online || !self.online.logged_in() || self.lb_records.is_empty() {
            return false;
        }
        let count = self.lb_records.len();
        let Some(action) = leaderboard_action(key, self.lb_focus, count) else {
            return false;
        };
        match action {
            LeaderboardKey::ToggleFocus => self.lb_focus = !self.lb_focus,
            LeaderboardKey::Up => self.lb_sel = self.lb_sel.saturating_sub(1),
            LeaderboardKey::Down => self.lb_sel = (self.lb_sel + 1).min(count - 1),
            LeaderboardKey::Open => {
                let sel = self.lb_sel.min(count - 1);
                self.open_play_detail(sel);
            }
            LeaderboardKey::Back => self.lb_focus = false,
        }
        true
    }

    /// Фоновое обновление кэша звёзд (публичный `/api/packs`, без токена).
    /// Повторный запрос не ставится, пока не пришёл предыдущий.
    fn request_stars(&mut self) {
        if self.stars_rx.is_some() {
            return;
        }
        let server = self.online.state.server.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.stars_rx = Some(rx);
        std::thread::spawn(move || {
            if let Ok(map) = crate::stars::fetch(&server) {
                let _ = tx.send(map);
            }
        });
    }

    /// Принять новый кэш звёзд (вызывается каждый кадр, как lb_rx). Ошибки
    /// сети молчат: остаётся прежний кэш.
    fn poll_stars(&mut self) {
        let Some(rx) = &self.stars_rx else { return };
        match rx.try_recv() {
            Ok(map) => {
                self.stars = map;
                crate::stars::save(&self.stars);
                self.stars_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.stars_rx = None,
        }
    }


    /// Превью песни в меню (отдельный ~20-сек клип <id>_pv из ассетов).
    fn play_preview(&mut self, song: usize) {
        let s = &self.songs[song];
        // порядок: свой превью-клип → кэш data/preview/<id>.ogg → сам трек.
        // Импортированные паки часто без preview (preview: null) — без
        // последнего варианта превью просто молчало.
        let own = s.preview.clone();
        let cached = format!("data/preview/{}.ogg", s.id);
        let full = s.audio.clone();
        let mut chosen: Option<(String, bool)> = None;
        for cand in [own, Some(cached), full].into_iter().flatten() {
            if std::path::Path::new(&cand).exists() {
                let is_full = s.audio.as_deref() == Some(cand.as_str());
                chosen = Some((cand, is_full));
                break;
            }
        }
        let Some((path, is_full)) = chosen else { return };
        if let Some(g) = self.gfx.as_mut() {
            if g.music.load(&path).is_ok() {
                self.music_gain = 0.8;
                g.music.set_volume(self.volume * self.music_gain);
                // у полного трека пропускаем вступление, чтобы превью было слышно
                let start = if is_full { (s.duration * 0.25).clamp(0.0, 30.0) } else { 0.0 };
                g.music.seek(start);
                g.music.play();
            }
        }
    }

    /// Загрузить в фон меню ноты выбранного чарта. Поток читает JSON, UI не
    /// ждёт; ошибки молчат — тогда остаётся случайный мотив.
    ///
    /// Смена песни/сложности НЕ чистит уже падающее поле: сразу гасим эмиссию
    /// из старого источника, а когда придёт новый — ставим его курсор на
    /// текущую позицию превью. Старые ноты продолжают падать сами по себе.
    fn ensure_bg_source(&mut self, selected: Option<Entry>) {
        let Some(e) = selected else {
            // ничего не выбрано — новых нот не будет, но поле доедет само
            if self.bg_key.take().is_some() {
                self.bg_field.clear_source();
            }
            return;
        };
        let Some(song) = self.songs.get(e.song) else { return };
        let Some(chart) = song.charts.get(e.diff) else { return };
        let key = (song.id.clone(), chart.diff.clone());
        if self.bg_key.as_ref() == Some(&key) {
            return;
        }
        // сразу помечаем ключ: даже пока чарт грузится, повторно не спавним
        self.bg_key = Some(key.clone());
        // старый источник молчит; живые ноты не трогаем
        self.bg_field.clear_source();
        let path = chart.path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.bg_rx = Some(rx);
        std::thread::spawn(move || {
            let src = Chart::load(&path)
                .ok()
                .map(|c| crate::bg::BgSource::from_chart(&c));
            let _ = tx.send((key, src));
        });
    }

    /// Принять результат фоновой загрузки нот (как auth_rx/lb_rx).
    fn poll_bg_source(&mut self) {
        let Some(rx) = &self.bg_rx else { return };
        match rx.try_recv() {
            Ok((key, src)) => {
                // пока грузили, могли выбрать другую песню — старый ответ мимо
                if self.bg_key.as_ref() == Some(&key) {
                    if let Some(src) = src {
                        let now = self.bg_time().unwrap_or(0.0);
                        self.bg_field.set_source(src, now);
                    }
                }
                self.bg_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.bg_rx = None,
        }
    }

    /// Время фона: фикс демо-шота или позиция реально играющего превью.
    fn bg_time(&self) -> Option<f32> {
        if let Some(t) = self.bg_t_demo {
            return Some(t);
        }
        let g = self.gfx.as_ref()?;
        if !g.music.is_playing() {
            return None;
        }
        let t = g.music.time();
        (t.is_finite()).then(|| t.max(0.0))
    }

    /// Двигать падающее поле каждый кадр. Часы фона идут по стенным дельтам,
    /// а не по скачкам позиции превью: перезапуск трека при смене песни не
    /// должен телепортировать уже летящие ноты.
    fn tick_bg(&mut self) {
        let Some(now) = self.bg_time() else {
            self.bg_last = None;
            return;
        };
        let inst = Instant::now();
        let dt = self
            .bg_last
            .map(|last| inst.duration_since(last).as_secs_f32())
            .unwrap_or(0.0);
        self.bg_last = Some(inst);
        self.bg_field.advance(dt.clamp(0.0, 0.1), now);
    }

    /// Время для рисования поля: только пока идёт превью/демо и есть живые
    /// ноты. Иначе экран рисует случайный мотив.
    fn bg_view_time(&self) -> Option<f32> {
        self.bg_time()?;
        (!self.bg_field.is_empty()).then(|| self.bg_field.clock())
    }

    /// Перо эмулирует системную мышь через libinput/OTD. Такое событие мыши
    /// надо игнорировать, иначе в игре получается двойной удар (hidraw +
    /// эмулированный клик). udev-правило намеренно не ставим: оно сломало бы
    /// планшет для всей системы.
    fn pen_emulated_mouse_at(&self, x: f32) -> bool {
        let Some(tablets) = self.tablets.as_ref() else {
            return false;
        };
        let (w, _) = self.size();
        crate::tablet::pen_emulates_mouse(
            tablets.pen_pressed_within(crate::tablet::PEN_PRESS_GRACE),
            tablets.pen_down(),
            tablets.pen_report_within(crate::tablet::PEN_REPORT_GRACE),
            x,
            w as f32,
        )
    }

    /// Движение системного курсора от пера: глушим при любом свежем отчёте,
    /// даже hover (перо — игровое устройство, а не вторая мышь). Клики
    /// по-прежнему фильтруются press-состоянием в [`Self::pen_emulated_mouse_at`].
    fn pen_emulated_motion_at(&self, x: f32) -> bool {
        let Some(tablets) = self.tablets.as_ref() else {
            return false;
        };
        let (w, _) = self.size();
        crate::tablet::pen_emulates_motion(
            tablets.pen_report_within(crate::tablet::PEN_REPORT_GRACE),
            x,
            w as f32,
        )
    }

    /// Живая X пера на судейской линии (мировые единицы), если планшет
    /// присутствует — даже при hover. None — пера нет.
    fn pen_x(&self) -> Option<f32> {
        let p = self.tablets.as_ref()?.active_state()?;
        p.present
            .then(|| (p.x_norm * 2.0 - 1.0) * crate::stage::note_x(2.0))
    }

    /// Правая кнопка в RMB-режиме: судит **правый** курсор (по его X и
    /// устройству); удержание ведёт правый слайд. Собственный press пера
    /// по-прежнему игнорируется.
    fn on_right_button(&mut self, pressed: bool) {
        let was = self.right_held;
        self.right_held = pressed;
        if !self.rmb_pen_press || self.settings.is_some() {
            return;
        }
        if !pressed || was {
            return;
        }
        let t = match &self.mode {
            Mode::Play { clock, paused: false, driver: None, .. } => {
                Some(clock.wall_to_time(Instant::now()))
            }
            _ => None,
        };
        let Some(t) = t else { return };
        let (_, aim) = cursor_sides(self.mouse_x, self.pen_x());
        let (input_mouse, input_tablet) = self.input_offsets();
        let offset = if aim.tablet_offset { input_tablet } else { input_mouse };
        let tj = t + offset / 1000.0;
        let mut judge = None;
        let mut err = 0.0;
        if let Mode::Play { chart, play, paused: false, driver: None, .. } = &mut self.mode {
            judge = apply_cursor_tap(play, chart, tj, aim, &mut self.hands);
            if matches!(judge, Some(Judge::Perfect) | Some(Judge::Good)) {
                err = play.errors.last().copied().unwrap_or(0.0) * 1000.0;
                match aim.device {
                    PressDevice::Pen => self.tablet_tap_errors.push(err),
                    PressDevice::Mouse => self.mouse_tap_errors.push(err),
                }
            }
        }
        if let Some(r) = &mut self.replay {
            let src = if aim.device == PressDevice::Pen { 1 } else { 0 };
            r.action(tj, src, true, aim.x, judge, err);
        }
    }

    fn to_library(&mut self) {
        if let Some(g) = self.gfx.as_ref() {
            g.music.pause();
        }
        // выход из просмотра реплея возвращает фокус на лидерборд: игрок
        // сразу может выбрать следующий, без мыши
        if matches!(&self.mode, Mode::Play { driver: Some(_), .. }) && !self.lb_records.is_empty() {
            self.lb_focus = true;
        }
        self.mode = Mode::Library {
            scroll: self.saved_scroll,
            selected: self.saved_selected,
        };
        self.replay = None;
        self.set_play_cursor(false);
    }

    /// Пере-читать библиотеку (базовая + импортированные папки), сохранив выбор.
    fn reload_library(&mut self) {
        let prev_sel = match &self.mode {
            Mode::Library { selected, .. } => *selected,
            _ => None,
        };
        if let Ok(songs) = crate::library::load() {
            self.songs = songs;
            if let Mode::Library { selected, .. } = &mut self.mode {
                *selected = prev_sel.filter(|e| {
                    self.songs
                        .get(e.song)
                        .map(|s| e.diff < s.charts.len())
                        .unwrap_or(false)
                });
            }
            // сброс кэша полноразмерных артов, т.к. набор песен изменился
            self.full_dims.clear();
            // новые паки (инбокс, F5, установка на лету) — догрузить их
            // миниатюры без перезапуска игры
            self.spawn_cover_preloader();
            // каталог сервера мог измениться вместе с паками — звёзды в фон
            self.request_stars();
            // ноты фона тоже могли переехать — перечитаем при следующем кадре
            self.bg_key = None;
            self.bg_field.clear_source();
            self.imported_sig = imported_sig();
        }
    }

    /// Установить пак, брошенный в окно (drag & drop `.deemopack`/папки пака).
    ///
    /// На Wayland `winit` не присылает `DroppedFile` вообще, поэтому основной
    /// путь там — инбокс `data/inbox` и диалог по F7. Здесь общий код установки.
    /// События Wayland-приёма файлов (окно winit их не получает само).
    fn poll_wayland_dnd(&mut self) {
        let events = match self.wayland_dnd.as_ref() {
            Some(d) => d.poll(),
            None => return,
        };
        for ev in events {
            match ev {
                guth_wayland_dnd::DndEvent::Hovered { .. } => {
                    self.toast = Some((
                        "Drop .deemopack or a pack folder to install".into(),
                        Instant::now(),
                    ));
                }
                guth_wayland_dnd::DndEvent::Dropped { paths, completion_id, .. } => {
                    for path in paths {
                        self.on_drop_file(path);
                    }
                    if let Some(id) = completion_id {
                        if let Some(d) = self.wayland_dnd.as_ref() {
                            d.complete_drop(id, true);
                        }
                    }
                }
                guth_wayland_dnd::DndEvent::DropFailed { reason } => {
                    self.toast = Some((format!("Install failed: {reason}"), Instant::now()));
                }
                guth_wayland_dnd::DndEvent::WorkerUnavailable { reason } => {
                    eprintln!("wayland dnd: worker stopped: {reason}");
                }
                _ => {}
            }
        }
    }

    fn on_drop_file(&mut self, path: std::path::PathBuf) {
        self.install_pack_path(&path);
    }

    /// Поставить пак из готового пути (drag & drop, диалог, инбокс).
    fn install_pack_path(&mut self, path: &std::path::Path) {
        match crate::pack_import::install_dropped(path, std::path::Path::new(crate::pack::IMPORT_DIR)) {
            Ok(title) => {
                self.toast = Some((format!("Installed {title}"), Instant::now()));
                // В библиотеке перечитываем список сразу — песня видна в этом
                // же кадре. В игре/на результате songs не трогаем: индексы
                // Mode::Play/Result указывают в него; авто-скан подхватит папку,
                // когда игрок вернётся в библиотеку.
                if matches!(self.mode, Mode::Library { .. }) {
                    self.reload_library();
                }
            }
            Err(msg) => self.toast = Some((msg, Instant::now())),
        }
    }

    /// Открыть родной диалог выбора пака в фоновом потоке (F7/строка настроек).
    /// UI продолжает рисоваться, пока диалог открыт.
    fn start_pick_pack(&mut self) {
        if self.pick_pending {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.pick_rx = Some(rx);
        self.pick_pending = true;
        std::thread::spawn(move || {
            if let Some(path) = crate::pack_import::pick_pack() {
                let _ = tx.send(path);
            }
        });
    }

    /// Принять выбор из диалога (вызывается каждый кадр, не блокирует).
    fn poll_pick(&mut self) {
        let Some(rx) = &self.pick_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(path) => {
                self.pick_rx = None;
                self.pick_pending = false;
                self.install_pack_path(&path);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                // отмена или диалог не запустился
                self.pick_rx = None;
                self.pick_pending = false;
            }
        }
    }

    /// Принять события инбокса data/inbox (вызывается каждый кадр).
    fn poll_inbox(&mut self) {
        let Some(rx) = &self.inbox_rx else {
            return;
        };
        let mut events = Vec::new();
        let mut closed = false;
        loop {
            match rx.try_recv() {
                Ok(ev) => events.push(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    closed = true;
                    break;
                }
            }
        }
        if closed {
            self.inbox_rx = None;
        }
        for ev in events {
            match &ev {
                crate::pack_import::InboxEvent::Installed { .. } => {
                    self.toast = Some((ev.toast(), Instant::now()));
                    if matches!(self.mode, Mode::Library { .. }) {
                        self.reload_library();
                    }
                }
                crate::pack_import::InboxEvent::Failed { .. } => {
                    self.toast = Some((ev.toast(), Instant::now()));
                }
            }
        }
    }

    /// Геометрия меню (общая для layout и draw_library): ранкинг слева,
    /// список песен в центре, целый арт песни справа, нижняя полоса.
    fn menu_geom(&self, selected: Option<usize>) -> screens::LibGeom {        let (w, h) = self.size();
        let aspect = selected
            .and_then(|si| self.full_dims.get(&si))
            .map(|&(w, h)| w as f32 / h.max(1) as f32);
        screens::lib_geom(w as f32, h as f32, aspect)
    }

    /// Геометрия меню: левый ранкинг, список песен, sort/group, кнопки сложности.
    fn layout(&mut self) {
        use crate::library::{GroupBy, SortBy};
        let (w, h) = self.size();
        let wf = w as f32;
        let hf = h as f32;
        // кнопки панели паузы (рисуются поверх игры)
        self.pause_rects.clear();
        if matches!(self.mode, Mode::Play { paused: true, .. }) {
            self.pause_rects = screens::pause_layout(wf, hf).buttons;
        }
        let (scroll, selected) = match &self.mode {
            Mode::Library { scroll, selected } => (*scroll, *selected),
            _ => return,
        };
        // фон: настоящие ноты выбранного чарта (перезагрузка при смене строки)
        self.ensure_bg_source(selected);
        let geom = self.menu_geom(selected.map(|e| e.song));

        // сортировка + группировка по строкам-чартам (osu!-стиль)
        let groups = crate::library::sort_group(
            &self.songs,
            self.sort_by,
            self.group_by,
            &self.stars,
            |e| {
                let Some(song) = self.songs.get(e.song) else { return 0 };
                let diff = song.chart_diff(e.diff);
                self.results.best(&song.id, diff).map(|r| r.score).unwrap_or(0)
            },
        );
        // фильтр: поиск по песне + (если появится диапазон) звёзды по строке
        self.view_groups = crate::library::filter_entries(groups, &self.songs, &self.search, &self.stars, None);

        let view_h = (hf - geom.top_h - geom.bot_h).max(1.0);
        let (_, _, total_h) = screens::library_rows(&self.view_groups, &geom, 0.0);
        self.total_h = total_h;

        // автопрокрутка к выбранной карточке — только при смене выбора
        let mut scroll = scroll;
        if selected != self.last_selected {
            self.last_selected = selected;
            if let Some(e) = selected {
                let (cards, _, _) = screens::library_rows(&self.view_groups, &geom, 0.0);
                if let Some(&(_, r)) = cards.iter().find(|(entry, _)| *entry == e) {
                    let view_top = geom.top_h + 8.0 * geom.s;
                    let view_bot = hf - geom.bot_h - 8.0 * geom.s;
                    if r[1] < view_top || r[1] + geom.row_h > view_bot {
                        let target = (view_top + view_bot - geom.row_h) * 0.5;
                        scroll = (r[1] - target).clamp(0.0, (total_h - view_h).max(0.0));
                        if let Mode::Library { scroll: s, .. } = &mut self.mode {
                            *s = scroll;
                        }
                    }
                }
            }
        }
        let (cards, headers, total_h) = screens::library_rows(&self.view_groups, &geom, scroll);
        self.card_rects = cards;
        self.lib_headers = headers;
        self.total_h = total_h;
        let sb = screens::scrollbar(&geom, view_h, total_h, scroll);
        self.sbar_track = sb.map(|(t, _)| t);
        self.sbar_thumb = sb.map(|(_, th)| th);

        // общая хром-геометрия (кнопки, поиск, аккаунт)
        let logged_in = self.online.logged_in();
        let chrome = screens::lib_chrome(wf, hf, &geom, selected.is_some(), logged_in);
        self.settings_btn = chrome.settings_btn;
        self.sort_btn = chrome.sort_btn;
        self.group_btn = chrome.group_btn;
        self.search_rect = chrome.search_rect;
        self.play_btn = chrome.play_btn;
        self.login_btn = chrome.login_btn;
        self.lb_toggle_rect = chrome.lb_toggle_rect;

        // dropdown
        self.dropdown_rects.clear();
        if let Some(dd) = self.open_dropdown {
            let btn = if dd == 0 { self.sort_btn } else { self.group_btn };
            let labels: Vec<&str> = if dd == 0 {
                SortBy::ALL.iter().map(|s| s.label()).collect()
            } else {
                GroupBy::ALL.iter().map(|g| g.label()).collect()
            };
            for (i, _) in labels.iter().enumerate() {
                self.dropdown_rects.push((i as u8, [btn[0], btn[1] + btn[3] + 4.0 * geom.s + i as f32 * 34.0 * geom.s, btn[2], 34.0 * geom.s]));
            }
        }

        // строки лидерборда: та же геометрия, что и у отрисовки
        self.lb_rows.clear();
        let leaders = self.lb_online && logged_in && selected.is_some();
        if leaders {
            self.lb_rows = screens::leaderboard_rows(&geom, hf - geom.bot_h, self.lb_records.len());
            self.lb_sel = self.lb_sel.min(self.lb_records.len().saturating_sub(1));
        } else {
            // список скрыт — фокус/детали не залипают
            self.lb_focus = false;
        }
        if selected.is_none() {
            // без выбранной карты секции лидерборда/реплеев нет вовсе
            self.rp_records.clear();
            self.rp_key.clear();
        }
        self.lib_geom = Some(geom);
    }

    /// Мировых единиц на пиксель экрана у судейской линии (для delta X мыши).
    fn mouse_scale(&self) -> f32 {
        let (w, h) = self.size();
        let empty = Chart::empty();
        let scene = Scene::new(&empty, self.speed);
        let mvp = scene.mvp(w as f32 / h as f32);
        let p0 = crate::math::project(&mvp, [0.0, 0.0, 0.0]);
        let p1 = crate::math::project(&mvp, [1.0, 0.0, 0.0]);
        let px = (p1[0] - p0[0]).abs() * (w as f32 * 0.5);
        if px > 1e-3 {
            1.0 / px
        } else {
            0.01
        }
    }

    /// В игре: скрыть и захватить курсор (относительный ввод). В меню: показать.
    /// В absolute-режиме без Hide cursor курсор видимый и не захвачен
    /// (клик прямо по колонке ноты). С Hide cursor системный курсор скрыт в
    /// обоих режимах: иначе в absolute он уезжает за пером (libinput), пока
    /// игра целится мышью, и «двоится».
    fn set_play_cursor(&mut self, play: bool) {
        let Some(g) = self.gfx.as_ref() else {
            return;
        };
        if play {
            if self.mouse_absolute {
                let _ = g.window.set_cursor_grab(winit::window::CursorGrabMode::None);
                self.cursor_grabbed = false;
            } else {
                let _ = g.window.set_cursor_grab(winit::window::CursorGrabMode::Locked);
                self.cursor_grabbed = true;
            }
            g.window
                .set_cursor_visible(!hide_system_cursor(true, self.line_cursor, self.mouse_absolute));
        } else {
            let _ = g.window.set_cursor_grab(winit::window::CursorGrabMode::None);
            g.window.set_cursor_visible(true);
            self.cursor_grabbed = false;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Deemo")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("window"));
        // file association for .deemopack — background, silent, right after the
        // window exists; never blocks the first frame or the GPU setup
        std::thread::spawn(|| match crate::assoc::register() {
            Ok(msg) => crate::assoc::log(&msg),
            Err(e) => crate::assoc::log(&format!("assoc: skipped ({e})")),
        });
        if self.fullscreen {
            window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
        }
        let gpu = Gpu::new(None).expect("gpu");
        let surface = gpu.instance.create_surface(window.clone()).expect("surface");
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoNoVsync,
            color_space: wgpu::SurfaceColorSpace::Auto,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &config);
        let mut renderer = Renderer::new(gpu.device, gpu.queue, format).expect("renderer");
        let text = Text::new(&mut renderer).expect("font");
        self.text = Some(text);
        self.white_ui = renderer.add_ui_texture("white-ui", 1, 1, &[255, 255, 255, 255]);
        let (fx, fy) = crate::theme::make_fades(&mut renderer);
        self.fade_x_ui = fx;
        self.fade_y_ui = fy;
        let music = MusicPlayer::new().expect("audio");
        renderer.set_ui_size(config.width, config.height);
        self.fps_last = Instant::now();
        self.frames = 0;
        window.request_redraw();
        // Wayland: подключить приём файлов самим (winit 0.30 этого не умеет).
        // На X11/Windows attach вернёт None — там работают события winit.
        self.wayland_dnd = guth_wayland_dnd::FileDnd::attach(window.as_ref(), || {});
        if self.wayland_dnd.is_some() {
            println!("wayland dnd: attached (file drops enabled)");
        }
        self.gfx = Some(Gfx {
            _instance: gpu.instance,
            _adapter: gpu.adapter,
            window,
            surface,
            config,
            renderer,
            music,
        });
        if let Some(path) = self.startup_pack.take() {
            self.install_pack_path(&path);
        }
        if let Some((s, d)) = self.pending_play.take() {
            self.start_play(s, d);
        }
        self.spawn_cover_preloader();
        self.tablets = Some(crate::tablet::Tablets::new(
            self.tablet_zoom,
            self.tablet_press_threshold,
        ));
        // инбокс data/inbox: на Wayland drag & drop нет вовсе, поэтому паки
        // можно просто класть в папку — фоновый поток поставит их сам
        let inbox = std::path::Path::new(crate::pack_import::INBOX_DIR);
        if let Err(e) = crate::pack_import::ensure_inbox(inbox) {
            eprintln!("inbox: cannot prepare {}: {e:#}", inbox.display());
        } else {
            println!("{}", crate::pack_import::INBOX_READY_LINE);
        }
        self.inbox_rx = Some(crate::pack_import::spawn_inbox_watcher(
            std::path::PathBuf::from(crate::pack::IMPORT_DIR),
        ));
        // старт: добить рекорды, оставшиеся с прошлого запуска (сеть — в фоне)
        let server = self.online.state.server.clone();
        self.queue.flush(&server);
        self.last_queue_flush = Instant::now();
        // звёзды сложностей: в фоне, кадр не ждёт (при ошибке остаётся кэш)
        self.request_stars();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.save_settings();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(g) = self.gfx.as_mut() {
                    g.config.width = size.width.max(1);
                    g.config.height = size.height.max(1);
                    g.surface.configure(&g.renderer.device, &g.config);
                    g.renderer.set_ui_size(g.config.width, g.config.height);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Сырая позиция нужна фильтру pen-emulated кликов: перо
                // двигает системный курсор в своей координате.
                self.system_cursor = [position.x as f32, position.y as f32];
                // Перо двигает системный курсор в своей координате — такое
                // движение игнорируем. Мышь в любой другой точке проходит:
                // у пера и мыши два независимых курсора.
                if !self.pen_emulated_motion_at(position.x as f32) {
                    self.cursor = [position.x as f32, position.y as f32];
                }
                if self.sbar_drag.is_some() && self.left_held {
                    self.sbar_drag_to(self.cursor[1]);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.settings.is_some() || self.lb_detail.is_some() {
                    return;
                }
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 60.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                } * self.scroll_speed;
                let (w, h) = self.size();
                let scale = crate::theme::ui_scale(w as f32, h as f32);
                let total_h = self.total_h;
                if let Mode::Library { scroll, .. } = &mut self.mode {
                    let view_h = (h as f32 - screens::TOP_H * scale - screens::BOT_H * scale).max(1.0);
                    let max = (total_h - view_h).max(0.0);
                    *scroll = (*scroll - dy).clamp(0.0, max);
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.key_mods = m.state();
            }
            WindowEvent::Focused(true) => {
                // после alt-tab/старта окно могло вернуть системный курсор:
                // в игре с Hide cursor его быть не должно
                if self.settings.is_none() && matches!(self.mode, Mode::Play { .. }) {
                    self.set_play_cursor(true);
                }
            }
            WindowEvent::HoveredFile(_) => {
                self.toast = Some(("Drop .deemopack or a pack folder to install".into(), Instant::now()));
            }
            WindowEvent::HoveredFileCancelled => {}
            WindowEvent::DroppedFile(path) => self.on_drop_file(path),
            WindowEvent::MouseInput { state, button, .. } => {
                // Перо дублирует мышь через libinput/OTD. Свежий нажим пера,
                // нажатое перо или курсор рядом с пером — значит клик пришёл
                // от пера: не трогаем кнопки, on_click и реплей.
                if self.pen_emulated_mouse_at(self.system_cursor[0]) {
                    return;
                }
                match button {
                    MouseButton::Left => {
                        self.left_held = state == ElementState::Pressed;
                        let t = match &self.mode {
                            Mode::Play { clock, .. } => {
                                Some(clock.wall_to_time(Instant::now()))
                            }
                            _ => None,
                        };
                        if self.settings.is_none() {
                            if let (Some(t), Some(r)) = (t, &mut self.replay) {
                                r.mouse_button(t, self.left_held, self.mouse_x);
                            }
                        }
                        if self.left_held {
                            // скроллбар ловит мышь первым: тянуть ползунок важнее,
                            // чем выбрать песню под ним
                            if !self.sbar_press() {
                                self.on_click();
                            }
                        } else {
                            self.sbar_drag = None;
                        }
                    }
                    MouseButton::Right => {
                        self.on_right_button(state == ElementState::Pressed);
                    }
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    self.on_key(event.logical_key, event_loop);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        if let winit::event::DeviceEvent::MouseMotion { delta } = event {
            if self.cursor_grabbed && !self.mouse_absolute {
                let half = 10.24; // половина ширины судейской линии
                let dx = delta.0 as f32 * self.mouse_scale();
                self.mouse_x = (self.mouse_x + dx).clamp(-half, half);
                // сырой ввод мыши в реплей (время карты)
                let t = match &self.mode {
                    Mode::Play { clock, .. } => Some(clock.wall_to_time(Instant::now())),
                    _ => None,
                };
                if let (Some(t), Some(r)) = (t, &mut self.replay) {
                    r.mouse_motion(t, dx, self.mouse_x);
                }
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.gfx.is_none() {
            return;
        }
        // кап ~120 fps: ровный пейсинг без flooding композитора
        let now = Instant::now();
        if now < self.next_frame {
            std::thread::sleep(self.next_frame - now);
        }
        self.next_frame = Instant::now() + std::time::Duration::from_secs_f64(1.0 / 120.0);
        self.drain_covers();
        // онлайн-лидерборд + реплеи: один ответ на карту
        if let Some(rx) = &self.lb_rx {
            match rx.try_recv() {
                Ok((key, recs, replays)) => {
                    // пока грузили, могли выбрать другую карту — старый ответ мимо
                    let same_chart = key
                        .rsplit_once(':')
                        .map(|(chart, _)| Some(chart) == self.chart_key().as_deref())
                        .unwrap_or(false);
                    if same_chart {
                        self.lb_key = key;
                        self.lb_records = recs;
                        self.rp_key = self.chart_key().unwrap_or_default();
                        self.rp_records = replays;
                        self.lb_sel = self.lb_sel.min(self.lb_records.len().saturating_sub(1));
                        if self.lb_records.is_empty() {
                            self.lb_focus = false;
                        }
                    }
                    self.lb_pending = None;
                    self.lb_rx = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    // поток умер, не прислав ответ: не блокируем следующие запросы
                    self.lb_pending = None;
                    self.lb_rx = None;
                }
            }
        }
        self.poll_replay_blob();
        // кэш звёзд сложностей (публичный каталог, фоновый поток)
        self.poll_stars();
        // вход/регистрация из экрана настроек (фон)
        self.poll_auth();
        // фон меню: ноты выбранного чарта (фоновый поток)
        self.poll_bg_source();
        // очередь рекордов: события фонового потока (тосты) + редкий ретрай
        self.poll_online_queue();
        if self.last_queue_flush.elapsed().as_secs() >= 45 {
            let server = self.online.state.server.clone();
            if self.queue.has_due(&server) {
                self.queue.flush(&server);
            }
            self.last_queue_flush = Instant::now();
        }
        // скачивание всех паков: проверка каталога и прогресс (фон)
        self.poll_packs();
        // инбокс data/inbox и диалог выбора пака (оба — фоновые потоки)
        self.poll_inbox();
        self.poll_pick();
        self.poll_wayland_dnd();
        // авто-подхват новых папок в data/imported (раз в ~1.2с в библиотеке)
        if matches!(self.mode, Mode::Library { .. })
            && self.last_import_scan.elapsed().as_millis() > 1200
        {
            self.last_import_scan = Instant::now();
            let sig = imported_sig();
            if sig != self.imported_sig {
                self.reload_library();
                self.toast = Some(("Library updated".into(), Instant::now()));
            }
        }
        self.gfx.as_ref().unwrap().window.request_redraw();
    }
}

impl App {
    fn on_key(&mut self, key: Key, event_loop: &ActiveEventLoop) {
        // F7 — поставить пак из файла: работает отовсюду (библиотека, игра,
        // настройки) и не зависит от drag & drop, которого на Wayland нет.
        if matches!(key, Key::Named(NamedKey::F7)) {
            self.start_pick_pack();
            return;
        }
        // Пока открыт экран настроек, все клавиши идут только ему: глобальные
        // игровые хоткеи (пауза, скорость, поиск, F11...) не срабатывают,
        // даже когда в фокусе текстовое поле.
        if self.settings.is_some() {
            self.on_key_settings(key);
            return;
        }
        if matches!(self.mode, Mode::Login { .. }) {
            self.on_key_login(key, event_loop);
            return;
        }
        // Библиотека — экран с единственным текстовым полем (поиск песен),
        // поэтому печатный ввод обрабатывается РАНЬШЕ игровых хоткеев: иначе
        // Space уходит в паузу, «[»/«]» — в скорость прокрутки, а фильтр в
        // ветке `Key::Character` молча выбрасывает «!», «#», «(», «%» и прочую
        // пунктуацию («пробел/другие символы в поиске не печатаются»).
        if matches!(self.mode, Mode::Library { .. }) {
            // открытые детали плея — модальный экран: остальные клавиши не
            // должны менять поиск/выбор под ним
            if self.lb_detail.is_some() {
                let _ = self.detail_key(&key);
                return;
            }
            // фокус на лидерборде: Tab входит/выходит, Up/Down, Enter
            if self.leaderboard_key(&key) {
                return;
            }
            // быстрая навигация: Home/End/PageUp/PageDown
            if self.list_key_nav(&key) {
                return;
            }
            // Up/Down — по строкам-чартам (osu!-стиль). Ctrl+Up/Down остаётся
            // прежним хоткеем скорости.
            if self.list_key_move(&key) {
                return;
            }
            if library_search_key(&mut self.search, &key, true) {
                // печатный ввод — снова поиск: фокус лидерборда не должен
                // перехватывать следующие пробелы
                self.lb_focus = false;
                if let Mode::Library { scroll, .. } = &mut self.mode {
                    *scroll = 0.0;
                }
                return;
            }
        }
        match key {
            Key::Named(NamedKey::Escape) => match &self.mode {
                Mode::Library { selected, .. } => {
                    if self.open_dropdown.is_some() {
                        self.open_dropdown = None;
                    } else if !self.search.is_empty() {
                        self.search.clear();
                    } else if selected.is_some() {
                        self.mode = Mode::Library {
                            scroll: self.scroll(),
                            selected: None,
                        };
                    } else {
                        event_loop.exit();
                    }
                }
                Mode::Play { .. } => self.to_library(),
                Mode::Result { .. } => self.to_library(),
                Mode::Login { .. } => {}
            },
            Key::Named(NamedKey::Backspace) => {
                if matches!(self.mode, Mode::Library { .. }) {
                    self.search.pop();
                } else if matches!(self.mode, Mode::Play { driver: Some(_), .. }) {
                    // «назад» из просмотра реплея — как Esc, обратно к списку
                    self.to_library();
                }
            }
            Key::Named(NamedKey::Space) => self.toggle_pause(),
            // F1 — настройки (главное меню/библиотека)
            Key::Named(NamedKey::F1) => {
                if matches!(self.mode, Mode::Library { .. }) {
                    self.open_settings();
                }
            }
            // S на паузе — открыть настройки (игра при этом остаётся на паузе)
            Key::Character(c)
                if c.as_str() == "s" && matches!(self.mode, Mode::Play { paused: true, .. }) =>
            {
                self.open_settings();
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.speed = (self.speed + 0.5).min(9.5);
                self.save_settings();
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.speed = (self.speed - 0.5).max(0.5);
                self.save_settings();
            }
            // скорость пролистывания меню: Ctrl+[ / Ctrl+] (в библиотеке
            // обычные [ и ] печатаются в поиск; точное значение — в F1 →
            // «скорость прокрутки»)
            Key::Character(c) if c.as_str() == "[" && self.key_mods.control_key() => {
                self.scroll_speed = (self.scroll_speed - 0.25).max(0.25);
                self.save_settings();
            }
            Key::Character(c) if c.as_str() == "]" && self.key_mods.control_key() => {
                self.scroll_speed = (self.scroll_speed + 0.25).min(5.0);
                self.save_settings();
            }
            Key::Named(NamedKey::Enter) => {
                match &self.mode {
                    Mode::Result { .. } => self.to_library(),
                    Mode::Library { selected: Some(e), .. } => {
                        let (si, diff) = (e.song, e.diff);
                        self.start_play(si, diff);
                    }
                    _ => {}
                }
            }
            Key::Named(NamedKey::F11) => self.toggle_fullscreen(),
            // F5 — пере-сканировать data/imported (подхватить новые папки)
            Key::Named(NamedKey::F5) => {
                self.reload_library();
                self.toast = Some((format!("Library updated ({} songs)", self.songs.len()), Instant::now()));
            }
            // F2 — сохранить реплей (сырой ввод) в файл
            Key::Named(NamedKey::F2) => {
                let play_ref = match &self.mode {
                    Mode::Play { play, .. } | Mode::Result { play, .. } => Some(play),
                    _ => None,
                };
                if let (Some(r), Some(play)) = (&self.replay, play_ref) {
                    match r.save(play) {
                        Ok(p) => {
                            let msg = format!("Replay saved: {}", p.display());
                            println!("{msg}");
                            self.toast = Some((msg, Instant::now()));
                        }
                        Err(e) => {
                            eprintln!("replay save failed: {e}");
                            self.toast = Some((format!("Replay error: {e}"), Instant::now()));
                        }
                    }
                }
            }
            // F3 — применить локальные оффсеты карты (на результате): пишутся в
            // data/offsets.json и действуют на следующих плеях этой карты
            Key::Named(NamedKey::F3) => {
                if matches!(self.mode, Mode::Result { .. }) {
                    self.apply_map_offsets();
                }
            }
            // G — переключить направляющие линии от нот (вне библиотеки)
            Key::Character(c)
                if c.as_str() == "g" && !matches!(self.mode, Mode::Library { .. }) =>
            {
                self.note_guides = !self.note_guides;
                self.save_settings();
            }
            // ввод поискового запроса в библиотеке обрабатывается выше, до
            // игровых хоткеев: здесь печатные символы уже не нужны, а фильтр
            // `is_alphanumeric` выбрасывал пунктуацию (см. push_search_text).
            _ => {}
        }
    }

    fn scroll(&self) -> f32 {
        match &self.mode {
            Mode::Library { scroll, .. } => *scroll,
            _ => 0.0,
        }
    }

    fn toggle_pause(&mut self) {
        let paused = matches!(self.mode, Mode::Play { paused: true, .. });
        self.set_paused(!paused);
    }

    fn set_paused(&mut self, pause: bool) {
        if let Mode::Play { paused, clock, music_on, .. } = &mut self.mode {
            if *paused == pause {
                return;
            }
            *paused = pause;
            if pause {
                clock.pause();
                if let Some(g) = self.gfx.as_ref() {
                    g.music.pause();
                }
            } else {
                clock.resume();
                if *music_on {
                    if let Some(g) = self.gfx.as_ref() {
                        g.music.play();
                    }
                }
            }
        }
    }

    /// Текущие настройки приложения одним значением.
    fn snapshot_settings(&self) -> Settings {
        let mut s = Settings {
            speed: self.speed,
            scroll_speed: self.scroll_speed,
            fullscreen: self.fullscreen,
            tablet_zoom: self.tablet_zoom,
            audio_offset_ms: self.audio_offset_ms,
            note_guides: self.note_guides,
            x_tolerance: self.x_tolerance,
            mouse_absolute: self.mouse_absolute,
            line_cursor: self.line_cursor,
            rmb_pen_press: self.rmb_pen_press,
            mouse_offset_ms: self.mouse_offset_ms,
            tablet_offset_ms: self.tablet_offset_ms,
            tablet_press_threshold: self.tablet_press_threshold,
            volume: self.volume,
        };
        s.sanitize();
        s
    }

    fn save_settings(&self) {
        crate::settings::save(&self.snapshot_settings());
    }

    /// Локальный оффсет текущей карты для устройства (мс), 0 если карты нет.
    fn map_local_ms(&self, device: usize) -> f32 {
        let (song, diff) = &self.play_key;
        self.map_offsets.local_ms(song, diff, device)
    }

    /// Оффсет ввода, реально применяемый в игре: глобальная настройка +
    /// локальная поправка карты. Возвращает (мышь, планшет), мс.
    fn input_offsets(&self) -> (f32, f32) {
        (
            self.mouse_offset_ms + self.map_local_ms(crate::map_offsets::MOUSE),
            self.tablet_offset_ms + self.map_local_ms(crate::map_offsets::TABLET),
        )
    }

    fn apply_music_volume(&self) {
        if let Some(g) = self.gfx.as_ref() {
            g.music.set_volume(self.volume * self.music_gain);
        }
    }

    /// Применить настройки из экрана настроек: поля, звук, окно, запись на диск.
    fn apply_settings(&mut self, s: &Settings) {
        let mut s = *s;
        s.sanitize();
        self.speed = s.speed;
        self.scroll_speed = s.scroll_speed;
        self.tablet_zoom = s.tablet_zoom;
        self.audio_offset_ms = s.audio_offset_ms;
        self.note_guides = s.note_guides;
        self.x_tolerance = s.x_tolerance;
        self.mouse_absolute = s.mouse_absolute;
        self.line_cursor = s.line_cursor;
        self.rmb_pen_press = s.rmb_pen_press;
        self.mouse_offset_ms = s.mouse_offset_ms;
        self.tablet_offset_ms = s.tablet_offset_ms;
        self.tablet_press_threshold = s.tablet_press_threshold;
        self.volume = s.volume;
        if self.fullscreen != s.fullscreen {
            self.set_fullscreen(s.fullscreen);
        }
        self.apply_music_volume();
        let (input_mouse, input_tablet) = self.input_offsets();
        if let Some(r) = &mut self.replay {
            r.set_input_offsets(input_mouse, input_tablet);
        }
        // на паузе X-допуск можно поправить и для текущей песни
        if let Mode::Play { play, .. } = &mut self.mode {
            play.set_x_tolerance(self.x_tolerance);
        }
        crate::settings::save(&s);
    }

    fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
        if let Some(g) = self.gfx.as_ref() {
            let fs = if on {
                Some(winit::window::Fullscreen::Borderless(None))
            } else {
                None
            };
            g.window.set_fullscreen(fs);
        }
        self.save_settings();
    }

    fn toggle_fullscreen(&mut self) {
        self.set_fullscreen(!self.fullscreen);
    }

    // ===== экран настроек =====

    fn open_settings(&mut self) {
        self.open_settings_with(true);
    }

    /// `check` — запускать ли фоновую проверку каталога паков (для headless
    /// скриншотов сеть не нужна).
    fn open_settings_with(&mut self, check: bool) {
        if self.settings.is_some() {
            return;
        }
        let snap = self.snapshot_settings();
        self.settings = Some(SettingsScreen::new(snap, &self.online));
        // настройки — оконный UI: системный курсор нужен, даже если игра на
        // паузе с Hide cursor/относительным вводом
        self.set_play_cursor(false);
        if check {
            self.start_packs_check();
        }
    }

    /// Закрыть экран настроек, сохранив URL сервера в auth.json.
    fn close_settings(&mut self) {
        let Some(sc) = self.settings.take() else {
            return;
        };
        let server = sc.server.text.trim().trim_end_matches('/').to_string();
        if !server.is_empty() && server != self.online.state.server {
            self.online.state.server = server;
            self.online.save();
        }
        // Hide cursor could have been toggled while the settings overlay was
        // open over a paused play: apply hiding/showing immediately.
        self.set_play_cursor(matches!(self.mode, Mode::Play { .. }));
    }

    fn on_key_settings(&mut self, key: Key) {
        let logged_in = self.online.logged_in();
        let ctrl = self.key_mods.control_key();
        // Пока идёт загрузка паков, Esc/Backspace отменяют её (первое нажатие),
        // если ни одно текстовое поле не в фокусе: второе Esc закроет экран.
        if self.packs.running() {
            let no_focus = self
                .settings
                .as_ref()
                .map(|sc| sc.focus.is_none())
                .unwrap_or(false);
            if no_focus
                && matches!(
                    key,
                    Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Backspace)
                )
            {
                self.cancel_packs();
                return;
            }
        }
        let (action, dirty, draft) = {
            let Some(sc) = self.settings.as_mut() else {
                return;
            };
            let action = sc.on_key(&key, logged_in, ctrl);
            (action, std::mem::take(&mut sc.dirty), sc.draft)
        };
        if dirty {
            self.apply_settings(&draft);
        }
        self.handle_settings_action(action);
    }

    fn on_click_settings(&mut self) {
        let [px, py] = self.cursor;
        let (action, dirty, draft) = {
            let Some(sc) = self.settings.as_mut() else {
                return;
            };
            let action = sc.on_click(px, py);
            (action, std::mem::take(&mut sc.dirty), sc.draft)
        };
        if dirty {
            self.apply_settings(&draft);
        }
        self.handle_settings_action(action);
    }

    fn handle_settings_action(&mut self, action: Option<SettingsAction>) {
        match action {
            Some(SettingsAction::Close) => self.close_settings(),
            Some(SettingsAction::Revert) => {
                let snap = {
                    let Some(sc) = self.settings.as_mut() else {
                        return;
                    };
                    sc.revert()
                };
                self.apply_settings(&snap);
            }
            Some(SettingsAction::Login) => self.start_auth(false),
            Some(SettingsAction::Register) => self.start_auth(true),
            Some(SettingsAction::Logout) => {
                self.online.logout();
                if let Some(sc) = &mut self.settings {
                    sc.status = None;
                    sc.password.clear();
                }
                self.toast = Some(("Signed out".into(), Instant::now()));
            }
            Some(SettingsAction::DownloadPacks) => {
                // повторное нажатие по строке во время загрузки = отмена
                if self.packs.running() {
                    self.cancel_packs();
                } else {
                    self.start_packs();
                }
            }
            Some(SettingsAction::InstallPack) => self.start_pick_pack(),
            None => {}
        }
    }

    // ===== «скачать все паки» =====

    /// Сервер для загрузки паков: введённый в настройках, иначе сохранённый,
    /// иначе дефолтный (общий с онлайном).
    fn pack_server(&self) -> String {
        let typed = self
            .settings
            .as_ref()
            .map(|sc| sc.server.text.trim().trim_end_matches('/').to_string())
            .unwrap_or_default();
        if !typed.is_empty() {
            return typed;
        }
        let saved = self.online.state.server.trim().trim_end_matches('/');
        if saved.is_empty() {
            crate::online::DEFAULT_SERVER.to_string()
        } else {
            saved.to_string()
        }
    }

    /// Фоновая проверка каталога: сколько всего паков и сколько уже стоит.
    /// В сеть уходит только фоновый поток — кадр не блокируется.
    fn start_packs_check(&mut self) {
        if self.packs.running() || self.packs_check_rx.is_some() {
            return;
        }
        let server = self.pack_server();
        let (tx, rx) = std::sync::mpsc::channel();
        self.packs_check_rx = Some(rx);
        self.packs.phase = PacksPhase::Checking;
        self.packs.note = None;
        std::thread::spawn(move || {
            let res = crate::pack_download::vanilla_catalog(&server).map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
    }

    /// Старт (или повтор) загрузки всех ванильных паков сервера.
    fn start_packs(&mut self) {
        if self.packs.running() {
            return;
        }
        let server = self.pack_server();
        // проверка каталога уже неактуальна — её ответ не должен перебить Running
        self.packs_check_rx = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        self.packs_rx = Some(rx);
        self.packs_cancel = Some(cancel.clone());
        self.packs.phase = PacksPhase::Running;
        self.packs.note = None;
        self.packs.cancelling = false;
        // счётчик из предыдущей проверки виден сразу, до ответа install_all
        let known = self.packs.total.unwrap_or(0);
        self.packs.prog = crate::pack_download::Progress {
            total: known,
            ..Default::default()
        };
        std::thread::spawn(move || {
            crate::pack_download::install_all(
                &server,
                std::path::Path::new(crate::pack::IMPORT_DIR),
                cancel,
                Some(tx),
            );
        });
    }

    /// Попросить фоновую загрузку остановиться (после текущего пака).
    fn cancel_packs(&mut self) {
        if let Some(c) = &self.packs_cancel {
            c.store(true, Ordering::Relaxed);
            self.packs.cancelling = true;
        }
    }

    /// Принять результаты фоновой проверки каталога и прогресс загрузки
    /// (вызывается каждый кадр, как poll_auth).
    fn poll_packs(&mut self) {
        if let Some(rx) = &self.packs_check_rx {
            match rx.try_recv() {
                Ok(Ok(catalog)) => {
                    self.packs_check_rx = None;
                    self.packs.total = Some(catalog.len());
                    self.packs.ids = catalog.iter().map(|e| e.id.clone()).collect();
                    self.packs.refresh_installed();
                    self.packs.note = None;
                    if !self.packs.running() {
                        self.packs.phase = PacksPhase::Idle;
                    }
                }
                Ok(Err(e)) => {
                    self.packs_check_rx = None;
                    self.packs.note = Some(e);
                    if !self.packs.running() {
                        self.packs.phase = PacksPhase::Idle;
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.packs_check_rx = None;
                    if !self.packs.running() {
                        self.packs.phase = PacksPhase::Idle;
                    }
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if self.packs_rx.is_none() {
            return;
        }
        let mut last: Option<crate::pack_download::Progress> = None;
        let mut closed = false;
        if let Some(rx) = &self.packs_rx {
            loop {
                match rx.try_recv() {
                    Ok(p) => last = Some(p),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        closed = true;
                        break;
                    }
                }
            }
        }
        let Some(p) = last else {
            // поток умер, не прислав финального прогресса — не оставляем
            // строку в вечном «running»
            if closed && self.packs.running() {
                self.packs_rx = None;
                self.packs_cancel = None;
                self.packs.phase = PacksPhase::Done;
            }
            return;
        };
        if p.finished {
            self.packs_rx = None;
            self.packs_cancel = None;
            self.packs.finish(p);
            self.packs.refresh_installed();
            // новые паки: звёзды могли появиться в каталоге — обновляем кэш
            self.request_stars();
            let msg = format!(
                "Packs: {} installed · {} failed",
                self.packs.prog.done + self.packs.prog.skipped,
                self.packs.prog.failed.len()
            );
            self.toast = Some((msg, Instant::now()));
        } else {
            self.packs.prog = p;
        }
    }

    /// Отправить вход/регистрацию в фоне (не блокируем кадры).
    fn start_auth(&mut self, register: bool) {
        if self.auth_pending {
            return;
        }
        let Some(sc) = self.settings.as_mut() else {
            return;
        };
        let server = sc.server.text.trim().trim_end_matches('/').to_string();
        let nick = sc.nickname_text().trim().to_string();
        let password = sc.password_text().to_string();
        if nick.is_empty() || password.is_empty() {
            sc.status = Some(("Enter nickname and password".into(), false));
            return;
        }
        let server = if server.is_empty() {
            crate::online::DEFAULT_SERVER.to_string()
        } else {
            server
        };
        self.online.state.server = server;
        self.online.save();
        let mut online = self.online.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.auth_rx = Some(rx);
        self.auth_pending = true;
        sc.status = Some((
            if register {
                "Creating account...".to_string()
            } else {
                "Signing in...".to_string()
            },
            true,
        ));
        std::thread::spawn(move || {
            let res = online.auth(&nick, &password, register);
            let _ = tx.send((res, online.state.clone()));
        });
    }

    /// Принять события очереди отправки рекордов (вызывается каждый кадр).
    fn poll_online_queue(&mut self) {
        while let Some(ev) = self.queue.poll() {
            // pp по результату, который сейчас на экране. Ответ мог прийти как
            // сразу после игры, так и фоновым проходом (рекорд был в очереди).
            let ack = self
                .result_rec
                .as_ref()
                .and_then(|want| ack_for_result(want, &ev.acks))
                .cloned();
            if let Some(ack) = &ack {
                self.result_pp = result_pp_from_ack(ack);
            }
            if ev.immediate {
                // событие на команду, отправленную сразу после результата
                let Some(pos) = self
                    .score_reports
                    .iter()
                    .position(|(seq, _)| *seq == ev.seq)
                else {
                    continue;
                };
                let (_, rec) = self.score_reports.remove(pos);
                // 2xx без поля pp (старый сервер): ответ пришёл, показывать
                // нечего — «—», а не вечный «PP PENDING».
                if ack.is_none()
                    && !self.queue.contains(&rec)
                    && self.result_rec.as_ref() == Some(&rec)
                {
                    self.result_pp = ResultPp::Hidden;
                }
                // Запись уже вычеркнута из очереди → 2xx; иначе она осталась.
                if ack.is_some() {
                    self.toast = Some((upload_toast(ack.as_ref()), Instant::now()));
                } else if let Some(msg) =
                    queue_toast(true, ev.uploaded, self.queue.contains(&rec))
                {
                    self.toast = Some((msg.to_string(), Instant::now()));
                }
            } else if ack.is_some() {
                // догрузка записи, которая ждала в очереди офлайн
                self.toast = Some((upload_toast(ack.as_ref()), Instant::now()));
            } else if let Some(msg) = queue_toast(false, ev.uploaded, false) {
                // фоновая отправка (старт игры или периодический ретрай)
                self.toast = Some((msg.to_string(), Instant::now()));
            }
        }
    }

    /// Принять результат фонового входа (вызывается каждый кадр).
    fn poll_auth(&mut self) {
        let Some(rx) = &self.auth_rx else {
            return;
        };
        let Ok((res, state)) = rx.try_recv() else {
            return;
        };
        self.auth_rx = None;
        self.auth_pending = false;
        match res {
            Ok(nick) => {
                self.online.state = state;
                self.online.save();
                if let Some(sc) = &mut self.settings {
                    sc.status = None;
                    sc.password.clear();
                }
                self.toast = Some((format!("Signed in as {nick}"), Instant::now()));
            }
            Err(e) => {
                if let Some(sc) = &mut self.settings {
                    sc.status = Some((format!("Sign-in failed: {e}"), false));
                }
                self.toast = Some((format!("Sign-in failed: {e}"), Instant::now()));
            }
        }
    }

    fn draw_settings(&mut self, ui: &mut Vec<UiQuad>) {
        let (w, h) = self.size();
        let white = self.white_ui;
        let Some(sc) = self.settings.as_mut() else {
            return;
        };
        let Some(text) = self.text.as_ref() else {
            return;
        };
        sc.draw(text, ui, white, w as f32, h as f32, &self.online, &self.packs);
    }

    /// Высота области списка (без верхней панели и нижней плашки).
    fn list_view_h(&self) -> f32 {
        let (w, h) = self.size();
        let s = crate::theme::ui_scale(w as f32, h as f32);
        (h as f32 - crate::screens::TOP_H * s - crate::screens::BOT_H * s).max(1.0)
    }

    /// Попадание в скроллбар: `Some(внутри_ползунка)`. Зона захвата шире
    /// видимой полоски (2 px), иначе в неё невозможно попасть мышью.
    fn sbar_hit(&self, x: f32, y: f32) -> Option<bool> {
        let track = self.sbar_track?;
        let thumb = self.sbar_thumb?;
        let (w, h) = self.size();
        let grab = 10.0 * crate::theme::ui_scale(w as f32, h as f32);
        if x < track[0] - grab || x > track[0] + track[2] + grab {
            return None;
        }
        if y < track[1] || y > track[1] + track[3] {
            return None;
        }
        Some(y >= thumb[1] && y <= thumb[1] + thumb[3])
    }

    /// Прокрутить список так, чтобы центр ползунка был в `y` с учётом захвата.
    fn sbar_drag_to(&mut self, y: f32) {
        let (Some(track), Some(thumb), Some(grab)) = (self.sbar_track, self.sbar_thumb, self.sbar_drag) else {
            return;
        };
        let max_scroll = (self.total_h - self.list_view_h()).max(0.0);
        let scroll = scroll_for_drag(y, track, thumb, grab, max_scroll);
        if let Mode::Library { scroll: s, .. } = &mut self.mode {
            *s = scroll;
        }
    }

    /// Нажать на скроллбар. true — событие съедено (песню не выбираем).
    fn sbar_press(&mut self) -> bool {
        if !matches!(self.mode, Mode::Library { .. }) {
            return false;
        }
        let [px, py] = self.cursor;
        let Some(in_thumb) = self.sbar_hit(px, py) else { return false };
        let thumb = self.sbar_thumb.unwrap_or([0.0; 4]);
        let grab = if in_thumb { py - thumb[1] } else { thumb[3] * 0.5 };
        self.sbar_drag = Some(grab);
        self.sbar_drag_to(py);
        true
    }

    /// Up/Down по плоскому списку чартов. Ctrl+Up/Down не трогаем — это
    /// прежний хоткей скорости.
    fn list_key_move(&mut self, key: &winit::keyboard::Key) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if self.key_mods.control_key() {
            return false;
        }
        let down = matches!(key, Key::Named(NamedKey::ArrowDown));
        let up = matches!(key, Key::Named(NamedKey::ArrowUp));
        if !down && !up {
            return false;
        }
        let flat: Vec<Entry> = self
            .view_groups
            .iter()
            .flat_map(|(_, v)| v.iter().copied())
            .collect();
        if flat.is_empty() {
            return true;
        }
        let cur = match &self.mode {
            Mode::Library { selected, .. } => *selected,
            _ => None,
        };
        let Some(next) = step_entry(&flat, cur, down) else { return true };
        if cur.map(|c| c.song) != Some(next.song) {
            self.play_preview(next.song);
        }
        if let Mode::Library { selected, .. } = &mut self.mode {
            *selected = Some(next);
        }
        true
    }

    /// Быстрая навигация по списку с клавиатуры (Home/End/PageUp/PageDown).
    fn list_key_nav(&mut self, key: &winit::keyboard::Key) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let view_h = self.list_view_h();
        let max_scroll = (self.total_h - view_h).max(0.0);
        let page = (view_h * 0.85).max(1.0);
        let first = self.card_rects.first().map(|&(i, _)| i);
        let last = self.card_rects.last().map(|&(i, _)| i);
        let Mode::Library { scroll, selected, .. } = &mut self.mode else { return false };
        match key {
            Key::Named(NamedKey::Home) => {
                *scroll = 0.0;
                if let Some(i) = first {
                    *selected = Some(i);
                }
                true
            }
            Key::Named(NamedKey::End) => {
                *scroll = max_scroll;
                if let Some(i) = last {
                    *selected = Some(i);
                }
                true
            }
            Key::Named(NamedKey::PageDown) => {
                *scroll = (*scroll + page).clamp(0.0, max_scroll);
                true
            }
            Key::Named(NamedKey::PageUp) => {
                *scroll = (*scroll - page).clamp(0.0, max_scroll);
                true
            }
            _ => false,
        }
    }

    fn on_click(&mut self) {
        let [px, py] = self.cursor;
        if self.settings.is_some() {
            self.on_click_settings();
            return;
        }
        if matches!(self.mode, Mode::Login { .. }) {
            let rects = self.login_rects.clone();
            for &(id, r) in &rects {
                if in_rect(px, py, r) {
                    match id {
                        0 | 1 => {
                            if let Mode::Login { focus, .. } = &mut self.mode { *focus = id; }
                        }
                        2 => self.submit_login(),
                        3 => {
                            if let Mode::Login { register, error, .. } = &mut self.mode {
                                *register = !*register;
                                *error = None;
                            }
                        }
                        _ => {}
                    }
                    return;
                }
            }
            return;
        }
        if matches!(self.mode, Mode::Play { .. }) {
            // пауза: клики по кнопкам панели (продолжить / настройки / выход)
            if let Mode::Play { paused: true, .. } = &self.mode {
                let rects = self.pause_rects.clone();
                for &(id, r) in &rects {
                    if in_rect(px, py, r) {
                        match id {
                            0 => self.set_paused(false),
                            1 => self.open_settings(),
                            _ => self.to_library(),
                        }
                        return;
                    }
                }
                return;
            }
            let t = match &self.mode {
                Mode::Play { clock, .. } => clock.current(),
                _ => 0.0,
            };
            // LMB — левый курсор: в обычном режиме мышь, в RMB-режиме тот
            // из мыши/пера, который сейчас левее.
            let aim = if self.rmb_pen_press {
                cursor_sides(self.mouse_x, self.pen_x()).0
            } else {
                mouse_aim(self.mouse_x)
            };
            let (input_mouse, input_tablet) = self.input_offsets();
            let offset = if aim.tablet_offset { input_tablet } else { input_mouse };
            let tj = t + offset / 1000.0;
            let mut judge = None;
            let mut err_ms = 0.0;
            if let Mode::Play { chart, play, paused, driver, .. } = &mut self.mode {
                // в просмотре реплея ввод выключен: клик не судит ноты
                if !*paused && driver.is_none() {
                    judge = apply_cursor_tap(play, chart, tj, aim, &mut self.hands);
                    // в выборку калибровки идут только попадания: у Miss
                    // `errors.last()` — это ошибка ПРЕДЫДУЩЕЙ ноты
                    if matches!(judge, Some(Judge::Perfect) | Some(Judge::Good)) {
                        err_ms = play.errors.last().copied().unwrap_or(0.0) * 1000.0;
                    }
                }
            }
            if matches!(judge, Some(Judge::Perfect) | Some(Judge::Good)) {
                match aim.device {
                    PressDevice::Pen => self.tablet_tap_errors.push(err_ms),
                    PressDevice::Mouse => self.mouse_tap_errors.push(err_ms),
                }
            }
            if let Some(r) = &mut self.replay {
                let src = if aim.device == PressDevice::Pen { 1 } else { 0 };
                r.action(tj, src, true, aim.x, judge, err_ms);
            }
            return;
        }
        if matches!(self.mode, Mode::Result { .. }) {
            let r = self.apply_button_rect();
            if in_rect(px, py, r) {
                // запомнить локальные оффсеты этой карты (глобальные не трогаем)
                self.apply_map_offsets();
                return;
            }
            self.to_library();
            return;
        }
        // открытые детали плея: только watch/close/клик мимо
        if self.lb_detail.is_some() {
            let watch = self.detail_watch_btn;
            let close = self.detail_close_btn;
            if in_rect(px, py, watch) {
                self.watch_detail_replay();
            } else if in_rect(px, py, close) {
                self.close_play_detail();
            } else {
                // клик по фону — закрыть, как Esc
                self.close_play_detail();
            }
            return;
        }
        let selected = match &self.mode {
            Mode::Library { selected, .. } => *selected,
            _ => None,
        };
        let scroll = self.scroll();
        // строка лидерборда: клик = выбрать и открыть детали, как Enter
        if selected.is_some() && self.open_dropdown.is_none() && !self.lb_rows.is_empty() {
            let rows = self.lb_rows.clone();
            for &(i, r) in &rows {
                if in_rect(px, py, r) {
                    self.open_play_detail(i);
                    return;
                }
            }
        }
        // открытый dropdown: выбор опции
        if self.open_dropdown.is_some() {
            let dd = self.open_dropdown.unwrap();
            let rects = self.dropdown_rects.clone();
            for &(i, r) in &rects {
                if in_rect(px, py, r) {
                    if dd == 0 {
                        self.sort_by = crate::library::SortBy::ALL[i as usize];
                    } else {
                        self.group_by = crate::library::GroupBy::ALL[i as usize];
                    }
                    self.open_dropdown = None;
                    self.mode = Mode::Library { scroll: 0.0, selected };
                    return;
                }
            }
            self.open_dropdown = None;
            return;
        }
        // кнопка SETTINGS в топбаре
        if in_rect(px, py, self.settings_btn) {
            self.open_settings();
            return;
        }
        // кнопка аккаунта: открывает экран настроек на блоке ONLINE
        if in_rect(px, py, self.login_btn) {
            self.open_settings();
            let logged_in = self.online.logged_in();
            if let Some(sc) = &mut self.settings {
                sc.select(crate::settings_screen::Row::Server, logged_in);
            }
            return;
        }
        // тумблер локально/онлайн в панели рекордов
        if self.online.logged_in() && in_rect(px, py, self.lb_toggle_rect) {
            self.lb_online = !self.lb_online;
            self.lb_focus = false;
            self.lb_sel = 0;
            return;
        }
        // кнопки sort/group
        if in_rect(px, py, self.sort_btn) {
            self.open_dropdown = Some(0);
            return;
        }
        if in_rect(px, py, self.group_btn) {
            self.open_dropdown = Some(1);
            return;
        }
        // Play — запуск выбранного чарта
        if let Some(e) = selected {
            if in_rect(px, py, self.play_btn) {
                self.start_play(e.song, e.diff);
                return;
            }
        }
        let cards = self.card_rects.clone();
        let (wf, hf) = self.size();
        let hf = hf as f32;
        let scale = crate::theme::ui_scale(wf as f32, hf);
        for &(e, r) in &cards {
            if py < screens::TOP_H * scale || py > hf - screens::BOT_H * scale {
                continue;
            }
            if in_rect(px, py, r) {
                if selected.map(|s| s.song) != Some(e.song) {
                    self.play_preview(e.song);
                }
                self.mode = Mode::Library { scroll, selected: Some(e) };
                return;
            }
        }
        // клик по пустому месту списка снимает выбор; арт и нижняя полоса
        // выбор не сбрасывают
        let w = wf as f32;
        let art_x = self
            .lib_geom
            .as_ref()
            .map(|g| g.art.map(|a| a[0]).unwrap_or(w - screens::MARGIN * scale))
            .unwrap_or(w - screens::MARGIN * scale);
        let in_list_area = px > screens::LEFT_W * scale
            && px < art_x
            && py > screens::TOP_H * scale
            && py < hf - screens::BOT_H * scale;
        if selected.is_some() && in_list_area {
            self.mode = Mode::Library { scroll, selected: None };
        }
    }

    fn redraw(&mut self) {
        if self.gfx.is_none() {
            return;
        }
        // фон библиотеки: двигаем падающее поле до layout/рисования
        self.tick_bg();
        let (w, h) = self.size();
        self.gfx.as_mut().unwrap().renderer.set_ui_size(w, h);

        // абсолютная мышь: позиция курсора → мировая X на судейской линии
        if self.mouse_absolute && matches!(self.mode, Mode::Play { .. }) {
            let scale = self.mouse_scale();
            let half = 10.24;
            let nx = ((self.cursor[0] - w as f32 * 0.5) * scale).clamp(-half, half);
            let dx = nx - self.mouse_x;
            self.mouse_x = nx;
            let t = match &self.mode {
                Mode::Play { clock, .. } => Some(clock.wall_to_time(Instant::now())),
                _ => None,
            };
            if let (Some(t), Some(r)) = (t, &mut self.replay) {
                if dx.abs() > 0.001 {
                    r.mouse_motion(t, dx, nx);
                }
            }
        }

        // обновление игры (сглаженные часы + lead-in)
        let held = self.left_held;
        let right_held = self.right_held;
        let rmb_pen = self.rmb_pen_press;
        // Планшет: тап = по сырым press-edges (точное время/x), слайд = по
        // удержанию активного планшета. Gaomon и Parblo равноправны — берём
        // тот, который отчитывался последним (а не жёстко Parblo).
        let active_tablet = self.tablets.as_ref().and_then(|t| t.active_state());
        let tablet_press = active_tablet.map(|p| p.present && p.press).unwrap_or(false);
        let tablet_x = active_tablet
            .filter(|p| p.present)
            .map(|p| (p.x_norm * 2.0 - 1.0) * crate::stage::note_x(2.0));
        // сырые репорты планшетов за кадр (для реплея и тапов)
        let tablet_events = self.tablets.as_ref().map(|t| t.drain()).unwrap_or_default();
        // эффективные оффсеты ввода: глобальные настройки + локальные карты
        // (одни и те же для тапов и слайдов обоих устройств)
        let (input_mouse, input_tablet) = self.input_offsets();
        // RMB-режим: кнопки адресуют левый/правый курсор по X. В обычном
        // режиме LMB — мышь, правая кнопка не судит.
        let (left_aim, right_aim) = if rmb_pen {
            cursor_sides(self.mouse_x, tablet_x)
        } else {
            (mouse_aim(self.mouse_x), mouse_aim(self.mouse_x))
        };
        let mut to_result = None;
        let mut replay_done = false;
        if let Mode::Play { chart, play, paused, song, diff, clock, music_on, driver } = &mut self.mode {
            if !*paused {
                let g = self.gfx.as_ref().unwrap();
                if !*music_on && clock.elapsed() >= clock.lead_in() {
                    g.music.play();
                    // music_on только если аудио реально загружено (иначе часы якорятся к 0)
                    *music_on = g.music.duration() > 0.0;
                }
                let t = clock.time(g.music.time(), *music_on);
                if let Some(driver) = driver {
                    // просмотр реплея: только записанные ноты, ввода и авто-Miss
                    // нет; после последней ноты (+хвост) — сразу в библиотеку
                    driver.apply(play, chart, t);
                    if t > driver.end() + REPLAY_TAIL {
                        replay_done = true;
                    }
                } else {
                    play.update(chart, t);
                    // раздельные оффсеты ввода: + сдвигает тап/слайд позже
                    let tt = t + input_tablet / 1000.0;
                    // сырой планшет в реплей + тапы по press-edges (без покадровой
                    // задержки); у каждого планшета СВОЙ prev_press
                    let mut prev = [self.gaomon_was_pressed, self.parblo_was_pressed];
                    for e in &tablet_events {
                        let et = clock.wall_to_time(e.wall);
                        if let Some(r) = &mut self.replay {
                            r.tablet(et, e);
                        }
                        let slot = match e.kind {
                            "gaomon" => 0,
                            "parblo" => 1,
                            _ => continue,
                        };
                        // собственный press пера игнорируется при RMB = pen:
                        // левая рука идёт с правой кнопки мыши
                        if tap_edge(&mut prev[slot], e.press) && !rmb_pen {
                            let px = (e.x_norm * 2.0 - 1.0) * crate::stage::note_x(2.0);
                            let tj = et + input_tablet / 1000.0;
                            let j = play.tap(chart, tj, px);
                            // только попадания: у Miss ошибка от прошлой ноты
                            let hit = matches!(j, Some(Judge::Perfect) | Some(Judge::Good));
                            let err = if hit {
                                play.errors.last().copied().unwrap_or(0.0) * 1000.0
                            } else {
                                0.0
                            };
                            if hit {
                                self.tablet_tap_errors.push(err);
                            }
                            // статистика по рукам: тап стилуса = левая рука
                            if let Some(j) = j {
                                self.hands.tablet.add(j);
                            }
                            if let Some(r) = &mut self.replay {
                                r.action(tj, 1, true, px, j, err);
                            }
                        }
                    }
                    self.gaomon_was_pressed = prev[0];
                    self.parblo_was_pressed = prev[1];
                    if held {
                        let offset = if left_aim.tablet_offset {
                            input_tablet
                        } else {
                            input_mouse
                        };
                        let lt = t + offset / 1000.0;
                        let hit = play.slide(chart, lt, left_aim.x, self.speed);
                        if hit {
                            if let Some(r) = &mut self.replay {
                                let src = if left_aim.device == PressDevice::Pen {
                                    1
                                } else {
                                    0
                                };
                                r.action(lt, src, false, left_aim.x, None, 0.0);
                            }
                        }
                    }
                    if tablet_press && !rmb_pen {
                        if let Some(px) = tablet_x {
                            let hit = play.slide(chart, tt, px, self.speed);
                            if hit {
                                if let Some(r) = &mut self.replay {
                                    r.action(tt, 1, false, px, None, 0.0);
                                }
                            }
                        }
                    }
                    // RMB: удержание правой кнопки ведёт правый курсор
                    // (X/устройство по текущим позициям мыши и пера)
                    if rmb_pen && right_held {
                        let offset = if right_aim.tablet_offset {
                            input_tablet
                        } else {
                            input_mouse
                        };
                        let rt = t + offset / 1000.0;
                        let hit = play.slide(chart, rt, right_aim.x, self.speed);
                        if hit {
                            if let Some(r) = &mut self.replay {
                                let src = if right_aim.device == PressDevice::Pen {
                                    1
                                } else {
                                    0
                                };
                                r.action(rt, src, false, right_aim.x, None, 0.0);
                            }
                        }
                    }
                    // два независимых трека: мышь и перо (перо — даже при
                    // hover, если планшет активен)
                    if let Some(r) = &mut self.replay {
                        r.track_mouse(t, self.mouse_x);
                        if let Some(px) = tablet_x {
                            r.track_pen(t, px);
                        }
                    }
                    let dur = g.music.duration();
                    let mut finish =
                        play.finished(chart, t) || (*music_on && dur > 0.0 && t >= dur - 0.05);
                    // надёжный финиш: музыка доиграла → grace 1.2 с реального времени
                    if *music_on && g.music.is_empty() {
                        let at = *self.music_ended_at.get_or_insert_with(Instant::now);
                        if at.elapsed().as_secs_f32() > 1.2 {
                            finish = true;
                        }
                    }
                    if finish {
                        // DMR3 уезжает вместе со скором; при разной длине
                        // чарта и плея поля в теле просто нет
                        let replay = match &mut self.replay {
                            Some(r) => {
                                r.track_end(t, self.mouse_x, tablet_x);
                                let (left, right) = r.tracks();
                                crate::replay::dmr3_base64(chart, play, right, left)
                            }
                            None => None,
                        };
                        to_result = Some((*song, *diff, play.clone(), replay));
                    }
                }
            }
        }
        if replay_done {
            // просмотр: без экрана результата и без отправки на сервер
            self.to_library();
        } else if let Some((song, diff, play, replay)) = to_result {
            self.gfx.as_ref().unwrap().music.pause();
            let diff_name = self.songs[song]
                .charts
                .get(diff)
                .map(|c| c.diff.clone())
                .unwrap_or_default();
            let song_id = self.songs[song].id.clone();
            self.play_key = (song_id.clone(), diff_name.clone());
            // Локальный оффсет карты: рекомендация по ОДНОМУ этому плею —
            // текущее локальное значение + средняя ошибка тапов устройства.
            // Устройство без тапов в этом прохождении не получает предложения («—»).
            let mean_of = |v: &[f32]| -> Option<f32> {
                if v.is_empty() { None } else { Some(v.iter().sum::<f32>() / v.len() as f32) }
            };
            let mouse_mean = mean_of(&self.mouse_tap_errors);
            let tablet_mean = mean_of(&self.tablet_tap_errors);
            self.result_map_mouse = self
                .map_offsets
                .recommend(&song_id, &diff_name, crate::map_offsets::MOUSE, mouse_mean);
            self.result_map_tablet = self.map_offsets.recommend(
                &song_id,
                &diff_name,
                crate::map_offsets::TABLET,
                tablet_mean,
            );
            // сохраняем лучший результат локально
            self.result_is_record = self.results.record(&song_id, &diff_name, &play, self.speed);
            // статистика по рукам: к тапам добавляем авто-Miss, которые
            // play.update атрибутировал руке по знаку pos
            let mut hands = self.hands;
            hands.fold_auto_miss(&play);
            // онлайн: рекорд сначала в очередь на диск, отправка — в фоне
            self.result_pp = ResultPp::Hidden;
            self.result_rec = None;
            if let Some(rec) =
                self.online
                    .pending_score(&song_id, &diff_name, &play, self.speed, &hands, replay.as_deref())
            {
                let server = rec.server.clone();
                let seq = self.queue.enqueue(rec.clone(), &server);
                self.score_reports.push((seq, rec.clone()));
                if self.score_reports.len() > 8 {
                    self.score_reports.remove(0);
                }
                // ответа ещё нет: показываем «PP PENDING» до события очереди
                self.result_pp = ResultPp::Pending;
                self.result_rec = Some(rec);
            }
            // дамп таймингов для диагностики оффсета
            if let Ok(mut f) = std::fs::File::create(".work/offset_debug.json") {
                use std::io::Write;
                let _ = writeln!(
                    f,
                    "{{\"offset_ms\":{},\"map_mouse_ms\":{},\"map_tablet_ms\":{},\"mean_ms\":{},\"mean_tap_ms\":{},\"median_ms\":{},\"n\":{},\"errors_ms\":{:?}}}",
                    self.audio_offset_ms,
                    self.map_local_ms(crate::map_offsets::MOUSE),
                    self.map_local_ms(crate::map_offsets::TABLET),
                    play.mean_error() * 1000.0,
                    play.mean_tap_error() * 1000.0,
                    play.median_error() * 1000.0,
                    play.errors.len(),
                    play.errors.iter().map(|e| (e * 1000.0).round() as i32).collect::<Vec<_>>()
                );
            }
            self.mode = Mode::Result { song, diff, play };
            self.set_play_cursor(false);
            // debug: автосохранение реплея на результате (для headless-теста)
            if std::env::var("DEEMO_REPLAY_AUTOSAVE").is_ok() {
                if let Mode::Result { play, .. } = &self.mode {
                    if let Some(r) = &self.replay {
                        match r.save(play) {
                            Ok(p) => println!("replay saved: {}", p.display()),
                            Err(e) => eprintln!("replay error: {e}"),
                        }
                    }
                }
            }
        }

        self.layout();

        let mut ui: Vec<UiQuad> = Vec::new();
        let mut sprites = Vec::new();
        let mut screen = vec![
            ScreenQuad { tex: Tex::Bg, color: WHITE },
            ScreenQuad { tex: Tex::BgMask, color: [1.0, 1.0, 1.0, 0.8] },
        ];

        if self.settings.is_some() {
            self.draw_settings(&mut ui);
        } else if matches!(self.mode, Mode::Library { .. }) {
            let selected = match &self.mode {
                Mode::Library { selected, .. } => *selected,
                _ => None,
            };
            self.draw_library(&mut ui, selected);
        } else if let Mode::Play { chart, play, paused, song, diff, clock, driver, .. } = &self.mode {
            let t = clock.current();
            let mut scene = Scene::with_play(chart, self.speed, play);
            scene.guides = self.note_guides;
            let mvp = scene.mvp(w as f32 / h as f32);
            self.gfx.as_mut().unwrap().renderer.set_camera(&mvp);
            sprites = scene.sprites(t, w, h);
            screen[1].color = [1.0, 1.0, 1.0, scene.bg_mask_alpha(t)];
            // засечки на линии нажатия: мышь (голубая) + активный планшет
            // (оранжевая; Gaomon и Parblo — равноправно, кто говорил последним).
            // В просмотре реплея ввода нет — засечек тоже.
            if driver.is_none() {
                // обычные игровые маркеры; Hide cursor — только скрытие
                // системного курсора, новых визуалов в игре нет
                let (mouse_press, pen_press) = cursor_marker_press(
                    self.rmb_pen_press,
                    self.mouse_x,
                    tablet_x,
                    self.left_held,
                    self.right_held,
                    tablet_press,
                );
                sprites.push(crate::stage::marker(
                    self.mouse_x,
                    [0.3, 0.8, 1.0, 1.0],
                    mouse_press,
                ));
                if let Some(px) = tablet_x {
                    sprites.push(crate::stage::marker(
                        px,
                        [1.0, 0.6, 0.2, 1.0],
                        pen_press,
                    ));
                }
            } else if let Some(driver) = driver {
                // курсор реплея: два вертикальных луча (левая/правая рука)
                crate::scene::push_replay_cursor(&mut sprites, driver, t);
            }
            self.draw_hud(&mut ui, *song, *diff, play, t, *paused, driver.as_ref());
        } else if let Mode::Result { song, diff, play } = &self.mode {
            self.draw_result(&mut ui, *song, *diff, play);
        } else if matches!(self.mode, Mode::Login { .. }) {
            self.draw_login(&mut ui);
        }
        // всплывающее уведомление (сохранение реплея по F2 и т.п.)
        if let Some((msg, t0)) = &self.toast {
            let age = t0.elapsed().as_secs_f32();
            if age < 2.5 {
                let alpha = if age > 2.0 { (2.5 - age) / 0.5 } else { 1.0 };
                let ctx = screens::Ctx {
                    text: self.text.as_ref().expect("font"),
                    white: self.white_ui,
                    fade_x: self.fade_x_ui,
                    fade_y: self.fade_y_ui,
                    w: w as f32,
                    h: h as f32,
                    s: crate::theme::ui_scale(w as f32, h as f32),
                };
                screens::draw_toast(&ctx, &mut ui, msg, alpha);
            }
        }

        if let Some((path, t0)) = &self.auto_shot {
            if t0.elapsed().as_secs_f32() > self.auto_shot_delay {
                // глифы, впервые нарисованные в этом кадре, — в атлас до рендера
                if let (Some(text), Some(g)) = (self.text.as_mut(), self.gfx.as_mut()) {
                    text.flush(&mut g.renderer);
                }
                let g = self.gfx.as_mut().unwrap();
                let tex = g.renderer.offscreen(w, h, &sprites, &screen, &ui);
                let _ = g.renderer.save_png(&tex, w, h, path);
                println!("screenshot: {path}");
                std::process::exit(0);
            }
        }

        // новые глифы из этого кадра — в атлас до отрисовки
        if let (Some(text), Some(g)) = (self.text.as_mut(), self.gfx.as_mut()) {
            text.flush(&mut g.renderer);
        }

        let g = self.gfx.as_mut().unwrap();
        match g.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
                let mut enc = g
                    .renderer
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
                g.renderer.draw(&mut enc, &view, &sprites, &screen, &ui);
                g.renderer.queue.submit(Some(enc.finish()));
                g.renderer.queue.present(frame);
                self.frames += 1;
                let el = self.fps_last.elapsed().as_secs_f32();
                if el >= 2.0 {
                    println!("fps = {:.0}", self.frames as f32 / el);
                    self.frames = 0;
                    self.fps_last = Instant::now();
                }
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                g.surface.configure(&g.renderer.device, &g.config);
            }
            other => eprintln!("surface: {other:?}"),
        }
    }

    fn draw_library(&mut self, ui: &mut Vec<UiQuad>, selected: Option<Entry>) {
        let (w, h) = self.size();
        let wf = w as f32;
        let hf = h as f32;
        // онлайн-лидерборд и публичные реплеи: один фоновый запрос на карту.
        // Запрос нужен и без входа в аккаунт (список реплеев публичный), но
        // в headless-скриншотах сеть не трогаем — берём фикстуру.
        if self.gfx.is_some() {
            if let Some(e) = selected {
                if let Some(chart) = self.songs.get(e.song).and_then(|s| s.charts.get(e.diff)) {
                    let sid = self.songs[e.song].id.clone();
                    let dn = chart.diff.clone();
                    // Лидерборд нужен и в локальном виде: из него берётся pp
                    // игрока для «BEST ON THIS CHART». Показываем его только
                    // при lb_online (см. draw_records).
                    let want_lb = self.online.logged_in();
                    self.request_leaderboard(&sid, &dn, want_lb);
                }
            }
        }

        // обложки видимых строк + арт выбранной песни
        let scale = crate::theme::ui_scale(wf, hf);
        let mut covers: HashMap<usize, usize> = HashMap::new();
        for &(e, r) in &self.card_rects {
            if r[1] + r[3] < screens::TOP_H * scale || r[1] > hf {
                continue;
            }
            if let Some(handle) = self.cover(e.song) {
                covers.insert(e.song, handle);
            }
        }
        if let Some(e) = selected {
            if let Some(handle) = self.cover(e.song) {
                covers.insert(e.song, handle);
            }
        }
        let art = selected.and_then(|e| self.full_cover(e.song));
        let scroll = self.scroll();
        let (cur_x, cur_y) = (self.cursor[0], self.cursor[1]);
        let row_h = self
            .lib_geom
            .as_ref()
            .map(|g| g.row_h)
            .unwrap_or(screens::ROW_H * scale);
        let hover = self
            .card_rects
            .iter()
            .find(|(_, r)| {
                r[1] >= screens::TOP_H * scale
                    && r[1] + row_h <= hf - screens::BOT_H * scale
                    && in_rect(cur_x, cur_y, *r)
            })
            .map(|(e, _)| *e);
        let lb_hover = if selected.is_some() {
            self.lb_rows
                .iter()
                .find(|(_, r)| in_rect(cur_x, cur_y, *r))
                .map(|(i, _)| *i)
        } else {
            None
        };
        let anim_t = self.anim_t();
        let Some(geom) = self.lib_geom.clone() else {
            return;
        };
        let chrome = screens::LibChrome {
            settings_btn: self.settings_btn,
            sort_btn: self.sort_btn,
            group_btn: self.group_btn,
            search_rect: self.search_rect,
            play_btn: self.play_btn,
            login_btn: self.login_btn,
            lb_toggle_rect: self.lb_toggle_rect,
        };

        // элементы открытого dropdown (с отметкой текущего значения)
        let mut items: Vec<screens::DropdownItem> = Vec::new();
        if let Some(dd) = self.open_dropdown {
            let (labels, current): (Vec<&'static str>, &'static str) = if dd == 0 {
                (
                    crate::library::SortBy::ALL.iter().map(|s| s.label()).collect(),
                    self.sort_by.label(),
                )
            } else {
                (
                    crate::library::GroupBy::ALL.iter().map(|g| g.label()).collect(),
                    self.group_by.label(),
                )
            };
            for &(i, r) in &self.dropdown_rects {
                let label = labels.get(i as usize).copied().unwrap_or("");
                items.push(screens::DropdownItem {
                    rect: r,
                    label,
                    active: label == current,
                });
            }
        }
        let dropdown = self.open_dropdown.map(|_| screens::Dropdown { items: &items });

        let sort_label = self.sort_by.label();
        let group_label = self.group_by.label();
        let lb_pending = self.lb_pending.is_some();
        let ctx = screens::Ctx {
            text: self.text.as_ref().expect("font"),
            white: self.white_ui,
            fade_x: self.fade_x_ui,
            fade_y: self.fade_y_ui,
            w: wf,
            h: hf,
            s: crate::theme::ui_scale(wf, hf),
        };
        let view = screens::LibraryView {
            geom: &geom,
            chrome: &chrome,
            songs: &self.songs,
            results: &self.results,
            online: &self.online,
            stars: &self.stars,
            cards: &self.card_rects,
            headers: &self.lib_headers,
            selected,
            search: &self.search,
            sort_label,
            group_label,
            covers: &covers,
            art,
            hover,
            lb_online: self.lb_online,
            lb_pending,
            lb_records: &self.lb_records,
            lb_sel: self.lb_sel,
            lb_focus: self.lb_focus,
            lb_rows: &self.lb_rows,
            lb_hover,
            dropdown,
            total_h: self.total_h,
            scroll,
            t: anim_t,
            bg_notes: self.bg_field.live(),
            bg_t: self.bg_view_time(),
        };
        screens::draw_library(&ctx, &view, ui);

        // детали плея — поверх библиотеки, та же типографика, что у результата
        if self.lb_detail.is_some() {
            let ui_rects = self.draw_play_detail_overlay(ui, &ctx);
            self.detail_watch_btn = ui_rects.watch_btn;
            self.detail_close_btn = ui_rects.close_btn;
        }
    }

    /// Нарисовать открытые детали плея и вернуть интерактивные прямоугольники.
    fn draw_play_detail_overlay(&self, ui: &mut Vec<UiQuad>, ctx: &screens::Ctx) -> screens::PlayDetailUi {
        let Some(detail) = &self.lb_detail else {
            return screens::PlayDetailUi {
                watch_btn: [0.0; 4],
                close_btn: [0.0; 4],
            };
        };
        // карта, на которой открыт оверлей (выбор не мог уехать: библиотека
        // под ним не кликается)
        let (song_title, artist, diff_name) = match &self.mode {
            Mode::Library { selected: Some(e), .. } => {
                let song = &self.songs[e.song];
                let chart = &song.charts[e.diff];
                (song.title.as_str(), song.composer.as_str(), chart.diff.as_str())
            }
            _ => ("", "", ""),
        };
        // звезда — снимок момента открытия деталей (detail.star)
        let star = detail.star;
        let can_watch = detail.can_watch();
        let reason = detail.watch_reason();
        let view = screens::PlayDetailView {
            song_title,
            artist,
            diff: diff_name,
            star,
            record: &detail.record,
            replay: detail.replay.as_ref(),
            can_watch,
            reason,
        };
        screens::draw_play_detail(ctx, &view, ui)
    }

    fn on_key_login(&mut self, key: Key, event_loop: &ActiveEventLoop) {
        match key {
            Key::Named(NamedKey::Escape) => self.to_library(),
            Key::Named(NamedKey::Tab) => {
                if let Mode::Login { focus, .. } = &mut self.mode {
                    *focus = 1 - *focus;
                }
            }
            Key::Named(NamedKey::Enter) => self.submit_login(),
            Key::Named(NamedKey::Backspace) => {
                if let Mode::Login { nickname, password, focus, .. } = &mut self.mode {
                    if *focus == 0 { nickname.pop(); } else { password.pop(); }
                }
            }
            Key::Character(c) => {
                let s = c.as_str();
                if let Mode::Login { nickname, password, focus, .. } = &mut self.mode {
                    if *focus == 0 {
                        if s.chars().all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '-') && nickname.len() < 24 {
                            nickname.push_str(s);
                        }
                    } else if password.len() < 64 {
                        password.push_str(s);
                    }
                }
            }
            _ => {}
        }
        let _ = event_loop;
    }

    fn submit_login(&mut self) {
        let (nickname, password, register) = match &self.mode {
            Mode::Login { nickname, password, register, .. } => (nickname.clone(), password.clone(), *register),
            _ => return,
        };
        match self.online.auth(&nickname, &password, register) {
            Ok(nick) => {
                self.toast = Some((format!("Signed in as {nick}"), Instant::now()));
                self.to_library();
            }
            Err(e) => {
                if let Mode::Login { error, .. } = &mut self.mode {
                    *error = Some(e);
                }
            }
        }
    }

    /// `song:diff` выбранной карты (для сопоставления ответов сети).
    fn chart_key(&self) -> Option<String> {
        let Mode::Library { selected: Some(e), .. } = &self.mode else {
            return None;
        };
        let song = self.songs.get(e.song)?;
        let chart = song.charts.get(e.diff)?;
        Some(format!("{}:{}", song.id, chart.diff))
    }

    /// Принять скачанный блоб (DMR1/DMR2/DMR3; каждый кадр, как lb_rx).
    fn poll_replay_blob(&mut self) {
        let Some(rx) = &self.rp_blob_rx else { return };
        match rx.try_recv() {
            Ok((song, diff, nick, res)) => {
                self.rp_blob_rx = None;
                self.rp_blob_pending = None;
                // пока качали, могли уйти из библиотеки или сменить карту —
                // не стартуем вслепую
                if self.settings.is_some() {
                    return;
                }
                let current = match &self.mode {
                    Mode::Library { selected: Some(e), .. } => Some((e.song, e.diff)),
                    _ => None,
                };
                if current != Some((song, diff)) {
                    return;
                }
                match res {
                    Ok(bytes) => self.start_replay_play(song, diff, &nick, &bytes),
                    Err(_) => {
                        self.toast = Some(("Replay unavailable".into(), Instant::now()));
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.rp_blob_rx = None;
                self.rp_blob_pending = None;
            }
        }
    }

    /// Один фоновый запрос на карту: лидерборд (если он сейчас показан) и
    /// публичный список реплеев. Оба едут в одном потоке и одним сообщением.
    fn request_leaderboard(&mut self, song: &str, diff: &str, want_lb: bool) {
        let chart_key = format!("{song}:{diff}");
        // список другой карты не должен висеть, пока грузится этот
        if !self.lb_key.starts_with(&format!("{chart_key}:")) {
            self.lb_records.clear();
            self.lb_sel = 0;
            self.lb_focus = false;
            self.lb_detail = None;
        }
        if self.rp_key != chart_key && !self.rp_records.is_empty() {
            self.rp_records.clear();
            self.lb_sel = 0;
            self.lb_focus = false;
            self.lb_detail = None;
        }
        let key = format!("{chart_key}:{}", u8::from(want_lb));
        if self.lb_key == key || self.lb_pending.as_deref() == Some(key.as_str()) {
            return;
        }
        self.lb_pending = Some(key.clone());
        let on = self.online.clone();
        let song = song.to_string();
        let diff = diff.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        self.lb_rx = Some(rx);
        std::thread::spawn(move || {
            let recs = if want_lb {
                on.leaderboard(&song, &diff, 20).unwrap_or_default()
            } else {
                Vec::new()
            };
            // ошибка/пусто — так же «нет списка»: лишнего блока в панели нет
            let replays = on.replays(&song, &diff).unwrap_or_default();
            let _ = tx.send((key, recs, replays));
        });
    }

    fn draw_login(&mut self, ui: &mut Vec<UiQuad>) {
        let (w, h) = self.size();
        let (nickname, password_len, focus, register, error) = match &self.mode {
            Mode::Login {
                nickname,
                password,
                focus,
                register,
                error,
            } => (
                nickname.clone(),
                password.chars().count(),
                *focus,
                *register,
                error.clone(),
            ),
            _ => return,
        };
        let server = self.online.state.server.clone();
        let ctx = screens::Ctx {
            text: self.text.as_ref().expect("font"),
            white: self.white_ui,
            fade_x: self.fade_x_ui,
            fade_y: self.fade_y_ui,
            w: w as f32,
            h: h as f32,
            s: crate::theme::ui_scale(w as f32, h as f32),
        };
        let view = screens::LoginView {
            server: &server,
            nickname: &nickname,
            password_len,
            focus,
            register,
            error: error.as_deref(),
        };
        let rects = screens::draw_login(&ctx, &view, ui);
        self.login_rects = rects;
    }

    fn draw_hud(
        &self,
        ui: &mut Vec<UiQuad>,
        song: usize,
        diff: usize,
        play: &Play,
        t: f32,
        paused: bool,
        replay: Option<&crate::replay::ReplayDriver>,
    ) {
        let (w, h) = self.size();
        let text = self.text.as_ref().expect("font");
        let s = &self.songs[song];
        // Игровой HUD не менялся: те же позиции, кегли и цвета.
        text.draw(&s.title, 24.0, 18.0, 20.0, WHITE, ui);
        text.draw(&s.composer, 24.0, 44.0, 13.0, DIM, ui);
        // просмотр реплея: только подпись «REPLAY · ник» и время
        if let Some(driver) = replay {
            screens::draw_replay_badge(text, ui, &driver.nick, t, driver.end());
        }
        // В игре нет легаси-счёта: только текущая hit accuracy и комбо.
        let acc = format!("{:.2}%", play.hit_accuracy());
        let aw = text.measure(&acc, 26.0);
        text.draw(&acc, w as f32 - aw - 24.0, 18.0, 26.0, WHITE, ui);
        let acc_l = text.measure("ACCURACY", 10.0);
        text.draw("ACCURACY", w as f32 - acc_l - 24.0, 48.0, 10.0, DIM, ui);
        if play.combo > 1 {
            let c = format!("{}", play.combo);
            text.draw_centered(&c, w as f32 * 0.5, h as f32 * 0.30, 56.0, [1.0, 1.0, 1.0, 0.85], ui);
            text.draw_centered("COMBO", w as f32 * 0.5, h as f32 * 0.30 + 60.0, 14.0, DIM, ui);
        }
        if let Some((j, jt)) = play.last_judge {
            if t - jt < 0.5 {
                let (label, col) = match j {
                    Judge::Perfect => ("PERFECT", [1.0, 0.95, 0.6, 1.0]),
                    Judge::Good => ("GOOD", [0.6, 0.9, 1.0, 1.0]),
                    Judge::Miss => ("MISS", [1.0, 0.4, 0.4, 1.0]),
                };
                let a = 1.0 - (t - jt) / 0.5;
                let col = [col[0], col[1], col[2], a];
                text.draw_centered(label, w as f32 * 0.5, h as f32 * 0.62, 26.0, col, ui);
            }
        }
        let sp = format!(
            "note speed {:.1} (up/down)   guides: {} (G)   Space — pause   Esc — quit",
            self.speed,
            if self.note_guides { "on" } else { "off" }
        );
        text.draw(&sp, 24.0, h as f32 - 30.0, 14.0, DIM, ui);
        if paused {
            // меню паузы — новая тема; сам HUD остаётся прежним
            let diff_name = s.charts.get(diff).map(|c| c.diff.clone()).unwrap_or_default();
            let ctx = screens::Ctx {
                text,
                white: self.white_ui,
                fade_x: self.fade_x_ui,
                fade_y: self.fade_y_ui,
                w: w as f32,
                h: h as f32,
                s: crate::theme::ui_scale(w as f32, h as f32),
            };
            screens::draw_pause(&ctx, ui, &s.title, &diff_name, &self.pause_rects);
        }
    }

    fn apply_button_rect(&self) -> [f32; 4] {
        let (w, h) = self.size();
        screens::result_layout(w as f32, h as f32).apply_btn
    }

    fn draw_result(&self, ui: &mut Vec<UiQuad>, song: usize, diff: usize, play: &Play) {
        let (w, h) = self.size();
        let s = &self.songs[song];
        let rank = crate::results::rank_for(play.accuracy(), play.counts[2] == 0);
        let diff_name = s
            .charts
            .get(diff)
            .map(|c| c.diff.clone())
            .unwrap_or_default();
        let ctx = screens::Ctx {
            text: self.text.as_ref().expect("font"),
            white: self.white_ui,
            fade_x: self.fade_x_ui,
            fade_y: self.fade_y_ui,
            w: w as f32,
            h: h as f32,
            s: crate::theme::ui_scale(w as f32, h as f32),
        };
        let view = screens::ResultView {
            song_title: &s.title,
            artist: &s.composer,
            diff: &diff_name,
            star: crate::stars::star(&self.stars, &s.id, &diff_name),
            play,
            rank,
            is_record: self.result_is_record,
            pp: self.result_pp,
            map_mouse: self.result_map_mouse,
            map_tablet: self.result_map_tablet,
            local_mouse: self.map_offsets.local_ms(
                &self.songs[song].id,
                &diff_name,
                crate::map_offsets::MOUSE,
            ),
            local_tablet: self.map_offsets.local_ms(
                &self.songs[song].id,
                &diff_name,
                crate::map_offsets::TABLET,
            ),
            global_mouse: self.mouse_offset_ms,
            global_tablet: self.tablet_offset_ms,
            mouse_errs: &self.mouse_tap_errors,
            tablet_errs: &self.tablet_tap_errors,
        };
        screens::draw_result(&ctx, &view, ui);
    }

    /// Применить локальные оффсеты карты с экрана результата (кнопка / F3).
    ///
    /// Глобальные `mouse_offset_ms`/`tablet_offset_ms` НЕ меняются: кнопка
    /// пишет только `data/offsets.json`, а при игре эффективный оффсет = глобальный
    /// + локальный (см. [`App::input_offsets`]). Устройство без тапов в этом прохождении
    /// сохраняет прежнее значение.
    fn apply_map_offsets(&mut self) {
        match self.commit_map_offsets() {
            Some(msg) => {
                println!("{msg}");
                self.toast = Some((msg, Instant::now()));
            }
            None => {
                let msg = "Map offset: no tap data for this chart yet".to_string();
                println!("{msg}");
                self.toast = Some((msg, Instant::now()));
            }
        }
    }

    /// Записать рекомендации в карту и сохранить файл. `None` — сохранять
    /// нечего (ни у одного устройства не было тапов).
    fn commit_map_offsets(&mut self) -> Option<String> {
        let (song, diff) = self.play_key.clone();
        let mouse = self.result_map_mouse;
        let tablet = self.result_map_tablet;
        if mouse.is_none() && tablet.is_none() {
            return None;
        }
        if let Some(ms) = mouse {
            self.map_offsets
                .set_local(&song, &diff, crate::map_offsets::MOUSE, ms);
        }
        if let Some(ms) = tablet {
            self.map_offsets
                .set_local(&song, &diff, crate::map_offsets::TABLET, ms);
        }
        self.map_offsets.save();
        let m = self.map_offsets.local_ms(&song, &diff, crate::map_offsets::MOUSE);
        let t = self.map_offsets.local_ms(&song, &diff, crate::map_offsets::TABLET);
        Some(format!(
            "Map offset saved: mouse {:+} · tablet {:+} ms",
            m.round() as i32,
            t.round() as i32
        ))
    }
}

/// Headless-скриншот экрана меню: `--shot out.png --screen library|main|pause|
/// results|settings|login|replay`. Рисует те же экраны, что и игровой цикл, но
/// без окна и без ввода — для проверки дизайна.
///
/// Для `--screen settings` флаг `--packs running|done|cancelled` подставляет
/// синтетический прогресс строки «Download all packs» (в сеть не ходим).
/// Демо-скриншот: собрать источник фона из чарта (song, diff).
fn demo_bg_source(app: &App, song: usize, diff: usize) -> Option<crate::bg::BgSource> {
    let c = app.songs.get(song)?.charts.get(diff)?;
    let chart = Chart::load(&c.path).ok()?;
    Some(crate::bg::BgSource::from_chart(&chart))
}

/// Демо-скриншот: «прогреть» поле перед фиксированным временем `t`, чтобы в
/// кадре были не только будущие ноты, но и те, что уже вошли в кадр. Шаги по
/// 1/60 с имитируют обычный игровой цикл (никакой анимации в headless нет).
fn demo_bg_warm(app: &mut App, song: usize, diff: usize, t: f32) {
    let Some(src) = demo_bg_source(app, song, diff) else { return };
    let warm = crate::bg::BG_FALL;
    let start = t - warm;
    app.bg_field.reset(start);
    app.bg_field.set_source(src, start);
    let steps = 120;
    for i in 1..=steps {
        let dt = warm / steps as f32;
        app.bg_field.advance(dt, start + dt * i as f32);
    }
    app.bg_t_demo = Some(t);
}

pub fn shot_screen(
    out: &str,
    screen: &str,
    demo: bool,
    width: u32,
    height: u32,
    packs: Option<&str>,
) -> Result<()> {
    let gpu = Gpu::new(None)?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(gpu.device, gpu.queue, format)?;
    let text = Text::new(&mut renderer)?;
    let white = renderer.add_ui_texture("white-ui", 1, 1, &[255, 255, 255, 255]);
    let (fx, fy) = crate::theme::make_fades(&mut renderer);
    renderer.set_ui_size(width, height);

    let mut app = App::new()?;
    app.text = Some(text);
    app.white_ui = white;
    app.fade_x_ui = fx;
    app.fade_y_ui = fy;
    app.headless_size = (width, height);
    // headless: публичные реплеи/лидерборд из фикстур — сеть в шотах не трогаем.
    // DEEMO_SHOT_REPLAYS=<json {"replays":[...]}> — кто имеет реплей;
    // DEEMO_SHOT_LB=<json {"leaderboard":[...]}> — строки лидерборда;
    // DEEMO_SHOT_DETAIL=1 открывает детали строки DEEMO_SHOT_DETAIL_IDX (0).
    if let Ok(path) = std::env::var("DEEMO_SHOT_REPLAYS") {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Ok(list) = crate::online::parse_replays(&v) {
                    app.rp_records = list;
                }
            }
        }
    }
    if let Ok(path) = std::env::var("DEEMO_SHOT_LB") {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(list) = v.get("leaderboard") {
                    if let Ok(recs) = serde_json::from_value::<Vec<crate::online::OnlineRecord>>(list.clone()) {
                        app.lb_records = recs;
                        app.lb_online = true;
                        app.online.state.token = Some("shot".into());
                        app.online.state.nickname = Some(
                            std::env::var("DEEMO_SHOT_NICK").unwrap_or_else(|_| "cyrsia".into()),
                        );
                    }
                }
            }
        }
    }
    // сортировка/группировка и выбранная сложность — для кадров списка чартов
    if let Ok(sort) = std::env::var("DEEMO_SHOT_SORT") {
        app.sort_by = match sort.as_str() {
            "difficulty" | "difficulty-low" => crate::library::SortBy::Difficulty,
            "difficulty-high" | "difficulty-desc" => crate::library::SortBy::DifficultyDesc,
            "artist" => crate::library::SortBy::Artist,
            "album" => crate::library::SortBy::Album,
            "length" => crate::library::SortBy::Length,
            "score" => crate::library::SortBy::Score,
            _ => crate::library::SortBy::Title,
        };
    }
    if let Ok(group) = std::env::var("DEEMO_SHOT_GROUP") {
        app.group_by = match group.as_str() {
            "none" => crate::library::GroupBy::None,
            "artist" => crate::library::GroupBy::Artist,
            "length" => crate::library::GroupBy::Length,
            _ => crate::library::GroupBy::Album,
        };
    }
    // фиксированная фаза мотива, чтобы скриншоты были воспроизводимы
    app.anim_start = Instant::now() - std::time::Duration::from_secs_f32(5.0);

    let sel = app
        .songs
        .iter()
        .position(|s| s.id == "v20thebeautifulmonnlight")
        .or_else(|| {
            app.songs
                .iter()
                .position(|s| s.cover.is_some() && s.charts.len() >= 3)
        })
        .unwrap_or(0);
    let sel = std::env::var("DEEMO_SHOT_SONG")
        .ok()
        .and_then(|id| app.songs.iter().position(|s| s.id == id))
        .unwrap_or(sel);
    let diff = std::env::var("DEEMO_SHOT_DIFF")
        .ok()
        .and_then(|d| {
            let charts = &app.songs.get(sel)?.charts;
            d.parse::<usize>()
                .ok()
                .filter(|&i| i < charts.len())
                .or_else(|| charts.iter().position(|c| c.diff == d))
        })
        .unwrap_or_else(|| {
            app.songs
                .get(sel)
                .map(|s| s.charts.len().saturating_sub(1))
                .unwrap_or(0)
        });
    let entry = Entry::new(sel, diff);

    let mut toast: Option<String> = None;
    match screen {
        "main" => {
            app.mode = Mode::Library {
                scroll: 0.0,
                selected: None,
            };
        }
        "pause" | "play" => {
            app.start_play(sel, diff);
            if let Mode::Play { play, paused, .. } = &mut app.mode {
                play.score = 874_320;
                play.combo = 63;
                play.max_combo = 63;
                play.counts = [120, 4, 2];
                play.last_judge = Some((Judge::Perfect, 5.72));
                *paused = screen == "pause";
            }
            app.mouse_x = -2.2;
            app.left_held = false;
        }
        "results" | "replay" => {
            if let Some(c) = app.songs.get(sel).and_then(|s| s.charts.get(diff)) {
                let chart = Chart::load(&c.path)?;
                let mut play = Play::new(&chart);
                let mut seed = 0x1234_5678u64;
                for i in 0..300 {
                    seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    let u = ((seed >> 33) as f32 / u32::MAX as f32) - 0.5;
                    let ms = if i % 37 == 0 { 78.0 + u * 40.0 } else { 6.0 + u * 96.0 };
                    app.mouse_tap_errors.push(ms);
                    app.tablet_tap_errors.push(ms * 0.8 - 3.0);
                }
                play.score = 987_430;
                play.max_combo = 388;
                play.counts = [412, 18, 6];
                app.result_is_record = true;
                // Строка pp: без сети подставляем ответ сервера (как будто
                // запись уже ушла и пришёл ack) — для проверки вёрстки.
                // `DEEMO_SHOT_PP=pending` рисует состояние «ждём очередь».
                app.result_pp = if std::env::var("DEEMO_SHOT_PP").as_deref() == Ok("pending") {
                    screens::ResultPp::Pending
                } else {
                    screens::ResultPp::Ready {
                        pp: 172.6,
                        total_pp: Some(2760.6),
                        pp_tablet: Some(86.0),
                        pp_mouse: Some(86.6),
                        record: true,
                    }
                };
                // локальные оффсеты карты: рекомендации по этим тапам поверх
                // значений из data/offsets.json (для демо-шота файл создаётся
                // заранее). Глобальные настройки остаются из settings.json.
                let song_id = app.songs[sel].id.clone();
                let diff_name = c.diff.clone();
                app.play_key = (song_id.clone(), diff_name.clone());
                let mean = |v: &[f32]| -> Option<f32> {
                    if v.is_empty() {
                        None
                    } else {
                        Some(v.iter().sum::<f32>() / v.len() as f32)
                    }
                };
                let m_mean = mean(&app.mouse_tap_errors);
                let t_mean = mean(&app.tablet_tap_errors);
                app.result_map_mouse = app.map_offsets.recommend(
                    &song_id,
                    &diff_name,
                    crate::map_offsets::MOUSE,
                    m_mean,
                );
                app.result_map_tablet = app.map_offsets.recommend(
                    &song_id,
                    &diff_name,
                    crate::map_offsets::TABLET,
                    t_mean,
                );
                app.mode = Mode::Result { song: sel, diff, play };
            }
            if screen == "replay" {
                if let Some(s) = app.songs.get(sel) {
                    toast = Some(format!(
                        "Replay saved: data/replays/{}_hard.json",
                        s.id
                    ));
                }
            }
        }
        "settings" => {
            // без сети: проверку каталога для скриншота не запускаем
            app.open_settings_with(false);
            let dirs = crate::pack::installed().len();
            app.packs.total = Some(447);
            app.packs.installed = dirs;
            app.packs.phase = PacksPhase::Idle;
            match packs {
                Some("running") => {
                    app.packs.phase = PacksPhase::Running;
                    app.packs.prog = crate::pack_download::Progress {
                        done: 128,
                        skipped: 0,
                        total: 447,
                        bytes: 438_173_696,
                        current: "10 -ichizero-".into(),
                        ..Default::default()
                    };
                }
                Some("done") => {
                    app.packs.phase = PacksPhase::Done;
                    app.packs.prog = crate::pack_download::Progress {
                        done: 444,
                        skipped: 3,
                        total: 447,
                        bytes: 1_402_531_840,
                        failed: vec![(
                            "ice10".into(),
                            "download: connection reset by peer".into(),
                        )],
                        finished: true,
                        ..Default::default()
                    };
                }
                Some("cancelled") => {
                    app.packs.phase = PacksPhase::Cancelled;
                    app.packs.prog = crate::pack_download::Progress {
                        done: 125,
                        skipped: 3,
                        total: 447,
                        bytes: 402_653_184,
                        cancelled: true,
                        finished: true,
                        ..Default::default()
                    };
                }
                _ => {}
            }
            if demo {
                let logged = app.online.logged_in();
                if let Some(sc) = app.settings.as_mut() {
                    sc.server.text = "https://deemo.example.com:8443".into();
                    sc.server.end();
                    sc.nickname.text = "player one".into();
                    sc.nickname.end();
                    if packs.is_some() {
                        // загрузка/результат: подсвечиваем строку загрузки
                        sc.select(crate::settings_screen::Row::Download, logged);
                    } else {
                        sc.select(crate::settings_screen::Row::Nickname, logged);
                        sc.focus = Some(1);
                    }
                }
            } else if packs.is_some() {
                let logged = app.online.logged_in();
                if let Some(sc) = app.settings.as_mut() {
                    // --packs install — подсветить строку «Install pack from file…»
                    let row = if packs == Some("install") {
                        crate::settings_screen::Row::Install
                    } else {
                        crate::settings_screen::Row::Download
                    };
                    sc.select(row, logged);
                }
            }
        }
        "login" => {
            app.mode = Mode::Login {
                nickname: "piano_kid".into(),
                password: "deemo".into(),
                focus: 0,
                register: false,
                error: None,
            };
        }
        _ => {
            app.mode = Mode::Library {
                scroll: 0.0,
                // без чартов выбирать нечего — библиотека рисуется пустой
                selected: if app.songs.is_empty() { None } else { Some(entry) },
            };
            if screen == "library-menu" {
                app.open_dropdown = Some(0);
            }
        }
    }

    // headless: сразу открыть детали плея (кнопка watch — по фикстуре реплеев)
    if std::env::var("DEEMO_SHOT_DETAIL").is_ok() && !app.lb_records.is_empty() {
        let idx = std::env::var("DEEMO_SHOT_DETAIL_IDX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
            .min(app.lb_records.len() - 1);
        app.lb_sel = idx;
        app.lb_focus = true;
        app.open_play_detail(idx);
    } else if std::env::var("DEEMO_SHOT_LB_FOCUS").is_ok() && !app.lb_records.is_empty() {
        app.lb_sel = std::env::var("DEEMO_SHOT_DETAIL_IDX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
            .min(app.lb_records.len() - 1);
        app.lb_focus = true;
    }

    // полноразмерный арт выбранной песни (нужен для аспекта панели)
    let selected_song = match &app.mode {
        Mode::Library { selected, .. } => selected.map(|e| e.song),
        Mode::Result { song, .. } => Some(*song),
        _ => None,
    };
    if let Some(si) = selected_song {
        if let Some(path) = app.songs[si].cover.clone() {
            if let Ok(img) = image::open(&path) {
                let rgba = img.to_rgba8();
                let (iw, ih) = rgba.dimensions();
                let handle = renderer.add_ui_texture(&path, iw, ih, &rgba.into_raw());
                app.covers.insert(path, handle);
                app.full_dims.insert(si, (iw, ih));
            }
        }
    }
    app.layout();

    // демо-шот: превью в headless не играет, поэтому поле прогревается до
    // фиксированной позиции — фон показывает настоящую картину песни.
    // DEEMO_BG_T — время фона (по умолчанию 30), DEEMO_BG_SWAP — сложность,
    // на которую «переключаемся» без очистки уже летящих нот.
    if demo && (screen == "main" || screen.starts_with("library")) {
        let t: f32 = std::env::var("DEEMO_BG_T")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30.0);
        demo_bg_warm(&mut app, sel, diff, t);
        if let Some(swap) = std::env::var("DEEMO_BG_SWAP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
        {
            // держим уже летящие ноты: меняем только источник будущих
            app.bg_field.advance(0.5, t + 0.5);
            if let Some(src) = demo_bg_source(&app, sel, swap) {
                app.bg_field.set_source(src, t + 0.5);
            }
            app.bg_field.advance(0.5, t + 1.0);
            app.bg_t_demo = Some(t + 1.0);
            if let Mode::Library { selected: Some(e), .. } = &mut app.mode {
                e.diff = swap.min(app.songs[e.song].charts.len().saturating_sub(1));
            }
        }
    }

    // миниатюры видимых строк
    let scale = crate::theme::ui_scale(width as f32, height as f32);
    let cards = app.card_rects.clone();
    for (e, r) in cards {
        if r[1] + r[3] < screens::TOP_H * scale || r[1] > height as f32 {
            continue;
        }
        if let Some(path) = app.thumb_path(e.song) {
            if app.covers.contains_key(&path) {
                continue;
            }
            if let Ok(img) = image::open(&path) {
                let rgba = img.to_rgba8();
                let (iw, ih) = rgba.dimensions();
                let handle = renderer.add_ui_texture(&path, iw, ih, &rgba.into_raw());
                app.covers.insert(path, handle);
            }
        }
    }

    let mut ui: Vec<UiQuad> = Vec::new();
    let mut sprites = Vec::new();
    let mut screen_q = vec![
        ScreenQuad { tex: Tex::Bg, color: WHITE },
        ScreenQuad { tex: Tex::BgMask, color: [1.0, 1.0, 1.0, 0.8] },
    ];

    match &app.mode {
        Mode::Library { selected, .. } => {
            if app.settings.is_none() {
                let selected = *selected;
                app.draw_library(&mut ui, selected);
            }
        }
        Mode::Play { chart, play, song, diff, paused, .. } => {
            let t = chart
                .notes
                .first()
                .map(|n| n.time + 0.55)
                .unwrap_or(3.0);
            let mut scene = Scene::with_play(chart, app.speed, play);
            scene.guides = app.note_guides;
            let mvp = scene.mvp(width as f32 / height as f32);
            renderer.set_camera(&mvp);
            sprites = scene.sprites(t, width, height);
            screen_q[1].color = [1.0, 1.0, 1.0, scene.bg_mask_alpha(t)];
            // Screenshot-only fixture for the two-cursor RMB mapping. Normal
            // gameplay uses the same helper in redraw().
            if let Ok(x) = std::env::var("DEEMO_SHOT_MOUSE_X") {
                if let Ok(x) = x.parse::<f32>() {
                    app.mouse_x = crate::stage::note_x(x);
                }
            }
            let pen_x = std::env::var("DEEMO_SHOT_PEN_X")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .map(crate::stage::note_x);
            let rmb_mode = app.rmb_pen_press || std::env::var("DEEMO_SHOT_RMB_MODE").is_ok();
            let lmb = app.left_held || std::env::var("DEEMO_SHOT_LMB").is_ok();
            let rmb = app.right_held || std::env::var("DEEMO_SHOT_RMB").is_ok();
            let (mouse_press, pen_press) =
                cursor_marker_press(rmb_mode, app.mouse_x, pen_x, lmb, rmb, false);
            sprites.push(crate::stage::marker(
                app.mouse_x,
                [0.3, 0.8, 1.0, 1.0],
                mouse_press,
            ));
            if let Some(px) = pen_x {
                sprites.push(crate::stage::marker(
                    px,
                    [1.0, 0.6, 0.2, 1.0],
                    pen_press,
                ));
            }
            app.draw_hud(&mut ui, *song, *diff, play, t, *paused, None);
        }
        Mode::Result { song, diff, play } => {
            app.draw_result(&mut ui, *song, *diff, play);
        }
        Mode::Login { .. } => {
            app.draw_login(&mut ui);
        }
    }

    if app.settings.is_some() {
        app.draw_settings(&mut ui);
    }
    if let Some(msg) = &toast {
        let ctx = screens::Ctx {
            text: app.text.as_ref().expect("font"),
            white: app.white_ui,
            fade_x: app.fade_x_ui,
            fade_y: app.fade_y_ui,
            w: width as f32,
            h: height as f32,
            s: crate::theme::ui_scale(width as f32, height as f32),
        };
        screens::draw_toast(&ctx, &mut ui, msg, 1.0);
    }

    // глифы, впервые нарисованные на этом экране, — в атлас до рендера
    if let Some(t) = app.text.as_mut() {
        t.flush(&mut renderer);
    }

    let tex = renderer.offscreen(width, height, &sprites, &screen_q, &ui);
    renderer.save_png(&tex, width, height, out)?;
    println!(
        "screen screenshot: {out} ({width}x{height}, screen={screen}, ui quads: {})",
        ui.len()
    );
    Ok(())
}

fn in_rect(x: f32, y: f32, r: [f32; 4]) -> bool {
    x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3]
}

/// Следующая строка-чарт при Up/Down. Вынесено из `App::list_key_move`,
/// чтобы границы списка и старт без выбора были проверяемы тестом.
pub fn step_entry(entries: &[Entry], cur: Option<Entry>, down: bool) -> Option<Entry> {
    if entries.is_empty() {
        return None;
    }
    match cur.and_then(|c| entries.iter().position(|&e| e == c)) {
        Some(i) if down => Some(entries[(i + 1).min(entries.len() - 1)]),
        Some(i) if !down => Some(entries[i.saturating_sub(1)]),
        Some(i) => Some(entries[i]),
        None => Some(entries[0]),
    }
}

/// Прокрутка по позиции курсора при перетаскивании ползунка.
///
/// Вынесено из `App::sbar_drag_to` как чистая функция: попадание мыши в
/// 2-пиксельную полоску иначе не проверить тестом.
fn scroll_for_drag(y: f32, track: [f32; 4], thumb: [f32; 4], grab: f32, max_scroll: f32) -> f32 {
    let free = (track[3] - thumb[3]).max(1.0);
    let frac = ((y - track[1] - grab) / free).clamp(0.0, 1.0);
    frac * max_scroll.max(0.0)
}

/// Сигнатура папки data/imported (имена подпапок) — для авто-подхвата новых.
fn imported_sig() -> u64 {
    let mut h: u64 = 0;
    if let Ok(rd) = std::fs::read_dir(crate::pack::IMPORT_DIR) {
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .collect();
        names.sort();
        for n in names {
            for b in n.bytes() {
                h = h.wrapping_mul(131).wrapping_add(b as u64);
            }
            h = h.wrapping_mul(131).wrapping_add(7);
        }
    }
    h
}

pub fn run() -> Result<()> {
    // Wayland-бэкенд winit не умеет drag & drop (файл, брошенный на окно, не
    // доходит до приложения). С DEEMO_X11=1 идём через XWayland, где работает.
    let forced = std::env::var("DEEMO_X11").map(|v| v != "0" && !v.is_empty()).unwrap_or(false);
    let mut builder = EventLoop::builder();
    #[cfg(all(unix, not(target_os = "macos")))]
    if forced {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }
    let event_loop = builder.build()?;
    if std::env::var("DEEMO_BACKEND_ONLY").is_ok() {
        let backend = if cfg!(windows) {
            "windows (winit)"
        } else if forced {
            "x11 (XWayland)"
        } else {
            "wayland/native"
        };
        println!("backend: {backend}");
        return Ok(());
    }
    if forced {
        println!("backend: x11 (XWayland) — drag & drop available");
    }
    event_loop.set_control_flow(ControlFlow::Poll);
    // двойной клик по .deemopack: первый позиционный аргумент ставим тем же
    // путём, что drag & drop / инбокс; результат — обычный тост библиотеки
    let args: Vec<String> = std::env::args().collect();
    let mut app = App::new()?;
    app.startup_pack = pack_path_from_args(&args);
    // если задан DEEMO_CHART — сразу игра (для тестов)
    if let Ok(path) = std::env::var("DEEMO_CHART") {
        if let Ok(chart) = Chart::load(&path) {
            let mut play = Play::new(&chart);
            play.set_x_tolerance(app.x_tolerance);
            let clock = Clock::new(active_ahead_time(app.speed) + 0.5, app.audio_offset_ms / 1000.0);
            let s0 = &app.songs[0];
            let diff0 = s0.charts.first().map(|c| c.diff.clone()).unwrap_or_default();
            app.replay = Some(crate::replay::Replay::new(
                &s0.id, &diff0, &s0.title, &chart,
                app.speed, app.audio_offset_ms, app.tablet_zoom, app.x_tolerance,
            ));
            if let Some(r) = &mut app.replay {
                r.set_input_offsets(app.mouse_offset_ms, app.tablet_offset_ms);
            }
            app.mode = Mode::Play { song: 0, diff: 0, chart, play, clock, music_on: false, paused: false, driver: None };
        }
    }
    let _ = &app;
    // debug: экран результата с синтетическими таймингами (для скриншота)
    if std::env::var("DEEMO_FAKE_RESULT").is_ok() {
        if let Ok(chart) = Chart::load("data/charts/12thchance/hard.json") {
            let mut play = Play::new(&chart);
            let mut seed = 12345u64;
            for _ in 0..300 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let u = (seed >> 33) as f32 / u32::MAX as f32;
                play.errors.push(0.035 + (u - 0.5) * 0.16);
            }
            play.counts = [250, 40, 10];
            play.score = 950000;
            play.max_combo = 200;
            app.result_is_record = true;
            app.mode = Mode::Result { song: 0, diff: 2, play };
        }
    }
    // debug: открыть экран входа (для headless-теста)
    if std::env::var("DEEMO_LOGIN").is_ok() {
        app.mode = Mode::Login {
            nickname: std::env::var("DEEMO_LOGIN_NICK").unwrap_or_default(),
            password: std::env::var("DEEMO_LOGIN_PW").unwrap_or_default(),
            focus: 0,
            register: false,
            error: None,
        };
    }
    // debug/демо: сразу открыть экран настроек (DEEMO_SCREEN=settings)
    if std::env::var("DEEMO_SCREEN").map(|v| v == "settings").unwrap_or(false) {
        app.open_settings();
        if matches!(app.mode, Mode::Play { .. }) {
            app.set_paused(true);
        }
    }
    if std::env::var("DEEMO_LB_ONLINE").is_ok() {
        app.lb_online = true;
    }
    event_loop.run_app(&mut app)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::{queue_toast, tap_edge};

    /// Лидерборд достижим и покидаем только клавиатурой: Tab входит и
    /// выходит, стрелки/Enter работают лишь в фокусе, Esc/Backspace снимают
    /// фокус. Без записей список не перехватывает ни одну клавишу.
    #[test]
    fn leaderboard_is_keyboard_only_and_not_a_trap() {
        use super::{leaderboard_action, LeaderboardKey};
        use winit::keyboard::{Key, NamedKey};
        use NamedKey as N;
        let key = |k: NamedKey| Key::Named(k);

        assert_eq!(
            leaderboard_action(&key(N::Tab), false, 3),
            Some(LeaderboardKey::ToggleFocus)
        );
        assert_eq!(
            leaderboard_action(&key(N::Tab), true, 3),
            Some(LeaderboardKey::ToggleFocus),
            "Tab leaves the list too"
        );
        assert_eq!(
            leaderboard_action(&key(N::ArrowDown), true, 3),
            Some(LeaderboardKey::Down)
        );
        assert_eq!(
            leaderboard_action(&key(N::ArrowUp), true, 3),
            Some(LeaderboardKey::Up)
        );
        assert_eq!(
            leaderboard_action(&key(N::Enter), true, 3),
            Some(LeaderboardKey::Open)
        );
        assert_eq!(
            leaderboard_action(&key(N::Escape), true, 3),
            Some(LeaderboardKey::Back)
        );
        assert_eq!(
            leaderboard_action(&key(N::Backspace), true, 3),
            Some(LeaderboardKey::Back)
        );
        // вне фокуса стрелки/Enter остаются прежними хоткеями и поиском
        for k in [N::ArrowUp, N::ArrowDown, N::Enter, N::Escape] {
            assert_eq!(leaderboard_action(&key(k), false, 3), None, "{k:?}");
        }
        // пустой список ничего не съедает: Tab/Space работают как раньше
        for k in [N::Tab, N::ArrowUp, N::ArrowDown, N::Enter, N::Space] {
            assert_eq!(leaderboard_action(&key(k), false, 0), None, "{k:?}");
        }
    }

    /// Hide cursor в absolute-режиме обязан скрывать системный курсор
    /// (иначе он «двоится» с пером); без опции — прежнее поведение.
    #[test]
    fn line_cursor_hides_the_system_cursor_in_both_modes() {
        use super::hide_system_cursor;
        assert!(hide_system_cursor(true, true, true), "absolute + line");
        assert!(hide_system_cursor(true, true, false), "relative + line");
        assert!(hide_system_cursor(true, false, false), "relative without line");
        assert!(!hide_system_cursor(true, false, true), "absolute without line");
        assert!(!hide_system_cursor(false, true, true), "menu");
    }

    /// LMB/RMB в RMB-режиме адресуют левый/правый курсор по X; при равном X
    /// левым считается мышь; без пера оба — мышь.
    #[test]
    fn cursor_sides_follow_positions() {
        use super::{cursor_sides, PressDevice};
        let (left, right) = cursor_sides(-2.0, Some(1.0));
        assert_eq!((left.device, left.x), (PressDevice::Mouse, -2.0));
        assert_eq!((right.device, right.x), (PressDevice::Pen, 1.0));
        assert!(!left.tablet_offset && right.tablet_offset);
        let (left, right) = cursor_sides(1.0, Some(-1.0));
        assert_eq!((left.device, left.x), (PressDevice::Pen, -1.0));
        assert_eq!((right.device, right.x), (PressDevice::Mouse, 1.0));
        // equal X: mouse is left
        let (left, right) = cursor_sides(0.0, Some(0.0));
        assert_eq!((left.device, left.x), (PressDevice::Mouse, 0.0));
        assert_eq!((right.device, right.x), (PressDevice::Pen, 0.0));
        // no pen: both mouse
        let (left, right) = cursor_sides(-2.0, None);
        assert_eq!((left.device, left.x), (PressDevice::Mouse, -2.0));
        assert_eq!((right.device, right.x), (PressDevice::Mouse, -2.0));
    }

    /// В RMB-режиме LMB судит левый курсор: если перо левее, тап идёт по
    /// X пера и засчитывается планшетной руке.
    #[test]
    fn lmb_taps_the_pen_when_the_pen_is_the_left_cursor() {
        use super::{apply_cursor_tap, cursor_sides, PressDevice};
        use crate::chart::{Chart, Note, NoteKind};
        use crate::play::{Hands, Judge};
        let chart = Chart {
            speed: 6.0,
            notes: vec![Note {
                time: 1.0,
                pos: -1.0,
                size: 1.0,
                speed: 1.0,
                kind: NoteKind::Normal,
                hold: 0.0,
                sounds: vec![],
            }],
            links: vec![],
        };
        let mut play = crate::play::Play::new(&chart);
        let pen_x = crate::stage::note_x(-1.0);
        let (left, _) = cursor_sides(2.0, Some(pen_x));
        assert_eq!(left.device, PressDevice::Pen);
        let mut hands = Hands::default();
        let j = apply_cursor_tap(&mut play, &chart, 1.0, left, &mut hands);
        assert_eq!(j, Some(Judge::Perfect));
        assert_eq!(hands.tablet.perfect, 1);
        assert_eq!(hands.mouse.total(), 0);
    }

    /// Маркеры в RMB-режиме отражают кнопки, а не tip-press пера:
    /// касание пера само press не даёт; LMB жмёт левый маркер, RMB — правый.
    #[test]
    fn cursor_marker_press_follows_buttons_in_rmb_mode() {
        use super::cursor_marker_press;
        // Pen left, mouse right.
        assert_eq!(
            cursor_marker_press(true, 2.0, Some(-1.0), false, false, true),
            (false, false),
            "pen tip alone must not press a marker"
        );
        assert_eq!(
            cursor_marker_press(true, 2.0, Some(-1.0), true, false, true),
            (false, true),
            "LMB presses the left (pen) marker"
        );
        assert_eq!(
            cursor_marker_press(true, 2.0, Some(-1.0), false, true, false),
            (true, false),
            "RMB presses the right (mouse) marker"
        );
        // Pen right, mouse left: mapping follows positions.
        assert_eq!(
            cursor_marker_press(true, -2.0, Some(1.0), true, false, false),
            (true, false)
        );
        assert_eq!(
            cursor_marker_press(true, -2.0, Some(1.0), false, true, false),
            (false, true)
        );
        // No pen: either button presses the mouse marker.
        assert_eq!(
            cursor_marker_press(true, -2.0, None, true, false, false),
            (true, false)
        );
        assert_eq!(
            cursor_marker_press(true, -2.0, None, false, true, false),
            (true, false)
        );
        // Normal mode keeps old semantics.
        assert_eq!(
            cursor_marker_press(false, 0.0, Some(1.0), true, false, false),
            (true, false)
        );
        assert_eq!(
            cursor_marker_press(false, 0.0, Some(1.0), false, false, true),
            (false, true)
        );
    }

    /// Детали открываются по строке и находят реплей строго по нику из
    /// публичного списка; без реплея кнопка выключена с причиной.
    #[test]
    fn play_detail_merges_the_replay_by_nick() {
        use super::detail_for_record;
        use crate::online::{OnlineRecord, ReplayEntry};
        let rec = OnlineRecord {
            nickname: "piano_kid".into(),
            song: "12thchance".into(),
            diff: "hard".into(),
            score: 987_430,
            accuracy: 96.56,
            accuracy_real: Some(88.5),
            pp: Some(172.6),
            pp_tablet: Some(86.0),
            pp_mouse: Some(86.6),
            hands: true,
            max_combo: 388,
            perfect: 412,
            good: 18,
            miss: 6,
            rank: "S".into(),
            full_combo: false,
            speed: 4.0,
            timestamp: 1_794_940_377,
        };
        let replay = ReplayEntry {
            nick: "piano_kid".into(),
            score: 987_430,
            pp: Some(172.6),
            accuracy: 88.5,
            timestamp: 1_794_940_377,
            notes: 436,
        };
        let d = detail_for_record(&rec, &[replay.clone()], Some(9.6));
        assert!(d.can_watch());
        assert_eq!(d.replay.as_ref().map(|r| r.nick.as_str()), Some("piano_kid"));
        assert_eq!(d.star, Some(9.6));
        // ряд есть, реплея нет — детали всё равно открываются, watch выключен
        let d = detail_for_record(&rec, &[], None);
        assert!(!d.can_watch());
        assert!(d.watch_reason().contains("No replay"));
        // чужой ник не подходит
        let d = detail_for_record(&rec, &[ReplayEntry { nick: "other".into(), ..replay }], None);
        assert!(!d.can_watch());
    }

    /// Up/Down идут по строкам-чартам, включая разные сложности одной песни,
    /// и не выходят за края.
    #[test]
    fn entry_navigation_walks_charts() {
        use super::step_entry;
        use crate::library::Entry;
        let list = [Entry::new(0, 0), Entry::new(0, 1), Entry::new(1, 0)];
        assert_eq!(step_entry(&list, None, true), Some(list[0]));
        assert_eq!(step_entry(&list, Some(list[0]), true), Some(list[1]));
        assert_eq!(step_entry(&list, Some(list[1]), true), Some(list[2]));
        assert_eq!(step_entry(&list, Some(list[2]), true), Some(list[2]));
        assert_eq!(step_entry(&list, Some(list[2]), false), Some(list[1]));
        assert_eq!(step_entry(&list, Some(list[0]), false), Some(list[0]));
        assert_eq!(step_entry(&[], None, true), None);
    }

    /// Двойной клик: первый позиционный существующий `.deemopack`/`.zip`
    /// импортируется, флаги и чужие пути молча игнорируются.
    #[test]
    fn pack_argument_is_the_first_existing_pack_file() {
        use super::{file_uri_to_path, pack_path_from_args};
        let dir = std::env::temp_dir().join(format!("deemo-cli-arg-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let pack = dir.join("test pack.deemopack");
        let zip = dir.join("bundle.zip");
        let txt = dir.join("notes.txt");
        std::fs::write(&pack, b"x").unwrap();
        std::fs::write(&zip, b"x").unwrap();
        std::fs::write(&txt, b"x").unwrap();
        let args = |tail: &[&str]| {
            let mut v = vec!["deemo".to_string()];
            v.extend(tail.iter().map(|s| s.to_string()));
            v
        };
        assert_eq!(pack_path_from_args(&args(&[])), None);
        assert_eq!(
            pack_path_from_args(&args(&["--x11", pack.to_str().unwrap()])),
            Some(pack.clone())
        );
        // первый позиционный — не пакет: остальные не ищем
        assert_eq!(
            pack_path_from_args(&args(&[txt.to_str().unwrap(), pack.to_str().unwrap()])),
            None
        );
        assert_eq!(pack_path_from_args(&args(&["./does-not-exist.deemopack"])), None);
        assert_eq!(
            pack_path_from_args(&args(&[zip.to_str().unwrap()])),
            Some(zip.clone())
        );
        // file:// URI с percent-escape (так его передают файловые менеджеры)
        let uri = format!("file://{}", pack.to_str().unwrap().replace(' ', "%20"));
        assert_eq!(pack_path_from_args(&args(&[&uri])), Some(pack.clone()));
        assert_eq!(
            file_uri_to_path("file:///tmp/a%20b.deemopack").as_deref(),
            Some(std::path::Path::new("/tmp/a b.deemopack"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tablet_tap_fires_once_per_press() {
        let mut prev = false;
        // hover → press: ровно один тап
        assert!(tap_edge(&mut prev, true), "press must fire");
        assert!(!tap_edge(&mut prev, true), "holding must not repeat the tap");
        assert!(!tap_edge(&mut prev, true), "still holding");
        // release: тишина
        assert!(!tap_edge(&mut prev, false), "release must not fire");
        // press again: новый тап
        assert!(tap_edge(&mut prev, true), "second press must fire");
        assert!(!tap_edge(&mut prev, false));
        assert!(tap_edge(&mut prev, true), "third press must fire");
    }

    #[test]
    fn tablet_tap_edges_are_independent_per_kind() {
        let mut gaomon = false;
        let mut parblo = false;
        // Gaomon нажал — Parblo свой фронт не потерял
        assert!(tap_edge(&mut gaomon, true));
        assert!(tap_edge(&mut parblo, true), "the second tablet still gets its tap");
        assert!(!tap_edge(&mut gaomon, true));
        assert!(!tap_edge(&mut parblo, true));
        assert!(!tap_edge(&mut gaomon, false));
        assert!(!tap_edge(&mut parblo, false));
        assert!(tap_edge(&mut gaomon, true));
    }

    #[test]
    fn queue_toasts_are_short_and_plain() {
        // успешная отправка сразу после результата
        assert_eq!(queue_toast(true, 1, false), Some("Score uploaded"));
        // не вышло — запись осталась, игрок видит это
        assert_eq!(
            queue_toast(true, 0, true),
            Some("Score queued — will retry")
        );
        // фон добрал ранее отложенный рекорд
        assert_eq!(
            queue_toast(false, 1, false),
            Some("Queued score uploaded")
        );
        // фоновый проход без отправок — молчим
        assert_eq!(queue_toast(false, 0, false), None);
        for msg in [
            "Score uploaded",
            "Score queued — will retry",
            "Queued score uploaded",
        ] {
            assert!(!msg.contains('!'), "Deemo tone: no exclamation marks");
            assert!(!msg.contains('\n'), "one short line");
        }
    }

    /// Ответ сервера по загруженной записи обновляет строку pp на результате,
    /// а чужая запись из того же прохода очереди — нет.
    #[test]
    fn pp_ack_updates_the_result_line_of_that_play() {
        use crate::online_queue::{PendingScore, ScoreAck, UploadedScore};
        let body = serde_json::json!({
            "song": "12thchance", "diff": "hard", "score": 987_430,
            "accuracy": 96.56, "achievement": 98.74, "timestamp": 7,
        });
        let mine = PendingScore::new("http://srv", "tk", body.as_object().unwrap().clone());
        let other = {
            let b = serde_json::json!({
                "song": "old", "diff": "hard", "score": 1,
                "accuracy": 1.0, "timestamp": 1,
            });
            PendingScore::new("http://srv", "tk", b.as_object().unwrap().clone())
        };
        let ack = ScoreAck {
            record: true,
            best: 987_430,
            pp: 172.6,
            total_pp: Some(2760.6),
            pp_tablet: Some(86.0),
            pp_mouse: Some(86.6),
        };
        let acks = vec![
            UploadedScore { rec: other.clone(), ack: Some(ScoreAck { pp: 4.0, ..ack.clone() }) },
            UploadedScore { rec: mine.clone(), ack: Some(ack.clone()) },
        ];
        let pp = super::result_pp_from_ack(super::ack_for_result(&mine, &acks).expect("ack for my play"));
        assert!(super::ack_for_result(&other, &acks).is_some());
        // строка на экране: серверные pp, сумма аккаунта и разбивка по рукам
        let line = crate::screens::pp_line(pp).expect("ready line");
        for part in ["172.6", "2760.6", "86.0", "86.6"] {
            assert!(line.contains(part), "server value {part} missing: {line}");
        }
        // пока ответа нет — PENDING без числа, а без входа строки нет
        let pending = crate::screens::pp_line(crate::screens::ResultPp::Pending).expect("pending");
        assert!(pending.contains("PENDING"));
        assert!(!pending.chars().any(|c| c.is_ascii_digit()), "{pending}");
        assert_eq!(crate::screens::pp_line(crate::screens::ResultPp::Hidden), None);
        // запись, которой нет в ответах прохода, ничего не обновляет
        let absent = PendingScore::new("http://srv", "tk", serde_json::Map::new());
        assert!(super::ack_for_result(&absent, &acks).is_none());
        // тост загрузки показывает прибавку pp
        assert_eq!(
            super::upload_toast(Some(&ack)),
            format!("Score uploaded · {}", crate::pp::ack_tag(172.6))
        );
        assert_eq!(super::upload_toast(None), "Score uploaded");
    }
}

#[cfg(test)]
mod scrollbar_tests {
    use super::scroll_for_drag;

    const TRACK: [f32; 4] = [500.0, 70.0, 2.0, 500.0];
    const THUMB_H: f32 = 50.0;

    fn thumb_at(ty: f32) -> [f32; 4] {
        [500.0, ty, 2.0, THUMB_H]
    }

    #[test]
    fn drag_maps_track_to_scroll_range() {
        let max = 3000.0;
        // верх дорожки (захват за середину ползунка) -> самый верх списка
        assert_eq!(scroll_for_drag(70.0, TRACK, thumb_at(70.0), THUMB_H * 0.5, max), 0.0);
        // низ дорожки -> самый низ списка
        let bottom = 70.0 + 500.0;
        assert!((scroll_for_drag(bottom, TRACK, thumb_at(70.0 + 500.0 - THUMB_H), THUMB_H * 0.5, max) - max).abs() < 1.0);
        // середина -> примерно половина
        let mid = 70.0 + 250.0;
        let s = scroll_for_drag(mid, TRACK, thumb_at(70.0 + 250.0 - THUMB_H * 0.5), THUMB_H * 0.5, max);
        assert!((s - max * 0.5).abs() < max * 0.02, "mid={s}");
    }

    #[test]
    fn drag_is_clamped_outside_the_track() {
        let max = 1000.0;
        assert_eq!(scroll_for_drag(-500.0, TRACK, thumb_at(70.0), 0.0, max), 0.0);
        assert!((scroll_for_drag(99999.0, TRACK, thumb_at(70.0), 0.0, max) - max).abs() < 1e-3);
        // без прокрутки (max=0) ничего не двигаем
        assert_eq!(scroll_for_drag(300.0, TRACK, thumb_at(70.0), 0.0, 0.0), 0.0);
    }

    #[test]
    fn grabbing_mid_thumb_keeps_relative_position() {
        let max = 500.0;
        // схватили за середину ползунка в его нынешнем положении -> прокрутка не прыгает
        let ty = 70.0 + 200.0;
        let s = scroll_for_drag(ty + THUMB_H * 0.5, TRACK, thumb_at(ty), THUMB_H * 0.5, max);
        let expected = max * (200.0 / (TRACK[3] - THUMB_H));
        assert!((s - expected).abs() < 1.0, "{s} vs {expected}");
    }
}
