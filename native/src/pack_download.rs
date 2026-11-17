//! Скачивание всех «ванильных» паков с сервера прямо из игры.
//!
//! Ванильные = паки, лежащие на сервере без владельца (`owner: null`), т.е.
//! изначальная библиотека Deemo. Пользовательские аплоады (`owner: "ник"`)
//! в «скачать всё» не попадают.
//!
//! Раньше 447 паков качались 447 отдельными запросами. Теперь сервер отдаёт
//! один zip-бандл (`/api/bundle/vanilla.zip`), который мы:
//!
//! 1. докачиваем в `<import_dir>/../.bundle.part` (с `Range`-докачкой и ETag,
//!    поэтому оборванная загрузка продолжается, а не начинается с нуля);
//! 2. распаковываем **потоково** прямо из файла: читаем только хвост (EOCD +
//!    центральный каталог), а данные каждого файла — кусками по 64 КиБ через
//!    `flate2`. Архив на 1.3 ГБ в память не попадает;
//! 3. каждый пак ставим атомарно: пишем в `import_dir/.<id>.tmp-<pid>/` и
//!    переименовываем в `import_dir/<id>`, когда группа записей пака кончилась
//!    и в ней есть `pack.json`. Оборванная установка не оставляет полпака.
//!
//! Раскладка бандла плоская, по папке на пак (зеркало `data/imported/<id>/`):
//! `manifest.json`, затем `<id>/pack.json`, `<id>/charts/<diff>.json`,
//! `<id>/audio.<ext>`, `<id>/preview.<ext>`, `<id>/cover.<ext>`.
//! JSON упакован deflate, медиа лежит как есть (store). Zip64 не поддерживаем.
//!
//! Если эндпоинт бандла отвечает 404/501 (старый сервер) — работаем по-старому:
//! качаем паки по одному (`/api/packs/<id>`) и ставим через [`install_zip`].
//! Этот путь оставлен и как аварийный: любой сбой бандла (сеть, битый архив)
//! не должен оставить пользователя без паков.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Пак из каталога сервера.
#[derive(Clone, Debug)]
pub struct CatalogEntry {
    pub id: String,
    pub title: String,
    /// `None` = ванильный пак (owner: null на сервере).
    pub owner: Option<String>,
}

/// Стадия загрузки паков: UI показывает текст, тесты — стадию.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    /// Ничего не происходит (или идёт пак-по-паку).
    #[default]
    Idle,
    /// Сервер собирает бандл — ждём.
    Building,
    /// Качаем архив бандла.
    Downloading,
    /// Распаковываем архив.
    Unpacking,
}

/// Прогресс для UI. UI-поток читает последнее сообщение из канала.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub done: usize,
    pub skipped: usize,
    pub total: usize,
    pub bytes: u64,
    pub current: String,
    pub failed: Vec<(String, String)>,
    pub finished: bool,
    pub cancelled: bool,
    /// Текущая стадия загрузки (для тестов и подписей).
    pub stage: Stage,
    /// Ожидаемый размер бандла в байтах (0 = неизвестен / старый сервер).
    /// UI может показать «42.0 MB из 1.3 GB»; существующие поля не меняются.
    pub total_bytes: u64,
    /// Сколько паков реально распаковано в этом прогоне. Во время скачивания
    /// `done` — это оценка «сколько паков как будто готово» (чтобы полоска
    /// ехала), а `unpacked` остаётся честным счётчиком распаковки.
    pub unpacked: usize,
}

impl Progress {
    pub fn percent(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            let p = (self.done + self.skipped) as f32 * 100.0 / self.total as f32;
            p.min(100.0)
        }
    }
}

/// Максимальный размер распакованного пака (защита от zip-бомб).
const MAX_UNPACKED: u64 = 512 * 1024 * 1024;
/// Максимум записей в одном паке.
const MAX_ENTRIES: usize = 4096;
/// Максимум записей во всём бандле (447 паков × ~10 файлов + запас).
const MAX_BUNDLE_ENTRIES: usize = 65_536;
/// Максимум центрального каталога, который готовы прочитать в память.
const MAX_CD: u64 = 64 * 1024 * 1024;
/// Размер куска при скачивании/распаковке.
const CHUNK: usize = 64 * 1024;
/// Так часто (по байтам) сообщаем прогресс скачивания в UI.
const PROGRESS_EVERY: u64 = 1024 * 1024;
/// Так часто (по времени) сообщаем прогресс, если байты капают медленно.
const PROGRESS_EVERY_TIME: std::time::Duration = std::time::Duration::from_millis(200);
/// Окно с конца файла, в котором ищем EOCD.
const TAIL_WINDOW: u64 = 64 * 1024;
/// Сколько ждём, пока сервер соберёт бандл (state=building).
const BUILD_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
/// Пауза между запросами, если сервер не прислал Retry-After.
const RETRY_AFTER_DEFAULT: u64 = 5;
/// Максимум одного пака по старому пути (`/api/packs/<id>`).
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;

/// Путь к бандлу на сервере.
const BUNDLE_PATH: &str = "/api/bundle/vanilla.zip";
/// Путь к статусу сборки бандла.
const BUNDLE_STATUS_PATH: &str = "/api/bundle/status";

/// Ошибка-маркер: сервер не умеет отдавать бандл (404/501) → качаем по одному.
#[derive(Debug)]
pub struct BundleUnsupported;

impl std::fmt::Display for BundleUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "server has no bundle endpoint")
    }
}

impl std::error::Error for BundleUnsupported {}

/// Ошибка-маркер: нас отменили.
#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Ошибка-маркер: сервер ещё собирает архив (503 + Retry-After).
#[derive(Debug)]
struct Building {
    retry_after: u64,
}

impl std::fmt::Display for Building {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bundle is being built (retry in {}s)", self.retry_after)
    }
}

impl std::error::Error for Building {}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(60))
        .build()
}

/// Агент для самого бандла: без общего таймаута (1.3 ГБ не влезают в 60 с),
/// но с таймаутом на отдельное чтение сокета.
fn download_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(30))
        .timeout_write(std::time::Duration::from_secs(30))
        .build()
}

/// Каталог сервера, только ванильные паки (без владельца).
pub fn vanilla_catalog(server: &str) -> Result<Vec<CatalogEntry>> {
    let url = format!("{}/api/packs", server.trim_end_matches('/'));
    let v: serde_json::Value = agent()
        .get(&url)
        .call()
        .map_err(|e| anyhow::anyhow!("catalog request failed: {e}"))?
        .into_json()
        .context("catalog is not valid JSON")?;
    let mut out = Vec::new();
    for p in v["packs"].as_array().cloned().unwrap_or_default() {
        let id = p["id"].as_str().unwrap_or("").to_string();
        if !crate::pack::safe_id(&id) {
            continue;
        }
        let owner = p["owner"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        if owner.is_some() {
            continue; // пользовательский аплоад — не ванильный
        }
        out.push(CatalogEntry {
            id,
            title: p["title"].as_str().unwrap_or("").to_string(),
            owner,
        });
    }
    Ok(out)
}

/// Скачать и поставить паки, сообщая прогресс в `tx`.
///
/// Уже установленные паки пропускаются (их можно перекачать, удалив папку).
/// Возвращает итоговый прогресс.
pub fn install_all(
    server: &str,
    import_dir: &Path,
    cancel: Arc<AtomicBool>,
    tx: Option<std::sync::mpsc::Sender<Progress>>,
) -> Progress {
    let mut prog = Progress::default();
    let catalog = match vanilla_catalog(server) {
        Ok(c) => c,
        Err(e) => {
            prog.failed.push(("catalog".into(), e.to_string()));
            prog.finished = true;
            emit(&tx, &prog);
            return prog;
        }
    };
    prog.total = catalog.len();
    let titles: HashMap<String, String> = catalog
        .iter()
        .map(|e| (e.id.clone(), if e.title.is_empty() { e.id.clone() } else { e.title.clone() }))
        .collect();
    let mut installed = installed_set(&catalog, import_dir);
    prog.skipped = installed.len();
    emit(&tx, &prog);
    let mut bundle_ok = false;

    if cancel.load(Ordering::Relaxed) {
        prog.cancelled = true;
        prog.finished = true;
        emit(&tx, &prog);
        return prog;
    }
    // всё уже стоит — качать 1.3 ГБ незачем
    if !catalog.is_empty() && installed.len() >= catalog.len() {
        prog.finished = true;
        emit(&tx, &prog);
        return prog;
    }
    let _ = std::fs::create_dir_all(import_dir);

    // ---------- один бандл ----------
    if bundle_enabled() {
        let mut on_event = |ev: BundleEvent| match ev {
            BundleEvent::Building { count } => {
                if let Some(c) = count.filter(|c| *c > 0) {
                    prog.total = c;
                }
                prog.stage = Stage::Building;
                prog.current = "server is building the archive…".into();
                emit(&tx, &prog);
            }
            BundleEvent::Count(c) => {
                if c > 0 {
                    prog.total = c;
                }
                emit(&tx, &prog);
            }
            BundleEvent::Bytes { on_disk, total } => {
                prog.bytes = on_disk;
                prog.total_bytes = total;
                prog.done = download_done(on_disk, total, prog.total, prog.skipped);
                prog.stage = Stage::Downloading;
                prog.current = "downloading the archive…".into();
                emit(&tx, &prog);
            }
        };
        match download_bundle(server, import_dir, cancel.as_ref(), &mut on_event) {
            Ok(dl) => {
                prog.bytes = dl.size;
                if dl.cancelled {
                    // отменили во время скачивания: полоска врала «сколько
                    // паков готово», а реально не поставлено ни одного
                    prog.done = 0;
                    prog.unpacked = 0;
                    prog.cancelled = true;
                } else {
                    let floor = prog.done; // полоска не проседает на распаковке
                    prog.stage = Stage::Unpacking;
                    prog.current = "unpacking…".into();
                    emit(&tx, &prog);
                    let mut on_group = |id: &str, rep: &ExtractReport| {
                        prog.current = titles.get(id).cloned().unwrap_or_else(|| id.to_string());
                        prog.done = rep.done.max(floor);
                        prog.unpacked = rep.done;
                        prog.failed = rep.failed.clone();
                        emit(&tx, &prog);
                    };
                    match extract_bundle_file(&dl.path, import_dir, &installed, cancel.as_ref(), &mut on_group)
                    {
                        Ok(rep) => {
                            prog.done = rep.done;
                            prog.unpacked = rep.done;
                            prog.failed = rep.failed.clone();
                            prog.cancelled = rep.cancelled;
                            bundle_ok = !rep.cancelled && rep.failed.is_empty();
                            if bundle_ok {
                                // архив целиком разобрали — он больше не нужен
                                let (part, etag_file) = bundle_paths(import_dir);
                                let _ = std::fs::remove_file(&part);
                                let _ = std::fs::remove_file(&etag_file);
                            }
                        }
                        Err(e) => {
                            // битый/чужой архив: .part оставляем (по нему видно,
                            // что скачано), паки добираем по одному
                            prog.failed.push(("bundle".into(), format!("unpack: {e}")));
                        }
                    }
                }
            }
            Err(e) if e.is::<BundleUnsupported>() => { /* старый сервер — по одному */ }
            Err(e) if e.is::<Cancelled>() => prog.cancelled = true,
            Err(e) => prog.failed.push(("bundle".into(), e.to_string())),
        }
    }

    // ---------- добор по одному (старый сервер / сбой бандла) ----------
    let complete = bundle_ok && prog.done + prog.skipped >= catalog.len();
    if !prog.cancelled && !complete {
        installed = installed_set(&catalog, import_dir);
        prog.skipped = installed.len();
        install_each(server, &catalog, import_dir, &mut prog, cancel.as_ref(), &tx);
    }

    prog.finished = true;
    prog.current.clear();
    prog.total = prog.total.max(catalog.len()).max(prog.done + prog.skipped);
    emit(&tx, &prog);
    prog
}

/// Старый путь: паки по одному через `/api/packs/<id>`.
fn install_each(
    server: &str,
    catalog: &[CatalogEntry],
    import_dir: &Path,
    prog: &mut Progress,
    cancel: &AtomicBool,
    tx: &Option<std::sync::mpsc::Sender<Progress>>,
) {
    let server = server.trim_end_matches('/');
    let installed = installed_set(catalog, import_dir);
    // всё, что уже стоит, считается в skipped, поэтому done — только этот добор
    prog.skipped = installed.len();
    prog.done = 0;
    for entry in catalog {
        if cancel.load(Ordering::Relaxed) {
            prog.cancelled = true;
            break;
        }
        if installed.contains(&entry.id) || is_installed(import_dir, &entry.id) {
            continue; // уже посчитан в skipped
        }
        prog.current = entry.title.clone();
        emit(tx, prog);
        match download_pack(server, &entry.id) {
            Ok(bytes) => match install_zip(&bytes, import_dir, &entry.id) {
                Ok(()) => {
                    prog.done += 1;
                    prog.unpacked += 1;
                    prog.bytes += bytes.len() as u64;
                }
                Err(e) => prog.failed.push((entry.id.clone(), format!("unpack: {e}"))),
            },
            Err(e) => prog.failed.push((entry.id.clone(), format!("download: {e}"))),
        }
        emit(tx, prog);
    }
}

/// Можно ли пользоваться бандлом (`DEEMO_PACK_BUNDLE=0` — выключить, отладка).
fn bundle_enabled() -> bool {
    match std::env::var("DEEMO_PACK_BUNDLE") {
        Ok(v) => !(v == "0" || v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("false")),
        Err(_) => true,
    }
}

fn emit(tx: &Option<std::sync::mpsc::Sender<Progress>>, prog: &Progress) {
    if let Some(tx) = tx {
        let _ = tx.send(prog.clone());
    }
}

fn is_installed(import_dir: &Path, id: &str) -> bool {
    import_dir.join(id).is_dir()
}

fn installed_set(catalog: &[CatalogEntry], import_dir: &Path) -> HashSet<String> {
    catalog
        .iter()
        .filter(|e| is_installed(import_dir, &e.id))
        .map(|e| e.id.clone())
        .collect()
}

/// Сколько паков из списка id уже установлено (папка есть) в `import_dir`.
/// Нужно UI, чтобы пересчитать «сколько установлено» без похода в сеть.
pub fn installed_ids<S: AsRef<str>>(ids: &[S], import_dir: &Path) -> usize {
    ids.iter().filter(|id| is_installed(import_dir, id.as_ref())).count()
}

// ---------------------------------------------------------------------------
// бандл: статус, HEAD, докачка
// ---------------------------------------------------------------------------

/// Статус сборки бандла на сервере (`/api/bundle/status`).
#[derive(Clone, Debug, Default)]
pub struct BundleStatus {
    /// `building` | `ready` | `error`.
    pub state: String,
    pub size: Option<u64>,
    pub count: Option<usize>,
    pub bytes: u64,
    pub error: Option<String>,
    /// Необязательная подсказка, как часто опрашивать статус (`retry_after`).
    pub retry_after: Option<u64>,
}

/// События скачивания бандла — из них собирается [`Progress`].
#[derive(Clone, Copy, Debug)]
pub enum BundleEvent {
    /// Сервер ещё собирает архив (`state: building`), известно число паков.
    Building { count: Option<usize> },
    /// Сколько паков в готовом архиве (поле `count` из `/api/bundle/status`).
    Count(usize),
    /// На диске столько-то байт из стольких-то.
    Bytes { on_disk: u64, total: u64 },
}

/// Итог скачивания бандла.
#[derive(Clone, Debug)]
pub struct Downloaded {
    /// `<import_dir>/../.bundle.part`.
    pub path: PathBuf,
    /// Сколько байт сейчас лежит в файле.
    pub size: u64,
    /// Ожидаемый размер (Content-Length).
    pub total: u64,
    /// Нас отменили: файл неполный, но останется для докачки.
    pub cancelled: bool,
    /// Файл скачан целиком.
    pub complete: bool,
}

/// Где лежит недокачанный бандл: рядом с `data/imported`, но не внутри
/// (иначе игра приняла бы `.bundle.part` за пак).
pub fn bundle_paths(import_dir: &Path) -> (PathBuf, PathBuf) {
    let dir = import_dir.parent().unwrap_or(import_dir);
    let part = dir.join(".bundle.part");
    let etag = dir.join(".bundle.part.etag");
    (part, etag)
}

/// `done` для полоски во время скачивания: прогресс по байтам, пересчитанный
/// в «сколько паков как будто уже готово», чтобы полоска ехала, а не стояла на 0.
fn download_done(on_disk: u64, total: u64, packs: usize, skipped: usize) -> usize {
    if total == 0 {
        return 0;
    }
    let left = packs.saturating_sub(skipped);
    let frac = (on_disk.min(total) as f64) / (total as f64);
    ((frac * left as f64) as usize).min(left)
}

/// Статус сборки бандла. `404/501` → [`BundleUnsupported`].
pub fn bundle_status(server: &str) -> Result<BundleStatus> {
    let url = format!("{}{BUNDLE_STATUS_PATH}", server.trim_end_matches('/'));
    let resp = match agent().get(&url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(404 | 501, _)) => return Err(BundleUnsupported.into()),
        Err(e) => return Err(anyhow!("bundle status failed: {e}")),
    };
    let v: serde_json::Value = resp.into_json().context("bundle status is not JSON")?;
    let num = |x: &serde_json::Value| x.as_u64();
    Ok(BundleStatus {
        state: v["state"].as_str().unwrap_or("").to_string(),
        size: num(&v["size"]),
        count: num(&v["count"]).map(|c| c as usize),
        bytes: num(&v["bytes"]).unwrap_or(0),
        error: v["error"].as_str().map(str::to_string),
        retry_after: num(&v["retry_after"]).filter(|n| *n > 0),
    })
}

/// HEAD бандла: (размер, ETag). `404/501` → [`BundleUnsupported`],
/// `503` → [`Building`] с `Retry-After`.
fn bundle_head(server: &str) -> Result<(u64, Option<String>)> {
    let url = format!("{}{BUNDLE_PATH}", server.trim_end_matches('/'));
    match agent().request("HEAD", &url).call() {
        Ok(resp) => {
            let size = resp
                .header("content-length")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .context("bundle HEAD has no Content-Length")?;
            let etag = resp.header("etag").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            Ok((size, etag))
        }
        Err(ureq::Error::Status(404 | 501, _)) => Err(BundleUnsupported.into()),
        Err(ureq::Error::Status(503, resp)) => {
            let retry_after = resp
                .header("retry-after")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(RETRY_AFTER_DEFAULT);
            Err(Building { retry_after }.into())
        }
        Err(ureq::Error::Status(code, _)) => Err(anyhow!("bundle HEAD returned {code}")),
        Err(e) => Err(anyhow!("bundle HEAD failed: {e}")),
    }
}

/// Дождаться готового бандла: размер + ETag. Пока сервер собирает архив
/// (`state: building` / `503` + `Retry-After`), ждём и сообщаем о ожидании.
fn resolve_bundle(
    server: &str,
    cancel: &AtomicBool,
    on: &mut dyn FnMut(BundleEvent),
) -> Result<(u64, Option<String>)> {
    let deadline = std::time::Instant::now() + BUILD_WAIT;
    let mut wait = RETRY_AFTER_DEFAULT;
    loop {
        let st = bundle_status(server)?;
        match st.state.as_str() {
            "ready" => {}
            "building" => {
                if std::time::Instant::now() >= deadline {
                    bail!("bundle is still building after {}s", BUILD_WAIT.as_secs());
                }
                if let Some(r) = st.retry_after {
                    wait = r.max(1);
                }
                on(BundleEvent::Building { count: st.count });
                if sleep_cancel(cancel, std::time::Duration::from_secs(wait)) {
                    return Err(Cancelled.into());
                }
                continue;
            }
            "error" => bail!("server could not build the bundle: {}", st.error.unwrap_or_default()),
            other => bail!("unknown bundle state {other:?}"),
        }
        if let Some(c) = st.count.filter(|c| *c > 0) {
            on(BundleEvent::Count(c));
        }
        match bundle_head(server) {
            Ok((size, etag)) => {
                if size == 0 {
                    bail!("server reports an empty bundle");
                }
                return Ok((size, etag));
            }
            Err(e) if e.is::<Building>() => {
                if let Some(b) = e.downcast_ref::<Building>() {
                    wait = b.retry_after.max(1);
                }
                if std::time::Instant::now() >= deadline {
                    bail!("bundle is still building after {}s", BUILD_WAIT.as_secs());
                }
                on(BundleEvent::Building { count: st.count });
                if sleep_cancel(cancel, std::time::Duration::from_secs(wait)) {
                    return Err(Cancelled.into());
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Спим, но реагируем на отмену. `true` = отменили.
fn sleep_cancel(cancel: &AtomicBool, total: std::time::Duration) -> bool {
    let step = std::time::Duration::from_millis(100);
    let mut left = total;
    while !left.is_zero() {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        let d = left.min(step);
        std::thread::sleep(d);
        left -= d;
    }
    cancel.load(Ordering::Relaxed)
}

/// Решение о докачке: с какого байта продолжать.
///
/// `.part` продолжается только если он не больше целого файла и ETag совпадает
/// (или ETag нет ни там, ни там). Иначе — с нуля.
fn resume_offset(part_len: u64, total: u64, part_etag: Option<&str>, etag: Option<&str>) -> u64 {
    if part_len == 0 || part_len > total {
        return 0;
    }
    let same = match (part_etag, etag) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    };
    if same {
        part_len
    } else {
        0
    }
}

/// Начало из `Content-Range: bytes N-M/total`.
fn content_range_start(value: &str) -> Option<u64> {
    let rest = value.trim().strip_prefix("bytes")?;
    rest.trim_start_matches('=')
        .trim_start()
        .split('-')
        .next()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Скачать бандл в `<import_dir>/../.bundle.part`, продолжая недокачанное.
///
/// При отмене `.part` остаётся: следующий запуск продолжит с того же места.
pub fn download_bundle(
    server: &str,
    import_dir: &Path,
    cancel: &AtomicBool,
    on: &mut dyn FnMut(BundleEvent),
) -> Result<Downloaded> {
    let (part, etag_file) = bundle_paths(import_dir);
    if let Some(parent) = part.parent() {
        std::fs::create_dir_all(parent).context("create bundle dir")?;
    }
    let (size, etag) = resolve_bundle(server, cancel, on)?;

    let on_disk = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let stored_etag = std::fs::read_to_string(&etag_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut offset = resume_offset(on_disk, size, stored_etag.as_deref(), etag.as_deref());

    // ETag текущего файла запоминаем до начала записи: если нас отменят,
    // следующий запуск поймёт, что это тот же архив.
    match &etag {
        Some(e) => {
            let _ = std::fs::write(&etag_file, e);
        }
        None => {
            let _ = std::fs::remove_file(&etag_file);
        }
    }
    if offset == 0 {
        let _ = std::fs::remove_file(&part);
    }
    on(BundleEvent::Bytes { on_disk: offset, total: size });

    let url = format!("{}{BUNDLE_PATH}", server.trim_end_matches('/'));
    let mut attempt = 0;
    'download: while offset < size {
        attempt += 1;
        if attempt > 3 {
            bail!("bundle download keeps restarting");
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(Downloaded { path: part, size: offset, total: size, cancelled: true, complete: false });
        }
        let mut req = download_agent().get(&url);
        if offset > 0 {
            req = req.set("Range", &format!("bytes={offset}-"));
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(404 | 501, _)) => return Err(BundleUnsupported.into()),
            Err(ureq::Error::Status(416, _)) => {
                // «такого диапазона нет» — файл битый/чужой, качаем заново
                let _ = std::fs::remove_file(&part);
                offset = 0;
                on(BundleEvent::Bytes { on_disk: 0, total: size });
                continue 'download;
            }
            Err(ureq::Error::Status(code, _)) => bail!("bundle GET returned {code}"),
            Err(e) => return Err(anyhow!("bundle GET failed: {e}")),
        };
        let mut start = offset;
        if offset > 0 {
            match resp.status() {
                206 => {
                    if let Some(s) = resp.header("content-range").and_then(content_range_start) {
                        if s != offset {
                            // сервер отдал не тот кусок — начинаем сначала
                            let _ = std::fs::remove_file(&part);
                            offset = 0;
                            on(BundleEvent::Bytes { on_disk: 0, total: size });
                            continue 'download;
                        }
                    }
                }
                200 => {
                    // Range проигнорирован: сервер шлёт файл целиком
                    start = 0;
                    let _ = std::fs::remove_file(&part);
                }
                code => bail!("bundle GET returned {code}"),
            }
        }

        let mut file = if start == 0 {
            File::create(&part).context("create bundle part file")?
        } else {
            OpenOptions::new()
                .append(true)
                .open(&part)
                .context("open bundle part file for resume")?
        };
        let mut reader = resp.into_reader();
        let mut buf = vec![0u8; CHUNK];
        let mut written = start;
        let mut emitted = written;
        let mut last = std::time::Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = file.flush();
                return Ok(Downloaded {
                    path: part,
                    size: written,
                    total: size,
                    cancelled: true,
                    complete: false,
                });
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(anyhow!("bundle read failed: {e}")),
            };
            file.write_all(&buf[..n]).context("write bundle part")?;
            written += n as u64;
            if written > size {
                bail!("bundle is larger than Content-Length ({written} > {size})");
            }
            if written - emitted >= PROGRESS_EVERY || last.elapsed() >= PROGRESS_EVERY_TIME {
                on(BundleEvent::Bytes { on_disk: written, total: size });
                emitted = written;
                last = std::time::Instant::now();
            }
        }
        file.flush().context("flush bundle part")?;
        drop(file);
        if written < size {
            bail!("bundle is incomplete: {written} of {size} bytes (kept for resume)");
        }
        offset = written;
    }
    let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if have != size {
        bail!("bundle on disk is {have} bytes, expected {size}");
    }
    on(BundleEvent::Bytes { on_disk: have, total: size });
    Ok(Downloaded { path: part, size: have, total: size, cancelled: false, complete: true })
}

// ---------------------------------------------------------------------------
// потоковая распаковка бандла
// ---------------------------------------------------------------------------

/// Что получилось при распаковке бандла.
#[derive(Clone, Debug, Default)]
pub struct ExtractReport {
    /// Паков поставлено.
    pub done: usize,
    /// Паков пропущено (уже были установлены).
    pub skipped: usize,
    /// Паки, которые не встали: (id, причина).
    pub failed: Vec<(String, String)>,
    /// Нас отменили посреди распаковки (частичный пак удалён).
    pub cancelled: bool,
}

/// Одна запись центрального каталога.
#[derive(Clone, Debug)]
struct CdEntry {
    name: String,
    method: u16,
    comp: u64,
    uncomp: u64,
    local: u64,
}

/// Записи одного пака (подряд идущие `<id>/...`).
struct Group {
    id: String,
    entries: Vec<CdEntry>,
}

/// Распаковать бандл из файла, не читая его целиком в память.
///
/// `installed` — паки, которые уже стоят: их данные даже не распаковываются
/// (просто не читаем; следующая запись всё равно ищется по своему offset).
/// `on_group(id, report)` зовётся после каждого пака — для прогресса в UI.
pub fn extract_bundle_file(
    zip_path: &Path,
    import_dir: &Path,
    installed: &HashSet<String>,
    cancel: &AtomicBool,
    on_group: &mut dyn FnMut(&str, &ExtractReport),
) -> Result<ExtractReport> {
    let mut file = File::open(zip_path)
        .with_context(|| format!("open bundle {}", zip_path.display()))?;
    let len = file.metadata().context("bundle metadata")?.len();
    let entries = read_central_directory(&mut file, len)?;
    let groups = build_groups(entries)?;
    if groups.is_empty() {
        bail!("bundle has no packs");
    }
    std::fs::create_dir_all(import_dir).context("create import dir")?;

    let mut rep = ExtractReport::default();
    for g in &groups {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            break;
        }
        if installed.contains(&g.id) {
            rep.skipped += 1;
            on_group(&g.id, &rep);
            continue;
        }
        let tmp = import_dir.join(format!(".{}.tmp-{}", g.id, std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let result = (|| -> Result<()> {
            std::fs::create_dir_all(&tmp).context("create temp dir")?;
            for e in &g.entries {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Cancelled.into());
                }
                let (_, rel) = e.name.split_once('/').expect("validated entry");
                let dest = safe_join(&tmp, rel)?;
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create {}", parent.display()))?;
                }
                extract_entry(&mut file, e, &dest, len)
                    .with_context(|| format!("extract {}", e.name))?;
            }
            // pack.json обязателен: без манифеста игра не покажет пак
            if !tmp.join("pack.json").is_file() {
                bail!("no pack.json");
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                let dest = import_dir.join(&g.id);
                if dest.exists() {
                    let _ = std::fs::remove_dir_all(&dest);
                }
                std::fs::rename(&tmp, &dest)
                    .with_context(|| format!("move into {}", dest.display()))?;
                rep.done += 1;
            }
            Err(e) if e.is::<Cancelled>() => {
                let _ = std::fs::remove_dir_all(&tmp);
                rep.cancelled = true;
                break;
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                rep.failed.push((g.id.clone(), e.to_string()));
            }
        }
        on_group(&g.id, &rep);
    }
    Ok(rep)
}

/// EOCD + центральный каталог из хвоста файла.
fn read_central_directory(file: &mut File, len: u64) -> Result<Vec<CdEntry>> {
    if len < 22 {
        bail!("not a zip archive (too short)");
    }
    let win = len.min(TAIL_WINDOW + 22);
    file.seek(SeekFrom::Start(len - win)).context("seek bundle tail")?;
    let mut tail = vec![0u8; win as usize];
    file.read_exact(&mut tail).context("read bundle tail")?;
    let pos = find_eocd(&tail).context("not a zip archive (no end-of-central-directory)")?;
    let eocd = len - win + pos as u64;

    let count = u16::from_le_bytes([tail[pos + 10], tail[pos + 11]]) as usize;
    let cd_size = u32::from_le_bytes(rd4(&tail, pos + 12)) as u64;
    let cd_off = u32::from_le_bytes(rd4(&tail, pos + 16)) as u64;
    if count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF {
        bail!("zip64 bundles are not supported");
    }
    if count > MAX_BUNDLE_ENTRIES {
        bail!("bundle has too many entries ({count})");
    }
    if cd_size > MAX_CD {
        bail!("bundle central directory is too big ({cd_size} bytes)");
    }
    let cd_end = cd_off.checked_add(cd_size).context("central directory offset overflow")?;
    if cd_end > eocd {
        bail!("central directory out of bounds");
    }
    file.seek(SeekFrom::Start(cd_off)).context("seek central directory")?;
    let mut cd = vec![0u8; cd_size as usize];
    file.read_exact(&mut cd).context("read central directory")?;

    let mut out = Vec::with_capacity(count);
    let mut p = 0usize;
    for _ in 0..count {
        if p + 46 > cd.len() {
            bail!("truncated central directory");
        }
        if rd4(&cd, p) != [0x50, 0x4b, 0x01, 0x02] {
            bail!("bad central directory signature");
        }
        let flags = u16::from_le_bytes([cd[p + 8], cd[p + 9]]);
        let method = u16::from_le_bytes([cd[p + 10], cd[p + 11]]);
        let comp = u32::from_le_bytes(rd4(&cd, p + 20)) as u64;
        let uncomp = u32::from_le_bytes(rd4(&cd, p + 24)) as u64;
        let name_len = u16::from_le_bytes([cd[p + 28], cd[p + 29]]) as usize;
        let extra_len = u16::from_le_bytes([cd[p + 30], cd[p + 31]]) as usize;
        let comment_len = u16::from_le_bytes([cd[p + 32], cd[p + 33]]) as usize;
        let local = u32::from_le_bytes(rd4(&cd, p + 42)) as u64;
        if p + 46 + name_len > cd.len() {
            bail!("truncated file name");
        }
        if flags & 1 != 0 {
            bail!("encrypted entries are not supported");
        }
        let name = String::from_utf8_lossy(&cd[p + 46..p + 46 + name_len]).to_string();
        p += 46 + name_len + extra_len + comment_len;
        out.push(CdEntry { name, method, comp, uncomp, local });
    }
    Ok(out)
}

/// Проверить раскладку бандла и сгруппировать записи по пакам.
///
/// Всё, что похоже на выход за пределы `import_dir` (`..`, абсолютные пути,
/// `\`, `:`, управляющие символы) — отказ от всего архива.
fn build_groups(entries: Vec<CdEntry>) -> Result<Vec<Group>> {
    let mut groups: Vec<Group> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for e in entries {
        if e.name == "manifest.json" {
            continue; // служебный манифест бандла, в пак не кладём
        }
        if e.name.ends_with('/') {
            continue; // каталог
        }
        if !is_safe_rel(&e.name) {
            bail!("unsafe entry path {:?}", e.name);
        }
        let (id, rel) = e
            .name
            .split_once('/')
            .ok_or_else(|| anyhow!("unexpected top-level entry {:?}", e.name))?;
        if !crate::pack::safe_id(id) {
            bail!("bad pack id {id:?} in {:?}", e.name);
        }
        if !is_safe_rel(rel) {
            bail!("unsafe entry path {:?}", e.name);
        }
        match groups.last_mut() {
            Some(g) if g.id == id => g.entries.push(e),
            _ => {
                if !seen.insert(id.to_string()) {
                    bail!("pack {id:?} appears in more than one place");
                }
                groups.push(Group { id: id.to_string(), entries: vec![e] });
            }
        }
    }
    for g in &groups {
        if g.entries.len() > MAX_ENTRIES {
            bail!("pack {} has too many entries ({})", g.id, g.entries.len());
        }
        let mut total = 0u64;
        for e in &g.entries {
            if e.uncomp > MAX_UNPACKED {
                bail!("{} in pack {} unpacks to {} bytes", e.name, g.id, e.uncomp);
            }
            total = total.saturating_add(e.uncomp);
        }
        if total > MAX_UNPACKED {
            bail!("pack {} unpacks to more than {MAX_UNPACKED} bytes", g.id);
        }
    }
    Ok(groups)
}

/// Распаковать одну запись в `dest` кусками по 64 КиБ.
fn extract_entry(file: &mut File, e: &CdEntry, dest: &Path, len: u64) -> Result<()> {
    file.seek(SeekFrom::Start(e.local)).context("seek local header")?;
    let mut hdr = [0u8; 30];
    file.read_exact(&mut hdr).context("read local header")?;
    if hdr[0..4] != [0x50, 0x4b, 0x03, 0x04] {
        bail!("bad local header");
    }
    let lname = u16::from_le_bytes([hdr[26], hdr[27]]) as u64;
    let lextra = u16::from_le_bytes([hdr[28], hdr[29]]) as u64;
    let data_at = e.local + 30 + lname + lextra;
    if data_at.saturating_add(e.comp) > len {
        bail!("truncated data");
    }
    file.seek(SeekFrom::Start(data_at)).context("seek entry data")?;
    let mut out = BufWriter::with_capacity(CHUNK, File::create(dest).context("create file")?);
    let written = match e.method {
        0 => {
            if e.comp != e.uncomp {
                bail!("stored entry has different sizes");
            }
            let mut r = BufReader::with_capacity(CHUNK, Read::take(&mut *file, e.comp));
            let n = copy_bounded(&mut r, &mut out, e.comp)?;
            n
        }
        8 => {
            let raw = BufReader::with_capacity(CHUNK, Read::take(&mut *file, e.comp));
            let mut d = flate2::read::DeflateDecoder::new(raw);
            // +1 байт: если распаковалось больше заявленного — это бомба
            copy_bounded(&mut d, &mut out, e.uncomp + 1)?
        }
        other => bail!("unsupported compression method {other}"),
    };
    out.flush().context("flush file")?;
    if written != e.uncomp {
        bail!("size mismatch: {written} != {}", e.uncomp);
    }
    Ok(())
}

/// Скопировать не больше `left` байт из `r` в `w` кусками по 64 КиБ.
fn copy_bounded<R: Read, W: Write>(r: &mut R, w: &mut W, mut left: u64) -> Result<u64> {
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    while left > 0 {
        let want = left.min(CHUNK as u64) as usize;
        let n = match r.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("read entry"),
        };
        w.write_all(&buf[..n]).context("write entry")?;
        total += n as u64;
        left -= n as u64;
    }
    Ok(total)
}

/// Безопасный относительный путь внутри пака: без `..`, `/` в начале, `\`, `:`.
fn is_safe_rel(name: &str) -> bool {
    if name.is_empty() || name.len() > 512 {
        return false;
    }
    if name.starts_with('/') || name.contains('\\') || name.contains(':') {
        return false;
    }
    if name.chars().any(|c| c.is_control()) {
        return false;
    }
    name.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

fn rd4(b: &[u8], at: usize) -> [u8; 4] {
    [b[at], b[at + 1], b[at + 2], b[at + 3]]
}

/// Ищем `PK\x05\x06` с конца (перед ним может быть zip-комментарий).
fn find_eocd(b: &[u8]) -> Option<usize> {
    if b.len() < 22 {
        return None;
    }
    let stop = b.len().saturating_sub(22);
    let start = stop.saturating_sub(65535);
    (start..=stop)
        .rev()
        .find(|&i| b[i] == 0x50 && b[i + 1] == 0x4b && b[i + 2] == 0x05 && b[i + 3] == 0x06)
}

// ---------------------------------------------------------------------------
// старый путь: пак = отдельный zip
// ---------------------------------------------------------------------------

fn download_pack(server: &str, id: &str) -> Result<Vec<u8>> {
    let url = format!("{server}/api/packs/{id}");
    let resp = agent()
        .get(&url)
        .call()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut buf)
        .context("read response body")?;
    if buf.is_empty() {
        bail!("empty response");
    }
    Ok(buf)
}

/// Распаковать `.deemopack` (zip) в `<import_dir>/<id>/` атомарно.
pub fn install_zip(bytes: &[u8], import_dir: &Path, id: &str) -> Result<()> {
    if !crate::pack::safe_id(id) {
        bail!("unsafe pack id {id:?}");
    }
    let entries = unzip(bytes)?;
    if entries.is_empty() {
        bail!("empty archive");
    }
    std::fs::create_dir_all(import_dir).context("create import dir")?;
    let tmp = import_dir.join(format!(".{id}.tmp-{}", std::process::id()));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    std::fs::create_dir_all(&tmp).context("create temp dir")?;
    let result = (|| -> Result<()> {
        for (name, data) in &entries {
            let dest = safe_join(&tmp, name)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, data).with_context(|| format!("write {name}"))?;
        }
        // pack.json обязателен: без манифеста игра не покажет пак как установленный
        if !tmp.join("pack.json").is_file() {
            bail!("archive has no pack.json");
        }
        Ok(())
    })();
    if let Err(e) = result {
        std::fs::remove_dir_all(&tmp).ok();
        return Err(e);
    }
    let dest = import_dir.join(id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).ok();
    }
    std::fs::rename(&tmp, &dest).with_context(|| format!("move into {}", dest.display()))?;
    Ok(())
}

/// Относительный путь внутри пака → безопасный абсолютный (без `..`, `/`, `\`).
fn safe_join(root: &Path, name: &str) -> Result<PathBuf> {
    let norm = name.replace('\\', "/");
    if norm.starts_with('/') || norm.contains("..") || norm.contains(':') {
        bail!("unsafe zip path {name:?}");
    }
    let mut path = root.to_path_buf();
    for part in norm.split('/').filter(|p| !p.is_empty()) {
        path.push(part);
    }
    Ok(path)
}

/// Минимальный zip-ридер: метод 0 (store) и 8 (deflate), без zip64.
///
/// Читает центральный каталог (истина о размерах и именах), затем локальные
/// заголовки — так же, как это делает любой распаковщик.
pub fn unzip(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let eocd = find_eocd(bytes).context("not a zip archive (no end-of-central-directory)")?;
    let count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let cd_size = u32::from_le_bytes(rd4(bytes, eocd + 12)) as usize;
    let cd_off = u32::from_le_bytes(rd4(bytes, eocd + 16)) as usize;
    if count > MAX_ENTRIES {
        bail!("archive has too many entries ({count})");
    }
    if cd_off + cd_size > bytes.len() {
        bail!("central directory out of bounds");
    }
    let mut out = Vec::with_capacity(count);
    let mut p = cd_off;
    let mut total_unpacked = 0u64;
    for _ in 0..count {
        if p + 46 > bytes.len() {
            bail!("truncated central directory");
        }
        if rd4(bytes, p) != [0x50, 0x4b, 0x01, 0x02] {
            bail!("bad central directory signature");
        }
        let method = u16::from_le_bytes([bytes[p + 10], bytes[p + 11]]);
        let comp = u32::from_le_bytes(rd4(bytes, p + 20)) as usize;
        let uncomp = u32::from_le_bytes(rd4(bytes, p + 24)) as u64;
        let name_len = u16::from_le_bytes([bytes[p + 28], bytes[p + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[p + 30], bytes[p + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[p + 32], bytes[p + 33]]) as usize;
        let local_off = u32::from_le_bytes(rd4(bytes, p + 42)) as usize;
        if p + 46 + name_len > bytes.len() {
            bail!("truncated file name");
        }
        let name = String::from_utf8_lossy(&bytes[p + 46..p + 46 + name_len]).to_string();
        p += 46 + name_len + extra_len + comment_len;

        total_unpacked += uncomp;
        if total_unpacked > MAX_UNPACKED {
            bail!("archive unpacks to more than {MAX_UNPACKED} bytes");
        }
        if name.ends_with('/') {
            continue; // каталог
        }
        if local_off + 30 > bytes.len() {
            bail!("bad local header offset for {name:?}");
        }
        if rd4(bytes, local_off) != [0x50, 0x4b, 0x03, 0x04] {
            bail!("bad local header signature for {name:?}");
        }
        let lname = u16::from_le_bytes([bytes[local_off + 26], bytes[local_off + 27]]) as usize;
        let lextra = u16::from_le_bytes([bytes[local_off + 28], bytes[local_off + 29]]) as usize;
        let data_at = local_off + 30 + lname + lextra;
        if data_at + comp > bytes.len() {
            bail!("truncated data for {name:?}");
        }
        let raw = &bytes[data_at..data_at + comp];
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut out_buf = Vec::with_capacity(uncomp as usize);
                flate2::read::DeflateDecoder::new(raw)
                    .take(MAX_UNPACKED)
                    .read_to_end(&mut out_buf)
                    .with_context(|| format!("inflate {name:?}"))?;
                out_buf
            }
            other => bail!("unsupported compression method {other} for {name:?}"),
        };
        if data.len() as u64 != uncomp {
            bail!("size mismatch for {name:?}: {} != {uncomp}", data.len());
        }
        out.push((name, data));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader as StdBufReader};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Собрать zip в памяти: (имя, данные, deflate?).
    fn make_zip(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data, deflate) in files {
            let (method, payload) = if *deflate {
                let mut enc =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                enc.write_all(data).unwrap();
                (8u16, enc.finish().unwrap())
            } else {
                (0u16, data.to_vec())
            };
            let crc = crc32fast::hash(data);
            let off = out.len() as u32;
            out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&payload);
            central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&off.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_off = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// Синтетический бандл по контракту сервера: manifest.json + <id>/...
    fn make_bundle(packs: &[(&str, Vec<(&str, Vec<u8>, bool)>)]) -> Vec<u8> {
        let mut files: Vec<(String, Vec<u8>, bool)> = Vec::new();
        files.push((
            "manifest.json".to_string(),
            br#"{"format":1,"packs":[]}"#.to_vec(),
            true,
        ));
        for (id, entries) in packs {
            for (rel, data, deflate) in entries {
                files.push((format!("{id}/{rel}"), data.clone(), *deflate));
            }
        }
        let refs: Vec<(&str, &[u8], bool)> = files
            .iter()
            .map(|(n, d, z)| (n.as_str(), d.as_slice(), *z))
            .collect();
        make_zip(&refs)
    }

    fn pack_files(id: &str, title: &str) -> Vec<(&'static str, Vec<u8>, bool)> {
        vec![
            (
                "pack.json",
                format!(
                    r#"{{"format_version":1,"id":"{id}","title":"{title}","composer":"C","charts":[{{"diff":"hard","path":"charts/hard.json"}}],"audio":"audio.ogg","cover":"cover.png"}}"#
                )
                .into_bytes(),
                true,
            ),
            ("charts/hard.json", br#"{"notes":[],"links":[]}"#.to_vec(), true),
            ("audio.ogg", vec![7u8; 40_000], false),
            ("cover.png", vec![1u8; 512], false),
        ]
    }

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("deemo-pd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn hidden(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with('.'))
            .collect()
    }

    // ------------------------- старые тесты -------------------------

    #[test]
    fn unzips_stored_and_deflated_entries() {
        let chart = br#"{"speed":6.0,"notes":[],"links":[]}"#;
        let audio = vec![7u8; 4096];
        let zip = make_zip(&[
            ("pack.json", br#"{"id":"x","title":"X","charts":[]}"#, true),
            ("charts/hard.json", chart.as_slice(), true),
            ("audio.ogg", &audio, false),
        ]);
        let entries = unzip(&zip).unwrap();
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["pack.json", "charts/hard.json", "audio.ogg"]);
        assert_eq!(entries[1].1, chart.to_vec());
        assert_eq!(entries[2].1, audio);
    }

    #[test]
    fn install_zip_writes_into_import_dir_atomically() {
        let root = temp_root("zip");
        let zip = make_zip(&[
            ("pack.json", br#"{"id":"demo","title":"Demo","charts":[]}"#, true),
            ("charts/easy.json", b"{}", true),
        ]);
        install_zip(&zip, &root, "demo").unwrap();
        assert!(root.join("demo/pack.json").is_file());
        assert!(root.join("demo/charts/easy.json").is_file());
        assert!(!root.join("demo.charts").exists());
        assert!(hidden(&root).is_empty(), "temp dirs left: {:?}", hidden(&root));
        install_zip(&zip, &root, "demo").unwrap();
        assert!(root.join("demo/pack.json").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_traversal_and_missing_manifest() {
        let root = temp_root("evil");
        let zip = make_zip(&[("../evil.txt", b"x", true)]);
        assert!(install_zip(&zip, &root, "evil").is_err());
        assert!(!root.join("evil").exists());
        let zip = make_zip(&[("charts/easy.json", b"{}", true)]);
        assert!(install_zip(&zip, &root, "nomanifest").is_err());
        assert!(!root.join("nomanifest").exists());
        assert!(install_zip(&zip, &root, "../bad").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn progress_percent_is_stable() {
        let mut p = Progress::default();
        assert_eq!(p.percent(), 0.0);
        p.total = 4;
        p.done = 1;
        p.skipped = 1;
        assert!((p.percent() - 50.0).abs() < f32::EPSILON);
        // бегущая оценка по байтам не должна вылезать за 100 %
        p.done = 10;
        assert_eq!(p.percent(), 100.0);
    }

    #[test]
    fn counts_installed_dirs_of_a_catalog() {
        let root = temp_root("count");
        std::fs::create_dir_all(root.join("one")).unwrap();
        std::fs::create_dir_all(root.join("three")).unwrap();
        std::fs::write(root.join("two"), b"not a dir").unwrap();
        let ids = vec!["one".to_string(), "two".to_string(), "three".to_string(), "gone".to_string()];
        assert_eq!(installed_ids(&ids, &root), 2);
        assert_eq!(installed_ids::<String>(&[], &root), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ------------------------- распаковка бандла -------------------------

    #[test]
    fn extracts_bundle_packs_and_skips_installed() {
        let root = temp_root("bundle-extract");
        let import = root.join("imported");
        let zip = make_bundle(&[
            ("alpha", pack_files("alpha", "Alpha")),
            ("beta", pack_files("beta", "Beta")),
        ]);
        let path = root.join("bundle.zip");
        std::fs::write(&path, &zip).unwrap();
        // beta уже стоит
        std::fs::create_dir_all(import.join("beta")).unwrap();
        std::fs::write(import.join("beta/pack.json"), b"{}").unwrap();
        std::fs::write(import.join("beta/marker"), b"keep").unwrap();
        let installed: HashSet<String> = ["beta".to_string()].into_iter().collect();
        let cancel = AtomicBool::new(false);
        let mut seen: Vec<String> = Vec::new();
        let rep = extract_bundle_file(&path, &import, &installed, &cancel, &mut |id, _| {
            seen.push(id.to_string())
        })
        .unwrap();

        assert_eq!(rep.done, 1, "one pack installed");
        assert_eq!(rep.skipped, 1, "beta skipped");
        assert!(rep.failed.is_empty(), "failures: {:?}", rep.failed);
        assert!(!rep.cancelled);
        assert_eq!(seen, vec!["alpha", "beta"]);

        assert!(import.join("alpha/pack.json").is_file());
        assert!(import.join("alpha/charts/hard.json").is_file());
        assert_eq!(std::fs::read(import.join("alpha/audio.ogg")).unwrap(), vec![7u8; 40_000]);
        assert!(import.join("alpha/cover.png").is_file());
        // установленный бета не тронут
        assert!(import.join("beta/marker").is_file());
        assert_eq!(std::fs::read(import.join("beta/pack.json")).unwrap(), b"{}");
        // временных папок не осталось
        assert!(hidden(&import).is_empty(), "leftovers: {:?}", hidden(&import));
        // сам .part распаковка не удаляет — это делает install_all
        assert!(path.is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bundle_rejects_traversal_and_bad_paths() {
        let root = temp_root("bundle-evil");
        let import = root.join("imported");
        let cancel = AtomicBool::new(false);
        let installed = HashSet::new();
        for bad in ["../evil.txt", "/etc/passwd", "alpha/../../x", "alpha\\x", "alpha/a:b", "top.txt"] {
            // хороший пак + одна плохая запись
            let files = vec![
                ("manifest.json", br#"{}"#.to_vec(), true),
                ("alpha/pack.json", b"{}".to_vec(), true),
                (bad, b"x".to_vec(), true),
            ];
            let refs: Vec<(&str, &[u8], bool)> =
                files.iter().map(|(n, d, z)| (*n, d.as_slice(), *z)).collect();
            let bad_zip = make_zip(&refs);
            let path = root.join("bad.zip");
            std::fs::write(&path, &bad_zip).unwrap();
            assert!(
                extract_bundle_file(&path, &import, &installed, &cancel, &mut |_, _| {}).is_err(),
                "entry {bad:?} must be rejected"
            );
            assert!(!import.join("evil.txt").exists());
            assert!(!import.join("alpha").exists(), "nothing installed for {bad:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bundle_pack_without_manifest_fails_but_others_install() {
        let root = temp_root("bundle-nomanifest");
        let import = root.join("imported");
        let zip = make_bundle(&[
            ("good", pack_files("good", "Good")),
            ("nopack", vec![("charts/hard.json", b"{}".to_vec(), true)]),
        ]);
        let path = root.join("bundle.zip");
        std::fs::write(&path, &zip).unwrap();
        let cancel = AtomicBool::new(false);
        let rep = extract_bundle_file(&path, &import, &HashSet::new(), &cancel, &mut |_, _| {})
            .unwrap();
        assert_eq!(rep.done, 1);
        assert_eq!(rep.failed.len(), 1);
        assert_eq!(rep.failed[0].0, "nopack");
        assert!(import.join("good/pack.json").is_file());
        assert!(!import.join("nopack").exists());
        assert!(hidden(&import).is_empty(), "leftovers: {:?}", hidden(&import));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bundle_extraction_cancels_without_partial_pack() {
        let root = temp_root("bundle-cancel");
        let import = root.join("imported");
        let zip = make_bundle(&[
            ("one", pack_files("one", "One")),
            ("two", pack_files("two", "Two")),
        ]);
        let path = root.join("bundle.zip");
        std::fs::write(&path, &zip).unwrap();
        let cancel = AtomicBool::new(false);
        let rep = extract_bundle_file(&path, &import, &HashSet::new(), &cancel, &mut |id, _| {
            if id == "one" {
                cancel.store(true, Ordering::Relaxed); // отменяем после первого пака
            }
        })
        .unwrap();
        assert!(rep.cancelled);
        assert_eq!(rep.done, 1);
        assert!(import.join("one/pack.json").is_file());
        assert!(!import.join("two").exists());
        assert!(hidden(&import).is_empty(), "leftovers: {:?}", hidden(&import));
        let _ = std::fs::remove_dir_all(&root);
    }

    // ------------------------- resume / Range -------------------------

    #[test]
    fn resume_offset_follows_etag() {
        // пустой .part — всегда с нуля
        assert_eq!(resume_offset(0, 1000, Some("a"), Some("a")), 0);
        // совпал ETag — продолжаем
        assert_eq!(resume_offset(400, 1000, Some("a"), Some("a")), 400);
        // ETag другой — заново
        assert_eq!(resume_offset(400, 1000, Some("a"), Some("b")), 0);
        // у одной стороны ETag нет — не доверяем
        assert_eq!(resume_offset(400, 1000, Some("a"), None), 0);
        assert_eq!(resume_offset(400, 1000, None, Some("a")), 0);
        // без ETag вообще продолжаем (нечем проверить)
        assert_eq!(resume_offset(400, 1000, None, None), 400);
        // файл больше целого — мусор
        assert_eq!(resume_offset(1200, 1000, Some("a"), Some("a")), 0);
        // ровно целый — докачивать нечего
        assert_eq!(resume_offset(1000, 1000, Some("a"), Some("a")), 1000);
    }

    #[test]
    fn parses_content_range_start() {
        assert_eq!(content_range_start("bytes 100-199/1000"), Some(100));
        assert_eq!(content_range_start("bytes=0-"), Some(0));
        assert_eq!(content_range_start("bytes 0-0/1"), Some(0));
        assert_eq!(content_range_start("items 5-6/9"), None);
        assert_eq!(content_range_start(""), None);
    }

    #[test]
    fn download_progress_is_smooth_and_capped() {
        assert_eq!(download_done(0, 1000, 10, 0), 0);
        assert_eq!(download_done(500, 1000, 10, 0), 5);
        assert_eq!(download_done(1000, 1000, 10, 0), 10);
        // всё уже стоит — делить нечего
        assert_eq!(download_done(500, 1000, 10, 10), 0);
        // неизвестный размер — без оценки
        assert_eq!(download_done(500, 0, 10, 0), 0);
        // с пропусками оценка идёт только по оставшимся пакам
        assert_eq!(download_done(500, 1000, 10, 4), 3);
    }

    #[test]
    fn bundle_part_lives_next_to_import_dir() {
        let (part, etag) = bundle_paths(Path::new("/tmp/x/data/imported"));
        assert_eq!(part, Path::new("/tmp/x/data/.bundle.part"));
        assert_eq!(etag, Path::new("/tmp/x/data/.bundle.part.etag"));
    }

    // ------------------------- мини-сервер для e2e -------------------------

    #[derive(Clone, Debug)]
    struct Hit {
        method: String,
        path: String,
        range: Option<String>,
    }

    struct Mock {
        url: String,
        hits: Arc<Mutex<Vec<Hit>>>,
        stop: Arc<AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl Mock {
        fn start(routes: Routes) -> Mock {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let hits = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let (h2, s2) = (hits.clone(), stop.clone());
            let join = std::thread::spawn(move || {
                while !s2.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = handle(stream, &routes, &h2);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            Mock { url, hits, stop, join: Some(join) }
        }

        fn hits(&self) -> Vec<Hit> {
            self.hits.lock().unwrap().clone()
        }

        fn bundle_gets(&self) -> Vec<Hit> {
            self.hits()
                .into_iter()
                .filter(|h| h.method == "GET" && h.path == BUNDLE_PATH)
                .collect()
        }

        fn paths(&self) -> Vec<String> {
            self.hits().into_iter().map(|h| h.path).collect()
        }
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    struct Routes {
        catalog: String,
        bundle: Option<Vec<u8>>,
        etag: String,
        status: Option<String>,
        ignore_range: bool,
        packs: HashMap<String, Vec<u8>>,
        /// HEAD отвечает 503, пока не истечёт это число запросов.
        building_heads: std::sync::atomic::AtomicUsize,
        /// `/api/bundle/status` отвечает `building`, пока счётчик не кончится.
        building_status: std::sync::atomic::AtomicUsize,
    }

    impl Routes {
        fn new(bundle: Vec<u8>, count: usize) -> Routes {
            let catalog: Vec<String> = (0..count)
                .map(|i| format!(r#"{{"id":"p{i}","title":"P{i}","owner":null}}"#))
                .collect();
            Routes {
                catalog: format!(r#"{{"count":{count},"packs":[{}]}}"#, catalog.join(",")),
                status: Some(format!(
                    r#"{{"state":"ready","size":{},"count":{count},"bytes":{},"error":null}}"#,
                    bundle.len(),
                    bundle.len()
                )),
                etag: "\"bundle-1\"".into(),
                bundle: Some(bundle),
                ignore_range: false,
                packs: HashMap::new(),
                building_heads: std::sync::atomic::AtomicUsize::new(0),
                building_status: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    fn handle(mut s: TcpStream, r: &Routes, hits: &Mutex<Vec<Hit>>) -> std::io::Result<()> {
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut rd = StdBufReader::new(s.try_clone()?);
        let mut line = String::new();
        if rd.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("/").to_string();
        let mut range = None;
        loop {
            let mut h = String::new();
            if rd.read_line(&mut h)? == 0 {
                break;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.eq_ignore_ascii_case("range") {
                    range = Some(v.trim().to_string());
                }
            }
        }
        hits.lock().unwrap().push(Hit { method: method.clone(), path: path.clone(), range: range.clone() });
        let (code, headers, body) = route(r, &method, &path, range.as_deref());
        let mut clen = body.len();
        let mut extra = String::new();
        for (k, v) in &headers {
            if k.eq_ignore_ascii_case("content-length") {
                clen = v.parse().unwrap_or(body.len());
            } else {
                extra.push_str(&format!("{k}: {v}\r\n"));
            }
        }
        let head = format!(
            "HTTP/1.1 {code} {}\r\nConnection: close\r\nContent-Length: {clen}\r\n{extra}\r\n",
            reason(code)
        );
        s.write_all(head.as_bytes())?;
        if method != "HEAD" {
            s.write_all(&body)?;
        }
        s.flush()?;
        Ok(())
    }

    fn json() -> Vec<(String, String)> {
        vec![("Content-Type".to_string(), "application/json".to_string())]
    }

    fn route(
        r: &Routes,
        method: &str,
        path: &str,
        range: Option<&str>,
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        if path == "/api/packs" {
            return (200, json(), r.catalog.clone().into_bytes());
        }
        if let Some(id) = path.strip_prefix("/api/packs/") {
            return match r.packs.get(id) {
                Some(b) => (200, json(), b.clone()),
                None => (404, json(), b"{}".to_vec()),
            };
        }
        if path == BUNDLE_STATUS_PATH {
            let building = r
                .building_status
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    if n > 0 { Some(n - 1) } else { None }
                })
                .is_ok();
            if building {
                return (
                    200,
                    json(),
                    br#"{"state":"building","size":null,"count":1,"bytes":0,"error":null,"retry_after":1}"#
                        .to_vec(),
                );
            }
            return match &r.status {
                Some(j) => (200, json(), j.clone().into_bytes()),
                None => (404, json(), b"{}".to_vec()),
            };
        }
        if path == BUNDLE_PATH {
            let Some(b) = &r.bundle else {
                return (404, json(), b"{}".to_vec());
            };
            let mut hdrs = vec![
                ("Content-Type".to_string(), "application/zip".to_string()),
                ("ETag".to_string(), r.etag.clone()),
                ("Accept-Ranges".to_string(), "bytes".to_string()),
            ];
            if method == "HEAD" {
                let left = r
                    .building_heads
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                        if n > 0 { Some(n - 1) } else { None }
                    })
                    .is_ok();
                if left {
                    hdrs.push(("Retry-After".to_string(), "1".to_string()));
                    return (503, hdrs, Vec::new());
                }
                hdrs.push(("Content-Length".to_string(), b.len().to_string()));
                return (200, hdrs, Vec::new());
            }
            if let Some(rng) = range.filter(|_| !r.ignore_range) {
                if let Some(start) = rng
                    .strip_prefix("bytes=")
                    .and_then(|s| s.split('-').next())
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    let start = start.min(b.len() as u64) as usize;
                    let last_byte = b.len() - 1;
                    hdrs.push((
                        "Content-Range".to_string(),
                        format!("bytes {start}-{last_byte}/{}", b.len()),
                    ));
                    return (206, hdrs, b[start..].to_vec());
                }
            }
            return (200, hdrs, b.clone());
        }
        (404, json(), b"{}".to_vec())
    }

    fn reason(code: u16) -> &'static str {
        match code {
            200 => "OK",
            206 => "Partial Content",
            404 => "Not Found",
            503 => "Service Unavailable",
            _ => "Error",
        }
    }

    fn installed_manifest(dir: &Path, id: &str) -> bool {
        let p = dir.join(id).join("pack.json");
        p.is_file() && crate::pack::manifest_of(&dir.join(id)).is_some()
    }

    // ------------------------- e2e: install_all через бандл -------------------------

    #[test]
    fn install_all_downloads_bundle_extracts_and_cleans_up() {
        let root = temp_root("e2e-bundle");
        let import = root.join("imported");
        std::fs::create_dir_all(&import).unwrap();
        let bundle = make_bundle(&[
            ("p0", pack_files("p0", "P0")),
            ("p1", pack_files("p1", "P1")),
        ]);
        let mock = Mock::start(Routes::new(bundle.clone(), 2));
        // p1 уже стоит до запуска
        std::fs::create_dir_all(import.join("p1")).unwrap();
        std::fs::write(import.join("p1/pack.json"), bundle_pack_json("p1", "P1")).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let prog = install_all(&mock.url, &import, cancel.clone(), Some(tx));

        assert_eq!(prog.total, 2);
        assert_eq!(prog.skipped, 1, "p1 must be skipped");
        assert_eq!(prog.done, 1, "p0 must be installed");
        assert_eq!(prog.unpacked, 1, "unpacked counts the packs written this run");
        assert!(prog.failed.is_empty(), "failures: {:?}", prog.failed);
        assert!(prog.finished);
        assert!(!prog.cancelled);
        assert_eq!(prog.bytes, bundle.len() as u64, "bytes = size of the bundle");
        assert!(installed_manifest(&import, "p0"));
        assert!(import.join("p0/charts/hard.json").is_file());
        assert!(import.join("p0/audio.ogg").is_file());
        assert!(installed_manifest(&import, "p1"));
        // .part и .etag убраны после успешной распаковки
        assert!(!root.join(".bundle.part").exists(), ".part not cleaned up");
        assert!(!root.join(".bundle.part.etag").exists(), ".etag not cleaned up");
        assert!(hidden(&import).is_empty(), "leftovers: {:?}", hidden(&import));
        // скачивали один раз и без Range
        let gets = mock.bundle_gets();
        assert_eq!(gets.len(), 1, "one bundle GET, got {gets:?}");
        assert!(gets[0].range.is_none(), "fresh download must not send Range");
        // прогресс приходил в канал
        let mut seen = 0;
        while let Ok(_p) = rx.try_recv() {
            seen += 1;
        }
        assert!(seen > 3, "progress messages expected, got {seen}");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn bundle_pack_json(id: &str, title: &str) -> Vec<u8> {
        format!(
            r#"{{"format_version":1,"id":"{id}","title":"{title}","composer":"C","charts":[{{"diff":"hard","path":"charts/hard.json"}}],"audio":"audio.ogg"}}"#
        )
        .into_bytes()
    }

    #[test]
    fn install_all_resumes_from_part_with_matching_etag() {
        let root = temp_root("e2e-resume");
        let import = root.join("imported");
        std::fs::create_dir_all(&import).unwrap();
        let bundle = make_bundle(&[
            ("p0", pack_files("p0", "P0")),
            ("p1", pack_files("p1", "P1")),
        ]);
        let (part, etag_file) = bundle_paths(&import);
        let first = bundle.len() / 2;
        std::fs::write(&part, &bundle[..first]).unwrap();
        std::fs::write(&etag_file, "\"bundle-1\"").unwrap();

        let mock = Mock::start(Routes::new(bundle.clone(), 2));
        let cancel = Arc::new(AtomicBool::new(false));
        let prog = install_all(&mock.url, &import, cancel, None);
        assert_eq!(prog.done, 2, "both packs from resumed bundle");
        assert!(prog.failed.is_empty(), "{:?}", prog.failed);
        assert!(installed_manifest(&import, "p0") && installed_manifest(&import, "p1"));
        let gets = mock.bundle_gets();
        assert_eq!(gets.len(), 1);
        assert_eq!(
            gets[0].range.as_deref(),
            Some(format!("bytes={first}-").as_str()),
            "must resume from {first}"
        );
        assert!(!part.exists() && !etag_file.exists(), "part/etag must be gone");
        assert!(hidden(&import).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_all_restarts_when_etag_changed() {
        let root = temp_root("e2e-etag");
        let import = root.join("imported");
        std::fs::create_dir_all(&import).unwrap();
        let bundle = make_bundle(&[("p0", pack_files("p0", "P0"))]);
        let (part, etag_file) = bundle_paths(&import);
        std::fs::write(&part, &bundle[..bundle.len() / 3]).unwrap();
        std::fs::write(&etag_file, "\"old-bundle\"").unwrap();

        let mock = Mock::start(Routes::new(bundle.clone(), 1));
        let prog = install_all(&mock.url, &import, Arc::new(AtomicBool::new(false)), None);
        assert_eq!(prog.done, 1);
        let gets = mock.bundle_gets();
        assert_eq!(gets.len(), 1);
        assert!(gets[0].range.is_none(), "changed ETag must restart from 0");
        assert!(installed_manifest(&import, "p0"));
        assert!(!part.exists() && !etag_file.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_all_survives_server_ignoring_range() {
        let root = temp_root("e2e-200");
        let import = root.join("imported");
        std::fs::create_dir_all(&import).unwrap();
        let bundle = make_bundle(&[("p0", pack_files("p0", "P0"))]);
        let (part, etag_file) = bundle_paths(&import);
        std::fs::write(&part, &bundle[..bundle.len() / 2]).unwrap();
        std::fs::write(&etag_file, "\"bundle-1\"").unwrap();

        let mut routes = Routes::new(bundle.clone(), 1);
        routes.ignore_range = true; // на Range сервер отвечает 200 и шлёт всё
        let mock = Mock::start(routes);
        let prog = install_all(&mock.url, &import, Arc::new(AtomicBool::new(false)), None);
        assert_eq!(prog.done, 1);
        assert!(prog.failed.is_empty(), "{:?}", prog.failed);
        assert_eq!(prog.bytes, bundle.len() as u64, "full body after restart");
        assert!(installed_manifest(&import, "p0"));
        let gets = mock.bundle_gets();
        assert_eq!(gets.len(), 1, "one GET that fell back to 200: {gets:?}");
        assert!(gets[0].range.is_some(), "the request did ask for a range");
        assert!(!part.exists() && !etag_file.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_all_falls_back_to_per_pack_when_bundle_missing() {
        let root = temp_root("e2e-fallback");
        let import = root.join("imported");
        std::fs::create_dir_all(&import).unwrap();
        let pack0 = make_zip(&[
            ("pack.json", &bundle_pack_json("p0", "P0"), true),
            ("charts/hard.json", b"{}", true),
        ]);
        let pack1 = make_zip(&[
            ("pack.json", &bundle_pack_json("p1", "P1"), true),
            ("charts/hard.json", b"{}", true),
        ]);
        let mut routes = Routes::new(Vec::new(), 2);
        routes.bundle = None; // 404 на бандл
        routes.status = None; // 404 на статус
        routes.packs.insert("p0".into(), pack0.clone());
        routes.packs.insert("p1".into(), pack1.clone());
        let mock = Mock::start(routes);

        let prog = install_all(&mock.url, &import, Arc::new(AtomicBool::new(false)), None);
        assert_eq!(prog.done, 2, "fallback installs both: {:?}", prog.failed);
        assert!(prog.failed.is_empty(), "{:?}", prog.failed);
        assert_eq!(prog.bytes, (pack0.len() + pack1.len()) as u64);
        assert!(installed_manifest(&import, "p0") && installed_manifest(&import, "p1"));
        let paths = mock.paths();
        assert!(paths.iter().any(|p| p == "/api/packs/p0"), "per-pack fallback: {paths:?}");
        assert!(
            !paths.iter().any(|p| p == BUNDLE_PATH),
            "no bundle download when endpoint is missing: {paths:?}"
        );
        assert!(!root.join(".bundle.part").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_all_skips_download_when_everything_installed() {
        let root = temp_root("e2e-allinstalled");
        let import = root.join("imported");
        std::fs::create_dir_all(&import).unwrap();
        for id in ["p0", "p1"] {
            std::fs::create_dir_all(import.join(id)).unwrap();
            std::fs::write(import.join(id).join("pack.json"), bundle_pack_json(id, id)).unwrap();
        }
        let mock = Mock::start(Routes::new(make_bundle(&[]), 2));
        let prog = install_all(&mock.url, &import, Arc::new(AtomicBool::new(false)), None);
        assert_eq!(prog.done, 0);
        assert_eq!(prog.skipped, 2);
        assert!(prog.finished);
        let paths = mock.paths();
        assert!(
            !paths.iter().any(|p| p.starts_with("/api/bundle")),
            "no bundle traffic when all packs installed: {paths:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_all_waits_for_building_bundle() {
        let root = temp_root("e2e-building");
        let import = root.join("imported");
        std::fs::create_dir_all(&import).unwrap();
        let bundle = make_bundle(&[("p0", pack_files("p0", "P0"))]);
        let mut routes = Routes::new(bundle.clone(), 1);
        // два первых опроса статуса — «собираем», потом готово; первый HEAD — 503
        routes.building_status = std::sync::atomic::AtomicUsize::new(2);
        routes.building_heads = std::sync::atomic::AtomicUsize::new(1);
        let mock = Mock::start(routes);
        let (tx, rx) = mpsc::channel();
        let prog = install_all(&mock.url, &import, Arc::new(AtomicBool::new(false)), Some(tx));
        assert_eq!(prog.done, 1, "pack from a bundle that was building: {:?}", prog.failed);
        assert!(prog.failed.is_empty(), "{:?}", prog.failed);
        assert!(installed_manifest(&import, "p0"));
        let paths = mock.paths();
        assert!(
            paths.iter().filter(|p| *p == BUNDLE_STATUS_PATH).count() >= 3,
            "must poll the status while building: {paths:?}"
        );
        // во время ожидания в UI уходило «собираем архив»
        let mut saw_building = false;
        while let Ok(p) = rx.try_recv() {
            if p.stage == Stage::Building {
                saw_building = true;
            }
        }
        assert!(saw_building, "UI must be told the server is building the bundle");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn download_bundle_cancel_keeps_part_for_resume() {
        let root = temp_root("e2e-cancel");
        let import = root.join("imported");
        std::fs::create_dir_all(&import).unwrap();
        // ~3 МБ, чтобы отмена случилась в середине
        let big = vec![9u8; 3 * 1024 * 1024];
        let bundle = make_bundle(&[(
            "p0",
            vec![
                ("pack.json", bundle_pack_json("p0", "P0"), true),
                ("audio.ogg", big, false),
            ],
        )]);
        let mock = Mock::start(Routes::new(bundle.clone(), 1));
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let mut on_disk_seen = 0u64;
        let dl = download_bundle(&mock.url, &import, cancel.as_ref(), &mut |ev| {
            if let BundleEvent::Bytes { on_disk, .. } = ev {
                on_disk_seen = on_disk;
                if on_disk >= 1024 * 1024 {
                    c2.store(true, Ordering::Relaxed);
                }
            }
        })
        .unwrap();
        assert!(dl.cancelled, "must stop on cancel (saw {on_disk_seen} bytes)");
        assert!(dl.size > 0 && dl.size < bundle.len() as u64, "partial part: {}", dl.size);
        assert!(dl.path.is_file());
        assert_eq!(std::fs::metadata(&dl.path).unwrap().len(), dl.size);
        assert!(root.join(".bundle.part.etag").is_file(), "etag saved for next run");

        // вторая попытка: продолжаем с того же места
        let partial = dl.size;
        let dl2 = download_bundle(&mock.url, &import, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert!(dl2.complete);
        assert_eq!(dl2.size, bundle.len() as u64);
        let gets = mock.bundle_gets();
        assert_eq!(gets.len(), 2);
        assert_eq!(gets[1].range.as_deref(), Some(format!("bytes={partial}-").as_str()));
        let _ = std::fs::remove_dir_all(&root);
    }

    // ------------------------- живой тест (нужна сеть) -------------------------

    /// Список установленных паков во временной папке (без скрытых).
    fn installed_packs(import: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(import) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() && !name.starts_with('.') {
                    out.push(name);
                }
            }
        }
        out.sort();
        out
    }

    /// Проверить, что каждый установленный пак читается `pack::manifest_of`
    /// и что чарты с аудио на месте. Возвращает список id.
    fn check_installed(import: &Path) -> Vec<String> {
        let mut ok = Vec::new();
        for id in installed_packs(import) {
            let dir = import.join(&id);
            assert!(dir.join("pack.json").is_file(), "{id}: no pack.json");
            let m = crate::pack::manifest_of(&dir).unwrap_or_else(|| panic!("{id}: manifest_of failed"));
            assert!(!m.charts.is_empty(), "{id}: no charts");
            for c in &m.charts {
                assert!(dir.join(&c.path).is_file(), "{id}: chart {} missing", c.path);
            }
            assert!(m.audio.is_some(), "{id}: no audio");
            let audio = m.audio.as_ref().unwrap();
            assert!(dir.join(audio).is_file(), "{id}: audio {audio} missing");
            println!(
                "live:   {id}: {} charts, audio {audio}, title {:?}",
                m.charts.len(),
                m.title
            );
            ok.push(id);
        }
        println!("live: verified {} installed packs", ok.len());
        if !ok.is_empty() {
            assert!(ok.len() >= 1);
        }
        ok
    }

    /// Живой прогон: `DEEMO_LIVE_BUNDLE=1 cargo test --release pack_download::live -- --nocapture`.
    ///
    /// 1) качаем бандл ~`DEEMO_LIVE_SECS` секунд и отменяем — `.part` должен
    ///    остаться непустым;
    /// 2) запускаем снова и убеждаемся, что качаем С ПРОДОЛЖЕНИЯ (байты растут
    ///    сверх первого размера, а не с нуля);
    /// 3) если бандл успел скачаться — распаковываем несколько паков и проверяем
    ///    `pack.json` + чарты + аудио через `pack::manifest_of`.
    ///
    /// Временная папка (`DEEMO_LIVE_DIR`, по умолчанию /tmp) удаляется в конце,
    /// настоящий `data/imported` не трогается.
    #[test]
    fn live_bundle_resume_and_extract() {
        if std::env::var("DEEMO_LIVE_BUNDLE").ok().as_deref() != Some("1") {
            return;
        }
        let server = std::env::var("DEEMO_SERVER").unwrap_or_else(|_| "http://199.247.22.102".into());
        let secs: u64 = std::env::var("DEEMO_LIVE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
        let root = std::env::var("DEEMO_LIVE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join(format!("deemo-bundle-live-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&root);
        let import = root.join("imported");
        std::fs::create_dir_all(&import).unwrap();
        println!("live: server {server}, import dir {}", import.display());

        let packs_want: usize = std::env::var("DEEMO_LIVE_PACKS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
        let full: bool = std::env::var("DEEMO_LIVE_FULL").ok().as_deref() == Some("1");
        let full_secs: u64 = std::env::var("DEEMO_LIVE_FULL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
        let spawn = |cancel: Arc<AtomicBool>, tx: mpsc::Sender<Progress>| {
            let (srv, imp) = (server.clone(), import.clone());
            std::thread::spawn(move || install_all(&srv, &imp, cancel, Some(tx)))
        };

        // --- прогон 1: качаем и отменяем ---
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let h = spawn(cancel.clone(), tx);
        let t0 = std::time::Instant::now();
        let mut last = Progress::default();
        let mut shown = (0u64, 0usize);
        while t0.elapsed().as_secs() < secs {
            if let Ok(p) = rx.recv_timeout(Duration::from_millis(300)) {
                if (p.bytes, p.done) != shown {
                    shown = (p.bytes, p.done);
                    println!(
                        "live: run1 {:>10} / {:>10} bytes · done {} · {:?}",
                        p.bytes, p.total_bytes, p.done, p.current
                    );
                }
                last = p;
            }
        }
        cancel.store(true, Ordering::Relaxed);
        while !last.finished {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(p) => last = p,
                Err(_) => break,
            }
        }
        let _ = h.join();
        println!(
            "live: run1 done in {:.1}s: cancelled={} done={} skipped={} bytes={} failed={:?}",
            t0.elapsed().as_secs_f64(),
            last.cancelled,
            last.done,
            last.skipped,
            last.bytes,
            last.failed
        );
        let (part, etag_file) = bundle_paths(&import);
        if !part.is_file() || part.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
            println!(
                "live: no .bundle.part -> server has no bundle endpoint (fallback path); \
                 installed so far: {}",
                installed_packs(&import).len()
            );
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let first_size = part.metadata().unwrap().len();
        println!("live: run1 kept .part of {first_size} bytes, etag file: {}", etag_file.is_file());
        assert!(first_size > 0, ".part must keep the downloaded bytes");
        assert!(etag_file.is_file(), ".etag must be stored for resume");

        // --- прогон 2: продолжаем с того же места, отменяем после нескольких паков ---
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let h = spawn(cancel.clone(), tx);
        let t0 = std::time::Instant::now();
        let mut first_seen: Option<u64> = None;
        let mut peak = first_size;
        let mut last = Progress::default();
        while t0.elapsed().as_secs() < secs * 3 {
            if let Ok(p) = rx.recv_timeout(Duration::from_millis(300)) {
                if first_seen.is_none() && p.bytes > 0 {
                    first_seen = Some(p.bytes);
                    println!("live: run2 starts at {} bytes (part was {first_size})", p.bytes);
                }
                peak = peak.max(p.bytes);
                let unpacked = p.unpacked;
                last = p;
                if unpacked >= packs_want {
                    break; // распаковка пошла — хватит
                }
            }
        }
        cancel.store(true, Ordering::Relaxed);
        while !last.finished {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(p) => last = p,
                Err(_) => break,
            }
        }
        let _ = h.join();
        let resumed = first_seen.unwrap_or(0);
        println!(
            "live: run2 finished in {:.1}s: resumed_at={resumed} peak={peak} cancelled={} unpacked={} bytes={}",
            t0.elapsed().as_secs_f64(),
            last.cancelled,
            last.unpacked,
            last.bytes
        );
        assert!(
            resumed + 1024 * 1024 >= first_size,
            "run2 must resume (first progress {resumed} vs part {first_size}), not restart from 0"
        );
        let after = part.metadata().map(|m| m.len()).unwrap_or(0);
        assert!(after >= first_size, "part must grow, not shrink ({after} < {first_size})");

        // --- что уже распаковалось: pack.json + чарты + аудио ---
        check_installed(&import);

        if full {
            // --- прогон 3: до конца (архив уже на диске, докачка/распаковка) ---
            let cancel = Arc::new(AtomicBool::new(false));
            let (tx, rx) = mpsc::channel();
            let h = spawn(cancel.clone(), tx);
            let t0 = std::time::Instant::now();
            let mut last = Progress::default();
            while t0.elapsed().as_secs() < full_secs {
                if let Ok(p) = rx.recv_timeout(Duration::from_millis(500)) {
                    last = p;
                    if last.finished {
                        break;
                    }
                }
            }
            if !last.finished {
                cancel.store(true, Ordering::Relaxed);
            }
            while !last.finished {
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(p) => last = p,
                    Err(_) => break,
                }
            }
            let _ = h.join();
            println!(
                "live: run3 in {:.1}s: done={} skipped={} total={} bytes={} failed={:?} finished={}",
                t0.elapsed().as_secs_f64(),
                last.done,
                last.skipped,
                last.total,
                last.bytes,
                last.failed,
                last.finished
            );
            let packs = check_installed(&import);
            println!("live: run3 verified {} packs, .part gone: {}", packs.len(), !part.exists());
            assert!(packs.len() >= packs_want, "expected at least {packs_want} packs");
            if last.failed.is_empty() && last.done + last.skipped >= last.total {
                assert!(!part.exists(), "complete run must delete .bundle.part");
                assert!(!etag_file.exists(), "complete run must delete .bundle.part.etag");
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        println!("live: OK (run1 {first_size} bytes kept, run2 grew to {after})");
    }
}
