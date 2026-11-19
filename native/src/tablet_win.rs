//! Windows-бэкенд планшетов: hidapi (windows-native) + разбор стандартной
//! HID digitizer-коллекции.
//!
//! Windows может показать один и тот же планшет несколькими HID-интерфейсами:
//! vendor-коллекцией (те же байты, что hidraw на Linux) и стандартной
//! digitizer-коллекцией (usage page 0x0D), чья раскладка задана дескриптором.
//! Открываем все интерфейсы, vendor-разбор пробуем первым; digitizer работает
//! как fallback и не перебивает vendor-репорты.
//!
//! Linux-путь (hidraw в `tablet.rs`) этим модулем не затрагивается.

use crate::hid_desc::DigitizerLayout;
use crate::tablet::{
    self, ProbeLog, Sink, Tablets, GAOMON_PID, GAOMON_PRESS_THRESHOLD, GAOMON_VID, PARBLO_PID,
    PARBLO_VID,
};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Target {
    kind: &'static str,
    vid: u16,
    pid: u16,
}

const TARGETS: [Target; 2] = [
    Target {
        kind: "gaomon",
        vid: GAOMON_VID,
        pid: GAOMON_PID,
    },
    Target {
        kind: "parblo",
        vid: PARBLO_VID,
        pid: PARBLO_PID,
    },
];

fn target_kind(vid: u16, pid: u16) -> Option<&'static str> {
    TARGETS
        .iter()
        .find(|t| t.vid == vid && t.pid == pid)
        .map(|t| t.kind)
}

/// Что делать с репортами конкретного интерфейса.
enum Mode {
    /// Только vendor-байты (обычная vendor-коллекция).
    Vendor,
    /// Сначала vendor-байты, при неудаче — digitizer по дескриптору.
    Auto(Option<DigitizerLayout>),
    /// Стандартная digitizer-коллекция (usage page 0x0D).
    Digitizer(Option<DigitizerLayout>),
}

/// Запустить Windows-потоки планшетов. Вызывается из `Tablets::new`.
pub(crate) fn spawn_tablets(tablets: &Tablets, zoom: f32, p_threshold: i32) {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            println!("tablet: hidapi init failed: {e}");
            println!("tablet: none found (hidapi on windows)");
            return;
        }
    };
    println!("tablet: hidapi: {} interfaces", api.device_list().count());

    let mut found_any = false;
    for target in TARGETS {
        let devices: Vec<DeviceInfo> = api
            .device_list()
            .filter(|d| d.vendor_id() == target.vid && d.product_id() == target.pid)
            .cloned()
            .collect();
        if devices.is_empty() {
            println!(
                "tablet: {} {:04x}:{:04x} not found",
                target.kind, target.vid, target.pid
            );
            continue;
        }
        found_any = true;
        let vendor_seen = Arc::new(AtomicBool::new(false));
        let threshold = match target.kind {
            "gaomon" => GAOMON_PRESS_THRESHOLD,
            _ => p_threshold.max(0),
        };

        // Сначала собрать дескрипторы и раскладки, затем открывать.
        let mut plans: Vec<(DeviceInfo, Option<DigitizerLayout>)> = Vec::new();
        for info in devices {
            let (desc, source) = descriptor_of(&info, &api);
            let layout = DigitizerLayout::parse(&desc);
            println!(
                "tablet: {} {:04x}:{:04x} interface {} usage_page 0x{:04x} usage 0x{:04x} descriptor {} bytes ({source})",
                target.kind,
                info.vendor_id(),
                info.product_id(),
                info.interface_number(),
                info.usage_page(),
                info.usage(),
                desc.len(),
            );
            if let Some(l) = &layout {
                println!(
                    "tablet: {} {:04x}:{:04x} digitizer layout: {}",
                    target.kind,
                    info.vendor_id(),
                    info.product_id(),
                    l.describe()
                );
            }
            print_input_lengths(
                &format!(
                    "tablet: {} {:04x}:{:04x} interface {}",
                    target.kind,
                    info.vendor_id(),
                    info.product_id(),
                    info.interface_number()
                ),
                &desc,
            );
            plans.push((info, layout));
        }

        for (info, layout) in plans {
            let digitizer_usage = info.usage_page() == 0x0d && matches!(info.usage(), 0x01 | 0x02);
            let mode = if digitizer_usage {
                if layout.is_none() {
                    println!(
                        "tablet: {} {:04x}:{:04x} digitizer interface without a usable layout — skipped (send tablet_probe.log)",
                        target.kind,
                        info.vendor_id(),
                        info.product_id(),
                    );
                    continue;
                }
                Mode::Digitizer(layout)
            } else if layout.is_some() {
                // Vendor-коллекция с digitizer-полями: vendor в приоритете.
                Mode::Auto(layout)
            } else {
                Mode::Vendor
            };

            let dev = match info.open_device(&api) {
                Ok(dev) => dev,
                Err(e) => {
                    println!(
                        "tablet: {} {:04x}:{:04x} open failed (interface {}): {e}",
                        target.kind,
                        info.vendor_id(),
                        info.product_id(),
                        info.interface_number(),
                    );
                    continue;
                }
            };
            println!(
                "tablet: {} {:04x}:{:04x} opened (interface {}, usage_page 0x{:04x} usage 0x{:04x}, {})",
                target.kind,
                info.vendor_id(),
                info.product_id(),
                info.interface_number(),
                info.usage_page(),
                info.usage(),
                match &mode {
                    Mode::Vendor => "vendor",
                    Mode::Auto(_) => "vendor-first",
                    Mode::Digitizer(_) => "digitizer",
                },
            );
            let sink = tablets.sink(target.kind, zoom, threshold);
            let kind = target.kind;
            let usage = (info.usage_page(), info.usage());
            let vendor_seen = vendor_seen.clone();
            std::thread::spawn(move || {
                run_interface(dev, sink, mode, vendor_seen, kind, usage);
            });
        }
    }

    if !found_any {
        println!("tablet: none found (hidapi on windows)");
    }
}

fn run_interface(
    dev: HidDevice,
    sink: Sink,
    mode: Mode,
    vendor_seen: Arc<AtomicBool>,
    kind: &'static str,
    usage: (u16, u16),
) {
    let mut buf = [0u8; 256];
    let mut announced = false;
    let mut unparsed = 0u32;
    let mut read_errors = 0u32;
    loop {
        match dev.read_timeout(&mut buf, 100) {
            Ok(0) => {}
            Ok(n) => {
                let bytes = &buf[..n];
                let mut vendor_ok = false;
                match &mode {
                    Mode::Vendor | Mode::Auto(_) => {
                        vendor_ok = sink.vendor_report(bytes);
                    }
                    Mode::Digitizer(_) => {}
                }
                if vendor_ok {
                    vendor_seen.store(true, Ordering::Relaxed);
                    if !announced {
                        println!(
                            "tablet: {kind} vendor layout active ({n} byte reports)"
                        );
                        announced = true;
                    }
                    continue;
                }

                match &mode {
                    Mode::Vendor => {
                        unparsed += 1;
                        if unparsed == 20 {
                            println!(
                                "tablet: {kind} vendor interface sends {n}-byte reports that do not match the layout; tablet_probe.log will show the raw bytes"
                            );
                        }
                    }
                    Mode::Auto(Some(layout)) | Mode::Digitizer(Some(layout)) => {
                        if let Some(reading) = layout.decode(bytes) {
                            if !vendor_seen.load(Ordering::Relaxed) {
                                sink.digitizer_report(&reading, bytes);
                                if !announced {
                                    println!(
                                        "tablet: {kind} digitizer interface selected (usage 0x{:04x}/0x{:02x}, report id {:?}, {} byte reports)",
                                        usage.0, usage.1, reading.report_id, n
                                    );
                                    announced = true;
                                }
                            }
                        } else if !matches!(&mode, Mode::Auto(_)) {
                            unparsed += 1;
                            if unparsed == 20 {
                                println!(
                                    "tablet: {kind} digitizer reports do not decode (len {n}); tablet_probe.log will show them"
                                );
                            }
                        }
                    }
                    Mode::Auto(None) | Mode::Digitizer(None) => {}
                }
            }
            Err(e) => {
                read_errors += 1;
                if read_errors <= 3 {
                    println!("tablet: {kind} read error: {e}");
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// Дескриптор: сначала настоящий из HidD_GetReportDescriptor, потом
/// реконструкция hidapi из preparsed data.
fn descriptor_of(info: &DeviceInfo, api: &HidApi) -> (Vec<u8>, &'static str) {
    if let Some(desc) = raw_descriptor(info.path()) {
        return (desc, "HidD");
    }
    if let Ok(dev) = info.open_device(api) {
        let mut buf = vec![0u8; hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
        if let Ok(n) = dev.get_report_descriptor(&mut buf) {
            buf.truncate(n);
            return (buf, "hidapi-reconstructed");
        }
    }
    (Vec::new(), "unavailable")
}

/// `HidD_GetReportDescriptor` через hid.dll (в windows-sys этой функции нет).
/// Открываем путь с нулевым доступом — так требует HID API.
fn raw_descriptor(path: &CStr) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleHandleW, GetProcAddress, LoadLibraryW,
    };

    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let hid_name: Vec<u16> = "hid.dll\0".encode_utf16().collect();
    unsafe {
        let handle = CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return None;
        }
        let mut hid = GetModuleHandleW(hid_name.as_ptr());
        if hid.is_null() {
            hid = LoadLibraryW(hid_name.as_ptr());
        }
        let mut result = None;
        if !hid.is_null() {
            let proc = GetProcAddress(hid, c"HidD_GetReportDescriptor".as_ptr() as *const u8);
            if let Some(f) = proc {
                let f: unsafe extern "system" fn(HANDLE, *mut u8, u32) -> u8 =
                    std::mem::transmute(f);
                let mut buf = vec![0u8; hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
                let ok = f(handle, buf.as_mut_ptr(), buf.len() as u32);
                if ok != 0 {
                    // API не возвращает длину: обрезаем нулевой хвост, иначе
                    // в лог попадёт 4 КБ нулей, а длина дескриптора соврёт.
                    if let Some(last) = buf.iter().rposition(|&b| b != 0) {
                        buf.truncate(last + 1);
                        result = Some(buf);
                    }
                }
            }
        }
        let _ = CloseHandle(handle);
        result
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x} ")).collect()
}

/// Одна строка `input length ...` из дескриптора (для лога).
fn print_input_lengths(prefix: &str, desc: &[u8]) {
    let lens = crate::hid_desc::input_report_lengths(desc);
    if lens.is_empty() {
        return;
    }
    let text = lens
        .iter()
        .map(|(id, n)| match id {
            Some(id) => format!("{n} bytes (report id 0x{id:02x})"),
            None => format!("{n} bytes (no report id)"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    println!("{prefix} input report length: {text}");
}

/// `--tablet-probe [seconds]`: полный дамп HID-интерфейсов, дескрипторов и
/// сырых репортов. Пишется в `tablet_probe.log` рядом с exe.
pub(crate) fn probe(seconds: f32) -> anyhow::Result<()> {
    let mut log = ProbeLog::new().map_err(|e| anyhow::anyhow!("tablet_probe.log: {e}"))?;
    log.line("deemo tablet probe (windows hidapi-native)");
    log.line(&format!("log: {}", log.path.display()));
    log.line(&format!(
        "exe: {}",
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<{e}>"))
    ));
    log.line(&format!(
        "cwd: {}",
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<{e}>"))
    ));
    log.line(&format!("seconds: {seconds:.1}"));

    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            log.line(&format!("hidapi init failed: {e}"));
            log.line("-- probe finished --");
            return Ok(());
        }
    };

    struct Iface {
        idx: usize,
        info: DeviceInfo,
        layout: Option<DigitizerLayout>,
    }

    let devices: Vec<DeviceInfo> = api.device_list().cloned().collect();
    log.line(&format!("-- hid devices: {} --", devices.len()));
    let mut ifaces = Vec::new();
    for (i, info) in devices.iter().enumerate() {
        let (desc, source) = descriptor_of(info, &api);
        let layout = DigitizerLayout::parse(&desc);
        log.line(&format!("[{i}] path={}", info.path().to_string_lossy()));
        log.line(&format!(
            "    vid={:04x} pid={:04x} interface={} usage_page=0x{:04x} usage=0x{:04x} bus={:?}",
            info.vendor_id(),
            info.product_id(),
            info.interface_number(),
            info.usage_page(),
            info.usage(),
            info.bus_type(),
        ));
        log.line(&format!(
            "    product={:?} manufacturer={:?} serial={:?} release=0x{:04x}",
            info.product_string(),
            info.manufacturer_string(),
            info.serial_number(),
            info.release_number(),
        ));
        if desc.is_empty() {
            log.line(&format!("    descriptor: unavailable ({source})"));
        } else {
            log.line(&format!(
                "    descriptor ({} bytes, {source}): {}",
                desc.len(),
                hex(&desc)
            ));
        }
        if let Some(l) = &layout {
            log.line(&format!("    digitizer layout: {}", l.describe()));
        }
        let lens = crate::hid_desc::input_report_lengths(&desc);
        if !lens.is_empty() {
            let text = lens
                .iter()
                .map(|(id, n)| match id {
                    Some(id) => format!("{n} bytes (report id 0x{id:02x})"),
                    None => format!("{n} bytes (no report id)"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            log.line(&format!("    input report length: {text}"));
        }
        if let Some(kind) = target_kind(info.vendor_id(), info.product_id()) {
            log.line(&format!("    target: {kind}"));
        } else if info.usage_page() == 0x0d || layout.is_some() {
            log.line("    target: other-digitizer (not one of ours)");
        }
        ifaces.push(Iface {
            idx: i,
            info: info.clone(),
            layout,
        });
    }

    // Читаем все интерфейсы целевых планшетов и чужие digitizer-устройства:
    // так в лог попадёт устройство, которое Windows установила под другим
    // VID/PID (или вообще без vendor-коллекции).
    struct ReadJob {
        info: DeviceInfo,
        layout: Option<DigitizerLayout>,
        kind: Option<&'static str>,
        label: String,
    }
    let mut jobs = Vec::new();
    for iface in ifaces {
        let target = target_kind(iface.info.vendor_id(), iface.info.product_id());
        let other_digitizer = target.is_none()
            && (iface.info.usage_page() == 0x0d || iface.layout.is_some());
        if target.is_none() && !other_digitizer {
            continue;
        }
        let kind = target;
        let label = format!(
            "{} {:04x}:{:04x} #{}{}",
            kind.unwrap_or("other"),
            iface.info.vendor_id(),
            iface.info.product_id(),
            iface.idx,
            if iface.info.usage_page() == 0x0d {
                " digitizer"
            } else {
                ""
            }
        );
        jobs.push(ReadJob {
            info: iface.info,
            layout: iface.layout,
            kind,
            label,
        });
    }
    if jobs.is_empty() {
        log.line("no deemo tablets or digitizer interfaces found");
        log.line("-- probe finished --");
        return Ok(());
    }

    /// Что удалось увидеть на одном интерфейсе за время пробы.
    #[derive(Default)]
    struct Stats {
        reports: u32,
        vendor: u32,
        digitizer: u32,
        min_x: Option<i32>,
        max_x: Option<i32>,
        min_p: Option<i32>,
        max_p: Option<i32>,
        pressed: bool,
    }

    impl Stats {
        fn observe(&mut self, x: i32, p: i32, pressed: bool) {
            self.min_x = Some(self.min_x.map_or(x, |v| v.min(x)));
            self.max_x = Some(self.max_x.map_or(x, |v| v.max(x)));
            self.min_p = Some(self.min_p.map_or(p, |v| v.min(p)));
            self.max_p = Some(self.max_p.map_or(p, |v| v.max(p)));
            self.pressed |= pressed;
        }

        fn line(&self, label: &str) -> String {
            if self.reports == 0 {
                return format!("[{label}] no raw reports");
            }
            let range = match (self.min_x, self.max_x) {
                (Some(a), Some(b)) => format!(
                    "x {a}..{b}, p {}..{}, pressed={}",
                    self.min_p.unwrap_or(0),
                    self.max_p.unwrap_or(0),
                    if self.pressed { "yes" } else { "no" }
                ),
                _ => "raw only (layout did not parse)".to_string(),
            };
            format!(
                "[{label}] {} reports ({} vendor, {} digitizer), {range}",
                self.reports, self.vendor, self.digitizer
            )
        }
    }

    // Проба длится `seconds`. Ховер/движение пишем не больше 20 раз на
    // интерфейс и только по одному в слот `seconds/20` (сэмплы распределены по
    // всему тесту), а нажатия — отдельно, до 20 штук, даже если ховер уже
    // забил свою квоту. Иначе потоковый ховер съедает лог до того, как
    // тестировщик успеет нажать перо.
    log.line("-- raw reports (up to 20 moving/hover samples + up to 20 pressed per interface, spread over the probe, ms since read start) --");
    let span = seconds.max(0.5);
    let slot_ms = span * 1000.0 / 20.0;
    let log = Arc::new(Mutex::new(log));
    let start = Instant::now();
    let deadline = start + Duration::from_secs_f32(span);
    let mut handles = Vec::new();
    for job in jobs {
        let dev = match job.info.open_device(&api) {
            Ok(dev) => dev,
            Err(e) => {
                log.lock()
                    .unwrap()
                    .line(&format!("[{}] open failed: {e}", job.label));
                continue;
            }
        };
        let log = log.clone();
        let label = job.label.clone();
        let kind = job.kind;
        let layout = job.layout;
        handles.push((
            label.clone(),
            std::thread::spawn(move || -> Stats {
                let mut stats = Stats::default();
                let mut buf = [0u8; 256];
                let mut last_slot: Option<usize> = None;
                let mut hover_logged = 0u32;
                let mut press_logged = 0u32;
                loop {
                    if Instant::now() >= deadline {
                        break;
                    }
                    match dev.read_timeout(&mut buf, 50) {
                        Ok(0) => {}
                        Ok(n) => {
                            let ms = start.elapsed().as_secs_f32() * 1000.0;
                            let data = &buf[..n];
                            let mut note = String::new();
                            let mut is_press = false;
                            if let Some(kind) = kind {
                                if let Some((x, p, hover)) = tablet::parse_report(kind, data) {
                                    note = format!("vendor x={x} p={p} hover={hover}");
                                    stats.vendor += 1;
                                    is_press = !hover;
                                    stats.observe(x, p, is_press);
                                }
                            }
                            if note.is_empty() {
                                if let Some(l) = &layout {
                                    if let Some(r) = l.decode(data) {
                                        note = format!(
                                            "digitizer x={} y={:?} p={} tip={} in_range={}",
                                            r.x_raw, r.y_raw, r.pressure, r.tip, r.in_range
                                        );
                                        stats.digitizer += 1;
                                        is_press = r.tip;
                                        stats.observe(r.x_raw, r.pressure, r.tip);
                                    }
                                }
                            }
                            if is_press {
                                if press_logged >= 20 {
                                    continue;
                                }
                                press_logged += 1;
                            } else {
                                if hover_logged >= 20 {
                                    continue;
                                }
                                let slot = ((ms / slot_ms) as usize).min(19);
                                if last_slot == Some(slot) {
                                    continue;
                                }
                                last_slot = Some(slot);
                                hover_logged += 1;
                            }
                            stats.reports += 1;
                            if let Ok(mut l) = log.lock() {
                                l.line(&format!(
                                    "[{label}] +{ms:.1}ms len={n} {}| {note}",
                                    hex(data)
                                ));
                            }
                        }
                        Err(e) => {
                            if let Ok(mut l) = log.lock() {
                                l.line(&format!("[{label}] read error: {e}"));
                            }
                            break;
                        }
                    }
                }
                stats
            }),
        ));
    }
    let mut totals = Vec::new();
    for (label, handle) in handles {
        let stats = handle.join().unwrap_or_default();
        let line = stats.line(&label);
        totals.push((label, line));
    }
    if let Ok(mut l) = log.lock() {
        l.line("-- summary --");
        for (_, line) in &totals {
            l.line(line);
        }
        l.line("-- probe finished --");
    }
    Ok(())
}
