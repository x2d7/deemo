//! Прямое чтение планшетов. Linux — hidraw (без ML), Windows — hidapi
//! (см. `tablet_win.rs`). Gaomon S620 + Parblo Ninos N4.
//! Форматы vendor-репортов — из mread (driver/tablets). Берём X (ширина) и
//! давление, Y игнорируем. Параллельно пишем сырой лог для калибровки.
//!
//! Данные калибровки: hover = давление 0 у обоих; press = давление > порога.
//! Gaomon X≈26456, P 0..8191; Parblo X≈21598, P 0..4358 (hover-флаг byte1 0xA0/0xA1).
//!
//! Разбор vendor-байтов (`parse_report`) общий для обеих ОС: на Windows
//! hidapi отдаёт те же репорты, что hidraw на Linux (плюс, возможно, байт
//! Report ID спереди — его учитываем по длине репорта).

use std::fs::{File, OpenOptions};
#[cfg(not(windows))]
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Сырой X правого края Gaomon S620 (ширина, 0..26455). Зашито в игру:
/// калибровка в настройках не нужна и не показывается.
pub const GAOMON_X_MAX: f32 = 26456.0;
/// Сырой X правого края Parblo Ninos N4 (ширина). Тоже зашито.
pub const PARBLO_X_MAX: f32 = 21598.0;

pub const GAOMON_VID: u16 = 0x256c;
pub const GAOMON_PID: u16 = 0x006d;
pub const PARBLO_VID: u16 = 0x0483;
pub const PARBLO_PID: u16 = 0xa019;

/// Порог нажатия Gaomon: у него hover даёт p=0, контакт — тысячи.
pub const GAOMON_PRESS_THRESHOLD: i32 = 40;

#[derive(Clone, Copy, Default)]
pub struct TabletState {
    pub present: bool,
    pub x_raw: i32,
    pub pressure: i32,
    pub hover: bool,
    /// нажим стилусом (давление выше порога)
    pub press: bool,
    /// нормализованный X 0..1 по ширине конкретного планшета (с учётом zoom)
    pub x_norm: f32,
    /// тот же X без zoom: им пользуется фильтр эмулированной мыши, потому что
    /// композитор двигает системный курсор по полной ширине планшета
    pub x_norm_raw: f32,
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
    pen_emulates_motion(report, cursor_x, width)
}

/// Движение системного курсора, сгенерированное пером, — независимо от того,
/// нажато ли перо. `report` уже отфильтрован по свежести: пока перо шлёт
/// отчёты (в том числе hover), libinput двигает системный курсор, и игровая
/// мышь не должна прыгать за ним. Настоящая мышь в другой точке проходит.
pub fn pen_emulates_motion(
    report: Option<TabletState>,
    cursor_x: f32,
    width: f32,
) -> bool {
    report.is_some_and(|st| {
        st.present && pen_matches_cursor(st.x_norm_raw, cursor_x, width, PEN_X_TOLERANCE)
    })
}

/// X_max конкретного планшета (для нормализации vendor-репорта).
pub fn x_max_of(kind: &str) -> f32 {
    match kind {
        "gaomon" => GAOMON_X_MAX,
        "parblo" => PARBLO_X_MAX,
        _ => 1.0,
    }
}

fn u16le(b: &[u8], at: usize) -> i32 {
    i32::from(b[at]) | (i32::from(b[at + 1]) << 8)
}

/// Разбор vendor-репорта планшета: `(x_raw, pressure, hover)`.
///
/// Gaomon S620 отдаёт 12-байтные репорты (встречаются и 8-байтные прошивки):
/// X в b[2..4], давление в b[6..8]. Parblo Ninos N4 — 10 байт, b[1] = 0xA0
/// (hover) / 0xA1 (контакт), X в b[2..4], давление в b[6..8].
///
/// Если устройство использует HID Report ID, и Linux hidraw, и hidapi на
/// Windows отдают его первым байтом — тогда репорт длиннее на 1, и все
/// смещения сдвигаются. Длину репорта мы знаем, поэтому выбираем смещение по
/// ней, а не угадываем.
pub fn parse_report(kind: &str, b: &[u8]) -> Option<(i32, i32, bool)> {
    match kind {
        "gaomon" => {
            let off = match b.len() {
                8 | 12 => 0,
                9 | 13 => 1,
                n if n > 8 => 0,
                _ => return None,
            };
            if b.len() < off + 8 {
                return None;
            }
            // UCLogic: 0xC0 — out of range, bit 6 — express keys. Neither is a
            // pen sample; parsing them as X/pressure would jump the cursor.
            let flags = b[off + 1];
            if flags == 0xc0 || flags & 0x40 != 0 {
                return None;
            }
            let x = u16le(b, off + 2);
            let p = u16le(b, off + 6);
            Some((x, p, p == 0))
        }
        "parblo" => {
            let off = match b.len() {
                10 => 0,
                11 => 1,
                _ => return None,
            };
            let flag = b[off + 1];
            if flag != 0xa0 && flag != 0xa1 {
                return None;
            }
            let x = u16le(b, off + 2);
            let p = u16le(b, off + 6);
            Some((x, p, flag == 0xa0))
        }
        _ => None,
    }
}

/// Обновление общего состояния планшета: одно и то же для Linux hidraw и
/// Windows hidapi/digitizer.
#[derive(Clone)]
pub(crate) struct Sink {
    kind: &'static str,
    state: Arc<Mutex<TabletState>>,
    active: Arc<Mutex<Option<(&'static str, Instant)>>>,
    last_report: Arc<Mutex<Option<Instant>>>,
    last_press: Arc<Mutex<Option<Instant>>>,
    log: Arc<Mutex<File>>,
    events: Arc<Mutex<Vec<RawTabletEvent>>>,
    start: Instant,
    zoom: f32,
    press_threshold: i32,
}

impl Sink {
    /// Покормить sink vendor-репортом. `true` — репорт разобран.
    pub(crate) fn vendor_report(&self, bytes: &[u8]) -> bool {
        let Some((x, p, hover)) = parse_report(self.kind, bytes) else {
            return false;
        };
        let raw = (x as f32 / x_max_of(self.kind)).clamp(0.0, 1.0);
        let xn = ((raw - 0.5) * self.zoom + 0.5).clamp(0.0, 1.0);
        let press = !hover && p > self.press_threshold;
        self.emit(x, p, hover, press, xn, raw, bytes);
        true
    }

    /// Покормить sink отчётом стандартной HID digitizer-коллекции.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn digitizer_report(&self, r: &crate::hid_desc::PenReading, bytes: &[u8]) {
        let raw = r.x_norm.clamp(0.0, 1.0);
        let xn = ((raw - 0.5) * self.zoom + 0.5).clamp(0.0, 1.0);
        // Кончик важнее давления: некоторые перья шлют p=0 при контакте.
        let press = r.tip && r.in_range;
        self.emit(r.x_raw, r.pressure, !r.tip, press, xn, raw, bytes);
    }

    fn emit(
        &self,
        x_raw: i32,
        pressure: i32,
        hover: bool,
        press: bool,
        x_norm: f32,
        x_norm_raw: f32,
        bytes: &[u8],
    ) {
        let now = Instant::now();
        if let Ok(mut g) = self.last_report.lock() {
            *g = Some(now);
        }
        if press {
            if let Ok(mut g) = self.last_press.lock() {
                *g = Some(now);
            }
        }
        {
            let mut s = self.state.lock().unwrap();
            *s = TabletState {
                present: true,
                x_raw,
                pressure,
                hover,
                press,
                x_norm,
                x_norm_raw,
            };
        }
        // какой планшет говорил последним (для маркера и слайдов)
        if let Ok(mut a) = self.active.lock() {
            *a = Some((self.kind, now));
        }
        // сырой репорт в очередь реплея (с wall-time)
        {
            let mut raw = [0u8; 16];
            let rl = bytes.len().min(16);
            raw[..rl].copy_from_slice(&bytes[..rl]);
            let ev = RawTabletEvent {
                wall: now,
                kind: self.kind,
                x_raw,
                pressure,
                hover,
                press,
                x_norm,
                raw,
                raw_len: rl as u8,
            };
            let mut q = self.events.lock().unwrap();
            if q.len() > 8192 {
                let excess = q.len() - 8192;
                q.drain(..excess);
            }
            q.push(ev);
        }
        if let Ok(mut lf) = self.log.lock() {
            let t = self.start.elapsed().as_secs_f32();
            let raw: String = bytes.iter().map(|x| format!("{x:02x}")).collect();
            let _ = writeln!(
                lf,
                "{{\"t\":{t:.4},\"kind\":\"{}\",\"x\":{x_raw},\"p\":{pressure},\"hover\":{hover},\"press\":{press},\"raw\":\"{raw}\"}}",
                self.kind
            );
        }
    }
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
        #[cfg(windows)]
        {
            crate::tablet_win::spawn_tablets(&t, zoom, pt);
        }
        #[cfg(not(windows))]
        {
            let gaomon = find_nodes_any_pid(GAOMON_VID);
            let parblo = find_nodes(PARBLO_VID, PARBLO_PID);
            for node in &gaomon {
                t.spawn_node(node, "gaomon", zoom, GAOMON_PRESS_THRESHOLD);
            }
            for node in &parblo {
                t.spawn_node(node, "parblo", zoom, pt);
            }
            if gaomon.is_empty() && parblo.is_empty() {
                println!("tablet: none found (hidraw on linux)");
            }
        }
        t
    }

    /// Общий sink планшета; Windows-бэкенд обновляет через него состояние.
    pub(crate) fn sink(&self, kind: &'static str, zoom: f32, press_threshold: i32) -> Sink {
        let state = match kind {
            "gaomon" => self.gaomon.clone(),
            _ => self.parblo.clone(),
        };
        Sink {
            kind,
            state,
            active: self.active.clone(),
            last_report: self.last_report.clone(),
            last_press: self.last_press.clone(),
            log: self.log.clone(),
            events: self.events.clone(),
            start: self.start,
            zoom,
            press_threshold,
        }
    }

    /// Linux hidraw: открыть/проверить узел здесь, чтобы лог не врал.
    #[cfg(not(windows))]
    fn spawn_node(
        &self,
        node: &str,
        kind: &'static str,
        zoom: f32,
        press_threshold: i32,
    ) {
        let mut f = match File::open(node) {
            Ok(f) => f,
            Err(e) => {
                println!(
                    "tablet: {kind} open failed ({node}): {e} — hidraw access is root-only; \
                     run scripts/install-udev.sh once"
                );
                return;
            }
        };
        let (vid, pid) = match kind {
            "gaomon" => (GAOMON_VID, GAOMON_PID),
            _ => (PARBLO_VID, PARBLO_PID),
        };
        println!("tablet: {kind} {vid:04x}:{pid:04x} opened ({node})");
        let sink = self.sink(kind, zoom, press_threshold);
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            loop {
                match f.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let _ = sink.vendor_report(&buf[..n]);
                    }
                    _ => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
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

/// Лог `--tablet-probe`: файл рядом с exe + stdout + консоль (если есть).
pub struct ProbeLog {
    file: File,
    console: Option<File>,
    pub path: PathBuf,
}

impl ProbeLog {
    pub fn new() -> std::io::Result<ProbeLog> {
        let path = probe_log_path();
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        // На Windows GUI-подсистема не даёт stdout; CONOUT$ появляется
        // только после AttachConsole (main пробует при --tablet-probe).
        #[cfg(windows)]
        let console = OpenOptions::new().write(true).open("CONOUT$").ok();
        #[cfg(not(windows))]
        let console = None;
        Ok(ProbeLog {
            file,
            console,
            path,
        })
    }

    pub fn line(&mut self, s: &str) {
        let _ = writeln!(self.file, "{s}");
        let _ = self.file.flush();
        let _ = writeln!(std::io::stdout(), "{s}");
        let _ = std::io::stdout().flush();
        if let Some(c) = self.console.as_mut() {
            let _ = writeln!(c, "{s}");
            let _ = c.flush();
        }
    }
}

fn probe_log_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("tablet_probe.log")))
        .unwrap_or_else(|| PathBuf::from("tablet_probe.log"))
}

/// `--tablet-probe [seconds]`: собрать диагностику HID и сырые репорты.
pub fn probe(seconds: f32) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        crate::tablet_win::probe(seconds)
    }
    #[cfg(not(windows))]
    {
        probe_hidraw(seconds)
    }
}

#[cfg(not(windows))]
fn probe_hidraw(seconds: f32) -> anyhow::Result<()> {
    use std::time::Duration;

    let mut log = ProbeLog::new().map_err(|e| anyhow::anyhow!("tablet_probe.log: {e}"))?;
    log.line("deemo tablet probe (linux hidraw)");
    log.line(&format!("log: {}", log.path.display()));
    log.line(&format!(
        "exe: {}",
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<{e}>"))
    ));
    log.line(&format!("seconds: {seconds:.1}"));

    let mut nodes: Vec<(String, String, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/class/hidraw") {
        for e in rd.flatten() {
            let base = e.file_name().to_string_lossy().to_string();
            let node = format!("/dev/{base}");
            let (vid, pid) = vidpid(&node);
            nodes.push((node, vid, pid));
        }
    }
    nodes.sort();
    log.line(&format!("-- hidraw devices: {} --", nodes.len()));
    for (node, vid, pid) in &nodes {
        let base = node.rsplit('/').next().unwrap_or("");
        let uevent = std::fs::read_to_string(format!(
            "/sys/class/hidraw/{base}/device/uevent"
        ))
        .unwrap_or_default();
        let name = uevent
            .lines()
            .find_map(|l| l.strip_prefix("HID_NAME="))
            .unwrap_or("");
        log.line(&format!("{node}: vid={vid} pid={pid} name={name:?}"));
    }

    let targets: Vec<(String, &'static str)> = nodes
        .iter()
        .filter_map(|(node, vid, pid)| {
            if vid == "256c" {
                Some((node.clone(), "gaomon"))
            } else if vid == "0483" && pid == "a019" {
                Some((node.clone(), "parblo"))
            } else {
                None
            }
        })
        .collect();
    if targets.is_empty() {
        log.line("no deemo tablet hidraw nodes (gaomon 256c:006d / parblo 0483:a019)");
    }

    log.line("-- raw reports (up to 20 per node, ms since probe start) --");
    let log = Arc::new(Mutex::new(log));
    let start = Instant::now();
    let deadline = start + Duration::from_secs_f32(seconds.max(0.5));
    for (node, kind) in targets {
        let log = log.clone();
        std::thread::spawn(move || {
            let mut f = match File::open(&node) {
                Ok(f) => f,
                Err(e) => {
                    log.lock().unwrap().line(&format!(
                        "{node}: open failed: {e} — hidraw access is root-only; run scripts/install-udev.sh"
                    ));
                    return;
                }
            };
            let mut buf = [0u8; 64];
            let mut shown = 0usize;
            loop {
                if Instant::now() >= deadline || shown >= 20 {
                    break;
                }
                match f.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let ms = start.elapsed().as_secs_f32() * 1000.0;
                        let hex: String =
                            buf[..n].iter().map(|b| format!("{b:02x} ")).collect();
                        let parsed = match parse_report(kind, &buf[..n]) {
                            Some((x, p, hover)) => {
                                format!(" vendor x={x} p={p} hover={hover}")
                            }
                            None => " vendor=unparsed".to_string(),
                        };
                        if let Ok(mut l) = log.lock() {
                            l.line(&format!(
                                "[{kind}] +{ms:.1}ms len={n} {hex}|{parsed}"
                            ));
                        }
                        shown += 1;
                    }
                    _ => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
    }
    std::thread::sleep(Duration::from_secs_f32(seconds.max(0.5)));
    if let Ok(mut l) = log.lock() {
        l.line("-- probe finished --");
    }
    Ok(())
}

#[cfg(not(windows))]
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

#[cfg(not(windows))]
fn find_nodes_any_pid(vid: u16) -> Vec<String> {
    let vid = format!("{vid:04x}");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/class/hidraw") {
        for e in rd.flatten() {
            let node = format!("/dev/{}", e.file_name().to_string_lossy());
            let (v, _p) = vidpid(&node);
            if v == vid {
                out.push(node);
            }
        }
    }
    out.sort();
    out
}

#[cfg(not(windows))]
fn find_nodes(vid: u16, pid: u16) -> Vec<String> {
    let vid = format!("{vid:04x}");
    let pid = format!("{pid:04x}");
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

#[cfg(test)]
mod tests {
    use super::{
        PEN_REPORT_GRACE, PEN_X_TOLERANCE, TabletState, pen_emulates_motion, pen_emulates_mouse, pen_matches_cursor,
        parse_report, report_is_fresh,
    };
    use std::time::{Duration, Instant};

    fn pen(x_norm: f32) -> TabletState {
        TabletState {
            present: true,
            x_norm,
            x_norm_raw: x_norm,
            ..Default::default()
        }
    }

    fn gaomon(x: u16, p: u16) -> Vec<u8> {
        let mut b = vec![0u8; 12];
        b[2..4].copy_from_slice(&x.to_le_bytes());
        b[6..8].copy_from_slice(&p.to_le_bytes());
        b
    }

    fn parblo(flag: u8, x: u16, p: u16) -> Vec<u8> {
        let mut b = vec![0u8; 10];
        b[1] = flag;
        b[2..4].copy_from_slice(&x.to_le_bytes());
        b[6..8].copy_from_slice(&p.to_le_bytes());
        b
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
    /// Свежий hover-отчёт (без нажатия) у позиции пера тоже должен глушить
    /// движение курсора; обычная мышь в другой точке — нет.
    #[test]
    fn recent_pen_hover_motion_is_ignored() {
        let p = pen(0.5);
        assert!(pen_emulates_motion(Some(p), 0.5 * 1280.0, 1280.0));
        assert!(!pen_emulates_motion(None, 0.5 * 1280.0, 1280.0));
        assert!(!pen_emulates_motion(Some(p), 0.2 * 1280.0, 1280.0));
        // press-фильтр кликов не изменился: hover без нажатия клик не глушит
        assert!(!pen_emulates_mouse(false, false, Some(p), 0.5 * 1280.0, 1280.0));
    }

    #[test]
    fn zoom_does_not_break_the_emulated_mouse_filter() {
        // tablet_zoom=1.8 сдвигает игровой x_norm, но системный курсор
        // композитор двигает по полной ширине планшета — фильтр обязан
        // сравнивать raw-позицию, иначе перо «уводит» мышь.
        let raw = 0.7f32;
        let st = TabletState {
            present: true,
            x_norm: ((raw - 0.5) * 1.8 + 0.5).clamp(0.0, 1.0),
            x_norm_raw: raw,
            ..TabletState::default()
        };
        assert!(pen_emulates_motion(Some(st), raw * 1280.0, 1280.0));
        assert!(!pen_emulates_motion(Some(st), st.x_norm * 1280.0, 1280.0));
    }

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

    /// Gaomon S620: 12 байт, X в b[2..4], давление в b[6..8]. Ховер — p=0.
    #[test]
    fn gaomon_vendor_report_is_12_bytes() {
        let b = gaomon(1234, 500);
        assert_eq!(parse_report("gaomon", &b), Some((1234, 500, false)));
        let hover = gaomon(777, 0);
        assert_eq!(parse_report("gaomon", &hover), Some((777, 0, true)));
        // 8-байтная старая прошивка
        assert_eq!(parse_report("gaomon", &b[..8]), Some((1234, 500, false)));
    }

    /// Report ID спереди: длина +1, смещения сдвигаются на байт.
    #[test]
    fn report_id_prefix_is_handled_by_length() {
        let mut b = vec![0x07u8];
        b.extend_from_slice(&gaomon(4321, 900));
        assert_eq!(b.len(), 13);
        assert_eq!(parse_report("gaomon", &b), Some((4321, 900, false)));

        let mut p = vec![0x01u8];
        p.extend_from_slice(&parblo(0xa1, 2000, 300));
        assert_eq!(p.len(), 11);
        assert_eq!(parse_report("parblo", &p), Some((2000, 300, false)));
    }

    /// Parblo Ninos N4: 10 байт, b[1] 0xA0 hover / 0xA1 контакт.
    #[test]
    fn parblo_vendor_report_is_10_bytes() {
        assert_eq!(
            parse_report("parblo", &parblo(0xa0, 100, 0)),
            Some((100, 0, true))
        );
        assert_eq!(
            parse_report("parblo", &parblo(0xa1, 2000, 300)),
            Some((2000, 300, false))
        );
        // не тот флаг — не наш репорт
        assert_eq!(parse_report("parblo", &parblo(0x00, 2000, 300)), None);
        assert_eq!(parse_report("parblo", &[0u8; 64]), None);
    }

    /// Реальные репорты Gaomon S620 из старого захвата `.work/tablet_both.jsonl`
    /// (12 байт, report id 0x08). Проверяют ровно те смещения, которые читает
    /// игра на Linux и Windows.
    #[test]
    fn real_gaomon_capture_parses_to_the_recorded_values() {
        let hover = parse_report("gaomon", &hex("08804443604f000000000000"))
            .expect("hover report");
        assert_eq!(hover, (17220, 0, true));

        let press = parse_report("gaomon", &hex("0885d0340100ff1f00000000"))
            .expect("press report");
        assert_eq!(press, (13520, 8191, false));
    }

    #[test]
    fn malformed_reports_are_rejected() {
        assert_eq!(parse_report("gaomon", &[0u8; 4]), None);
        assert_eq!(parse_report("gaomon", &[]), None);
        assert_eq!(parse_report("parblo", &[0u8; 9]), None);
        assert_eq!(parse_report("nope", &[0u8; 16]), None);
    }

    fn hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks_exact(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }
}
