//! Прямое чтение планшетов по hidraw (без ML). Gaomon S620 + Parblo Ninos N4.
//! Форматы репортов — из mread (driver/tablets). Берём X (ширина) и давление, Y игнорируем.
//! Параллельно пишем сырой лог для калибровки нажатий.
//!
//! Данные калибровки: hover = давление 0 у обоих; press = давление > порога.
//! Gaomon X≈26456, P 0..8191; Parblo X≈21598, P 0..4358 (hover-флаг byte1 0xA0/0xA1).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Сырой X правого края Gaomon S620 (ширина, 0..26455). Зашито в игру:
/// калибровка в настройках не нужна и не показывается.
pub const GAOMON_X_MAX: f32 = 26456.0;
/// Сырой X правого края Parblo Ninos N4 (ширина). Тоже зашито.
pub const PARBLO_X_MAX: f32 = 21598.0;

#[derive(Clone, Copy, Default)]
pub struct TabletState {
    pub present: bool,
    pub x_raw: i32,
    pub pressure: i32,
    pub hover: bool,
    /// нажим стилусом (давление выше порога)
    pub press: bool,
    /// нормализованный X 0..1 по ширине конкретного планшета
    pub x_norm: f32,
}

/// Сырой репорт планшета с wall-time (для реплея/отладки ввода).
#[derive(Clone, Copy)]
pub struct RawTabletEvent {
    pub wall: Instant,
    pub kind: &'static str,
    pub x_raw: i32,
    pub pressure: i32,
    pub hover: bool,
    pub press: bool,
    pub x_norm: f32,
    pub raw: [u8; 16],
    pub raw_len: u8,
}

pub struct Tablets {
    pub gaomon: Arc<Mutex<TabletState>>,
    pub parblo: Arc<Mutex<TabletState>>,
    /// самый свежий репорт: (какой планшет, когда). Gaomon и Parblo равноправны:
    /// игра берёт активный планшет, а не жёстко Parblo.
    active: Arc<Mutex<Option<(&'static str, Instant)>>>,
    /// Момент последнего отчёта любого планшета — нужен, чтобы отличить
    /// эмулированную мышку (перо) от настоящей.
    last_report: Arc<Mutex<Option<Instant>>>,
    /// Момент последнего нажатия пером.
    last_press: Arc<Mutex<Option<Instant>>>,
    log: Arc<Mutex<File>>,
    events: Arc<Mutex<Vec<RawTabletEvent>>>,
    start: Instant,
}

/// Сколько живёт «активность» планшета без новых репортов.
pub const ACTIVE_TTL: std::time::Duration = std::time::Duration::from_secs(2);
/// Свежесть нажатия пера, при которой его эмулированная мышь игнорируется.
pub const PEN_PRESS_GRACE: std::time::Duration = std::time::Duration::from_millis(80);
/// Свежесть любого репорта пера для проверки близости курсора.
pub const PEN_REPORT_GRACE: std::time::Duration = std::time::Duration::from_millis(40);
/// Допуск совпадения X пера и системного курсора (доля ширины окна).
pub const PEN_X_TOLERANCE: f32 = 0.02;

/// Свежий ли отчёт: `last` не старше `within` на момент `now`.
///
/// Вынесено отдельно от мьютексов, чтобы окно свежести можно было проверить
/// тестом без реального hidraw-устройства.
pub fn report_is_fresh(
    last: Option<Instant>,
    within: std::time::Duration,
    now: Instant,
) -> bool {
    last.is_some_and(|t| now.saturating_duration_since(t) <= within)
}

/// Совпадает ли эмулированный пером курсор с позицией пера.
///
/// `pen_x_norm` — X пера, нормализованный в 0..1 по ширине планшета;
/// `cursor_x` — X системного курсора в пикселях окна.
pub fn pen_matches_cursor(pen_x_norm: f32, cursor_x: f32, width: f32, tol: f32) -> bool {
    if !pen_x_norm.is_finite()
        || !cursor_x.is_finite()
        || !width.is_finite()
        || width <= 0.0
        || !tol.is_finite()
        || tol < 0.0
    {
        return false;
    }
    (pen_x_norm * width - cursor_x).abs() <= tol * width
}

/// Чистое решение «это событие мыши сгенерировало перо, а не настоящая мышь».
///
/// `press_recently` / `pen_down` — факты из [`Tablets`], `report` — уже
/// отфильтрованное по свежести состояние пера. Отдельная функция нужна, чтобы
/// проверить все ветки без GPU/окна/планшета.
/// Эмулирует ли перо это мышиное событие.
///
/// Эмулированное событие всегда приходит в координате пера, поэтому глушим
/// только когда курсор в этой точке. Иначе страдает вторая рука: у пера и
/// мыши два независимых курсора, и мышь должна работать, даже пока перо
/// нажато.
pub fn pen_emulates_mouse(
    press_recently: bool,
    pen_down: bool,
    report: Option<TabletState>,
    cursor_x: f32,
    width: f32,
) -> bool {
    if !(press_recently || pen_down) {
        return false;
    }
    report.is_some_and(|st| {
        st.present && pen_matches_cursor(st.x_norm, cursor_x, width, PEN_X_TOLERANCE)
    })
}

fn vidpid(node: &str) -> (String, String) {
    let base = node.rsplit('/').next().unwrap_or("");
    let mut rp = std::fs::canonicalize(format!("/sys/class/hidraw/{base}/device")).ok();
    for _ in 0..8 {
        let Some(path) = &rp else { break };
        let v = path.join("idVendor");
        let p = path.join("idProduct");
        if v.exists() && p.exists() {
            let read = |f: &std::path::Path| {
                std::fs::read_to_string(f)
                    .map(|s| s.trim().to_lowercase())
                    .unwrap_or_default()
            };
            return (read(&v), read(&p));
        }
        rp = path.parent().map(|p| p.to_path_buf());
    }
    (String::new(), String::new())
}

fn find_nodes(vid: &str, pid: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/class/hidraw") {
        for e in rd.flatten() {
            let node = format!("/dev/{}", e.file_name().to_string_lossy());
            let (v, p) = vidpid(&node);
            if v == vid && p == pid {
                out.push(node);
            }
        }
    }
    out.sort();
    out
}

impl Tablets {
    pub fn new(zoom: f32, p_threshold: i32) -> Tablets {
        let _ = std::fs::create_dir_all(".work");
        let log = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(".work/tablet_ingame.jsonl")
            .expect("tablet log");
        let t = Tablets {
            gaomon: Arc::new(Mutex::new(TabletState::default())),
            parblo: Arc::new(Mutex::new(TabletState::default())),
            active: Arc::new(Mutex::new(None)),
            last_report: Arc::new(Mutex::new(None)),
            last_press: Arc::new(Mutex::new(None)),
            log: Arc::new(Mutex::new(log)),
            events: Arc::new(Mutex::new(Vec::new())),
            start: Instant::now(),
        };
        let zoom = zoom.max(0.1);
        // порог нажатия Parblo: лёгкие тапы дают p=1..5, поэтому порог низкий (p>0)
        let pt = p_threshold.max(0);
        for node in find_nodes("256c", "006d") {
            t.spawn(node, "gaomon", t.gaomon.clone(), GAOMON_X_MAX, 40, zoom);
        }
        for node in find_nodes("0483", "a019") {
            t.spawn(node, "parblo", t.parblo.clone(), PARBLO_X_MAX, pt, zoom);
        }
        t
    }

    fn spawn(
        &self,
        node: String,
        kind: &'static str,
        state: Arc<Mutex<TabletState>>,
        x_max: f32,
        press_threshold: i32,
        zoom: f32,
    ) {
        let log = self.log.clone();
        let events = self.events.clone();
        let last_report = self.last_report.clone();
        let last_press = self.last_press.clone();
        let active = self.active.clone();
        let start = self.start;
        std::thread::spawn(move || {
            let mut f = match File::open(&node) {
                Ok(f) => f,
                Err(_) => return,
            };
            let mut buf = [0u8; 64];
            loop {
                let n = match f.read(&mut buf) {
                    Ok(n) if n > 0 => n,
                    _ => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    }
                };
                let b = &buf[..n];
                let (x, p, hover) = match kind {
                    "gaomon" if n >= 8 => {
                        let x = b[2] as i32 | ((b[3] as i32) << 8);
                        let p = b[6] as i32 | ((b[7] as i32) << 8);
                        (x, p, p == 0)
                    }
                    "parblo" if n == 10 && (b[1] == 0xA0 || b[1] == 0xA1) => {
                        let x = b[2] as i32 | ((b[3] as i32) << 8);
                        let p = b[6] as i32 | ((b[7] as i32) << 8);
                        (x, p, b[1] == 0xA0)
                    }
                    _ => continue,
                };
                // зона уменьшена в zoom раз, центр сохранён
                let xn = (((x as f32 / x_max) - 0.5) * zoom + 0.5).clamp(0.0, 1.0);
                let press = !hover && p > press_threshold;
                if let Ok(mut g) = last_report.lock() {
                    *g = Some(Instant::now());
                }
                if press {
                    if let Ok(mut g) = last_press.lock() {
                        *g = Some(Instant::now());
                    }
                }
                {
                    let mut s = state.lock().unwrap();
                    *s = TabletState {
                        present: true,
                        x_raw: x,
                        pressure: p,
                        hover,
                        press,
                        x_norm: xn,
                    };
                }
                // какой планшет говорил последним (для маркера и слайдов)
                if let Ok(mut a) = active.lock() {
                    *a = Some((kind, Instant::now()));
                }
                // сырой репорт в очередь реплея (с wall-time)
                {
                    let mut raw = [0u8; 16];
                    let rl = n.min(16);
                    raw[..rl].copy_from_slice(&b[..rl]);
                    let ev = RawTabletEvent {
                        wall: Instant::now(),
                        kind,
                        x_raw: x,
                        pressure: p,
                        hover,
                        press,
                        x_norm: xn,
                        raw,
                        raw_len: rl as u8,
                    };
                    let mut q = events.lock().unwrap();
                    if q.len() > 8192 {
                        let excess = q.len() - 8192;
                        q.drain(..excess);
                    }
                    q.push(ev);
                }
                if let Ok(mut lf) = log.lock() {
                    let t = start.elapsed().as_secs_f32();
                    let raw: String = b.iter().map(|x| format!("{x:02x}")).collect();
                    let _ = writeln!(
                        lf,
                        "{{\"t\":{t:.4},\"kind\":\"{kind}\",\"x\":{x},\"p\":{p},\"hover\":{hover},\"press\":{press},\"raw\":\"{raw}\"}}"
                    );
                }
            }
        });
    }

    pub fn gaomon(&self) -> TabletState {
        *self.gaomon.lock().unwrap()
    }
    pub fn parblo(&self) -> TabletState {
        *self.parblo.lock().unwrap()
    }
    /// Самый свежий планшет, если он отчитывался не позже [`ACTIVE_TTL`].
    pub fn active(&self) -> Option<&'static str> {
        let guard = self.active.lock().ok()?;
        match *guard {
            Some((kind, at)) if at.elapsed() <= ACTIVE_TTL => Some(kind),
            _ => None,
        }
    }
    /// Состояние конкретного планшета по имени репорта.
    pub fn state_of(&self, kind: &str) -> TabletState {
        match kind {
            "gaomon" => self.gaomon(),
            "parblo" => self.parblo(),
            _ => TabletState::default(),
        }
    }
    /// Состояние активного планшета (если он реально что-то присылал).
    pub fn active_state(&self) -> Option<TabletState> {
        let kind = self.active()?;
        let state = self.state_of(kind);
        if state.present {
            Some(state)
        } else {
            None
        }
    }
    /// Был ли отчёт пера не позже `within` назад.
    pub fn pen_report_within(&self, within: std::time::Duration) -> Option<TabletState> {
        let last = *self.last_report.lock().ok()?;
        if !report_is_fresh(last, within, Instant::now()) {
            return None;
        }
        self.active_state()
    }

    /// Нажимало ли перо не позже `within` назад.
    pub fn pen_pressed_within(&self, within: std::time::Duration) -> bool {
        let last = self.last_press.lock().ok().and_then(|g| *g);
        report_is_fresh(last, within, Instant::now())
    }

    /// Нажато ли перо прямо сейчас (на активном планшете).
    pub fn pen_down(&self) -> bool {
        self.active_state().map(|s| s.press).unwrap_or(false)
    }

    /// Забрать накопленные сырые репорты (для реплея).
    pub fn drain(&self) -> Vec<RawTabletEvent> {
        let mut q = self.events.lock().unwrap();
        std::mem::take(&mut *q)
    }
}
#[cfg(test)]
mod tests {
    use super::{
        PEN_REPORT_GRACE, PEN_X_TOLERANCE, TabletState, pen_emulates_mouse, pen_matches_cursor,
        report_is_fresh,
    };
    use std::time::{Duration, Instant};

    fn pen(x_norm: f32) -> TabletState {
        TabletState {
            present: true,
            x_norm,
            ..Default::default()
        }
    }

    #[test]
    fn no_tablet_data_never_emulates_mouse() {
        assert!(!pen_emulates_mouse(false, false, None, 640.0, 1280.0));
    }

    /// Мышь работает второй рукой: пока перо нажато, клик в другом месте —
    /// это мышь, и глушить его нельзя.
    #[test]
    fn mouse_away_from_the_pen_is_never_filtered() {
        let w = 1280.0;
        // перо нажато в правой части, мышь кликает в левой
        assert!(!pen_emulates_mouse(false, true, Some(pen(0.8)), 200.0, w));
        assert!(!pen_emulates_mouse(true, true, Some(pen(0.8)), 200.0, w));
        // и наоборот
        assert!(!pen_emulates_mouse(false, true, Some(pen(0.2)), 1000.0, w));
    }

    /// Эмуляция пера всегда приходит в его координате — её и глушим.
    #[test]
    fn pen_emulation_at_the_pen_position_is_filtered() {
        let w = 1280.0;
        // нажатие пера и курсор ровно в нём
        assert!(pen_emulates_mouse(true, true, Some(pen(0.5)), 640.0, w));
        // 0.5 % ширины — ещё «та же точка»
        assert!(pen_emulates_mouse(true, false, Some(pen(0.505)), 640.0, w));
        // перо нажато и отчёт свежий, но курсор ушёл на 5 % — это мышь
        assert!(!pen_emulates_mouse(true, true, Some(pen(0.55)), 640.0, w));
        assert!(!pen_emulates_mouse(true, true, Some(pen(0.45)), 640.0, w));
    }

    #[test]
    fn without_a_press_the_mouse_is_left_alone() {
        let w = 1280.0;
        // просто ховер над планшетом не должен глушить мышь
        assert!(!pen_emulates_mouse(false, false, Some(pen(0.5)), 640.0, w));
        // даже если курсор случайно в точке пера
        assert!(!pen_emulates_mouse(false, false, Some(pen(0.5)), 641.0, w));
    }

    #[test]
    fn press_without_a_report_cannot_be_attributed() {
        assert!(!pen_emulates_mouse(true, true, None, 640.0, 1280.0));
    }

    #[test]
    fn stale_report_does_not_match() {
        let now = Instant::now();
        assert!(report_is_fresh(Some(now), PEN_REPORT_GRACE, now));
        assert!(!report_is_fresh(
            now.checked_sub(Duration::from_millis(100)).map(Some).unwrap_or(None),
            PEN_REPORT_GRACE,
            now
        ));
        // старый отчёт caller уже отбросил -> None
        assert!(!pen_emulates_mouse(false, false, None, 640.0, 1280.0));
    }

    #[test]
    fn proximity_rejects_degenerate_input() {
        assert!(!pen_matches_cursor(0.5, 640.0, 0.0, PEN_X_TOLERANCE));
        assert!(!pen_matches_cursor(0.5, 640.0, 1280.0, -0.1));
        assert!(pen_matches_cursor(0.5, 640.0, 1280.0, PEN_X_TOLERANCE));
    }
}
