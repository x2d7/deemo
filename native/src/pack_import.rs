//! Установка паков: drag & drop в окно, папка-инбокс и диалог выбора файла.
//!
//! Принимаем `.deemopack`/`.zip` с манифестом в корне архива и папку с
//! `pack.json`. Файлы ставятся в `data/imported/<id>/` тем же атомарным путём,
//! что и загрузчик паков; библиотека подхватывает новую папку сама.
//!
//! На Wayland у `winit` нет drag & drop вообще, поэтому есть два пути, которые
//! работают везде: папка `data/inbox/` (фоновый опрос раз в секунду) и
//! «Install pack from file…» в настройках (`kdialog`/`zenity`, хоткей F7).

use anyhow::{anyhow, bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime};

/// Предел размера брошенного файла (как у серверного загрузчика).
pub const MAX_DROP_BYTES: u64 = 64 * 1024 * 1024;
/// Предел размера папки пака, брошенной в окно.
const MAX_DIR_BYTES: u64 = 512 * 1024 * 1024;
/// Максимальная длина производного id (у `pack::safe_id` предел 64).
const ID_MAX: usize = 48;
/// Длина названия/причины в тосте.
const TOAST_TEXT_MAX: usize = 64;

/// Имя файла/папки → безопасный id пака `[a-z0-9_-]`.
///
/// Всё недопустимое сжимается в один `_`, края подрезаются, пустой результат
/// заменяется на `pack`. Гарантирует `crate::pack::safe_id`.
pub fn sanitize_pack_id(stem: &str) -> String {
    let mut out = String::new();
    for ch in stem.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let mut id = out.trim_matches(|c| c == '_' || c == '-').to_string();
    id.truncate(ID_MAX);
    let id = id.trim_end_matches(|c| c == '_' || c == '-').to_string();
    // страховка: наружу отдаём только то, что примет установщик паков
    if id.is_empty() || !crate::pack::safe_id(&id) {
        "pack".to_string()
    } else {
        id
    }
}

/// id пака из имени брошенного файла (`.deemopack`, `.zip`, любое — лишь бы
/// имя было осмысленным).
pub fn file_pack_id(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    sanitize_pack_id(stem)
}

/// Манифест из `pack.json` в папке (без авто-определения: папкой пака считаем
/// только явный манифест).
pub fn pack_dir_manifest(dir: &Path) -> Option<crate::pack::PackManifest> {
    if !dir.is_dir() {
        return None;
    }
    let text = std::fs::read_to_string(dir.join("pack.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Это папка установленного/устанавливаемого пака?
pub fn is_pack_dir(dir: &Path) -> bool {
    pack_dir_manifest(dir).is_some()
}

/// Идентификатор для папки: id из манифеста, иначе имя папки.
pub fn dir_pack_id(dir: &Path, manifest: &crate::pack::PackManifest) -> String {
    if crate::pack::safe_id(&manifest.id) {
        return manifest.id.clone();
    }
    let sanitized = sanitize_pack_id(&manifest.id);
    if crate::pack::safe_id(&sanitized) && sanitized != "pack" {
        return sanitized;
    }
    let from_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(sanitize_pack_id)
        .unwrap_or_else(|| "pack".to_string());
    if crate::pack::safe_id(&from_name) {
        from_name
    } else {
        "pack".to_string()
    }
}

/// Установить брошенный путь. `Ok(title)` — что показать в тосте
/// (`Installed <title>`), `Err(toast)` — готовый текст ошибки.
///
/// Папка принимается по `pack.json`; файл — только `.deemopack`/`.zip`.
pub fn install_dropped(path: &Path, import_dir: &Path) -> std::result::Result<String, String> {
    if path.is_dir() {
        return install_pack_dir(path, import_dir)
            .map_err(|e| format!("Install failed: {}", short_reason(&format!("{e:#}"))));
    }
    if !path.is_file() {
        return Err("Install failed: file not found".to_string());
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext != "deemopack" && ext != "zip" {
        return Err("Unsupported file: expected .deemopack".to_string());
    }
    install_pack_file(path, import_dir)
        .map_err(|e| format!("Install failed: {}", short_reason(&format!("{e:#}"))))
}

fn install_pack_file(path: &Path, import_dir: &Path) -> Result<String> {
    let size = std::fs::metadata(path)
        .with_context(|| "cannot read file")?
        .len();
    if size > MAX_DROP_BYTES {
        bail!("File is too large (max {} MB)", MAX_DROP_BYTES / (1024 * 1024));
    }
    let bytes = std::fs::read(path).with_context(|| "cannot read file")?;
    if bytes.len() as u64 > MAX_DROP_BYTES {
        bail!("File is too large (max {} MB)", MAX_DROP_BYTES / (1024 * 1024));
    }
    let id = file_pack_id(path);
    crate::pack_download::install_zip(&bytes, import_dir, &id)
        .with_context(|| format!("cannot install {id}"))?;
    Ok(installed_title(&import_dir.join(&id), &id))
}

fn install_pack_dir(dir: &Path, import_dir: &Path) -> Result<String> {
    if !is_pack_dir(dir) {
        bail!("not a pack folder (no valid pack.json)");
    }
    let manifest = pack_dir_manifest(dir)
        .ok_or_else(|| anyhow!("not a pack folder (no valid pack.json)"))?;
    let id = dir_pack_id(dir, &manifest);
    install_dir_atomic(dir, import_dir, &id)?;
    Ok(installed_title(&import_dir.join(&id), &id))
}

/// Скопировать папку пака во временную и переименовать в `<import>/<id>`.
fn install_dir_atomic(src: &Path, import_dir: &Path, id: &str) -> Result<()> {
    if !crate::pack::safe_id(id) {
        bail!("unsafe pack id {id:?}");
    }
    std::fs::create_dir_all(import_dir).context("create import dir")?;
    let tmp = import_dir.join(format!(".{id}.tmp-dir-{}", std::process::id()));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    std::fs::create_dir_all(&tmp).context("create temp dir")?;
    let result = (|| -> Result<()> {
        let mut copied = 0u64;
        copy_tree(src, &tmp, &mut copied)?;
        if !tmp.join("pack.json").is_file() {
            bail!("folder has no pack.json");
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

/// Рекурсивное копирование только обычных файлов/папок (симлинки не трогаем,
/// чтобы не вытащить что-то за пределы папки пака).
fn copy_tree(src: &Path, dst: &Path, copied: &mut u64) -> Result<()> {
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let to = dst.join(entry.file_name());
        if meta.is_dir() {
            std::fs::create_dir_all(&to)?;
            copy_tree(&entry.path(), &to, copied)?;
        } else if meta.is_file() {
            *copied += meta.len();
            if *copied > MAX_DIR_BYTES {
                bail!("Folder is too large (max {} MB)", MAX_DIR_BYTES / (1024 * 1024));
            }
            std::fs::copy(entry.path(), &to)
                .with_context(|| format!("copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn installed_title(dir: &Path, fallback: &str) -> String {
    let title = crate::pack::manifest_of(dir)
        .map(|m| if m.title.trim().is_empty() { m.id } else { m.title })
        .unwrap_or_default();
    let title = if title.trim().is_empty() {
        fallback.to_string()
    } else {
        title
    };
    shorten(&title)
}

/// Одна строка и не длиннее тоста.
fn shorten(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = line.chars().take(TOAST_TEXT_MAX).collect();
    if out.chars().count() < line.chars().count() {
        out.push('…');
    }
    out
}

fn short_reason(reason: &str) -> String {
    let first = reason.lines().next().unwrap_or(reason).trim();
    shorten(if first.is_empty() { "unknown error" } else { first })
}

// ===== инбокс data/inbox =====

/// Папка-инбокс: всё, что в неё попадает, ставится само.
pub const INBOX_DIR: &str = "data/inbox";
/// Куда уезжают успешно установленные файлы/папки.
pub const INBOX_INSTALLED: &str = "_installed";
/// Куда уезжают файлы, которые не удалось поставить (чтобы не повторять).
pub const INBOX_FAILED: &str = "_failed";
/// Как часто фоновый поток смотрит в инбокс.
pub const INBOX_POLL: Duration = Duration::from_secs(1);
/// Заголовок диалога выбора файла.
pub const PICK_TITLE: &str = "Install Deemo pack";

/// Строка в stdout при старте (подсказка, без отдельных файлов).
pub const INBOX_READY_LINE: &str =
    "inbox ready: drop .deemopack or a pack folder into data/inbox (also F7 in game)";

/// Что произошло с одним элементом инбокса (событие для UI).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InboxEvent {
    Installed { name: String, title: String },
    Failed { name: String, reason: String },
}

impl InboxEvent {
    /// Готовый текст тоста.
    pub fn toast(&self) -> String {
        match self {
            InboxEvent::Installed { title, .. } => format!("Installed {title}"),
            InboxEvent::Failed { reason, .. } => reason.clone(),
        }
    }
}

/// То, что инбокс вообще рассматривает: не скрытое, не `_installed`/`_failed`,
/// файл `.deemopack`/`.zip` или папка с валидным `pack.json`.
/// Остальное (заметки, картинки, папки без манифеста) не трогаем и не тостим.
pub fn is_inbox_candidate(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.starts_with('.') || name == INBOX_INSTALLED || name == INBOX_FAILED {
        return false;
    }
    if path.is_dir() {
        return is_pack_dir(path);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    (ext == "deemopack" || ext == "zip") && path.is_file()
}

/// Создать `data/inbox` и её подпапки `_installed`/`_failed`.
pub fn ensure_inbox(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::create_dir_all(dir.join(INBOX_INSTALLED))
        .with_context(|| format!("create {}/{}", dir.display(), INBOX_INSTALLED))?;
    std::fs::create_dir_all(dir.join(INBOX_FAILED))
        .with_context(|| format!("create {}/{}", dir.display(), INBOX_FAILED))?;
    Ok(())
}

/// Наблюдатель за инбоксом. `settle` требует увидеть файл дважды (копирование
/// большого пака не ставится на половине); `immediate` ставит сразу (тесты,
/// `--inbox-test`).
pub struct Inbox {
    dir: PathBuf,
    import_dir: PathBuf,
    seen: HashMap<PathBuf, (u64, Option<SystemTime>)>,
    failed: HashSet<PathBuf>,
    settle: bool,
}

impl Inbox {
    /// Боевой режим: элемент должен «устояться» между двумя опросами.
    pub fn new(dir: impl Into<PathBuf>, import_dir: impl Into<PathBuf>) -> Inbox {
        Inbox {
            dir: dir.into(),
            import_dir: import_dir.into(),
            seen: HashMap::new(),
            failed: HashSet::new(),
            settle: true,
        }
    }

    /// Ставить всё сразу, без проверки на докачивание.
    pub fn immediate(dir: impl Into<PathBuf>, import_dir: impl Into<PathBuf>) -> Inbox {
        let mut inbox = Inbox::new(dir, import_dir);
        inbox.settle = false;
        inbox
    }

    /// Один проход: поставить всё подходящее, убрать источники в
    /// `_installed`/`_failed`. Ошибки не повторяются в следующем проходе.
    pub fn scan(&mut self) -> Vec<InboxEvent> {
        let mut events = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return events;
        };
        let mut paths: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| !self.failed.contains(p) && is_inbox_candidate(p))
            .collect();
        paths.sort();

        let mut next_seen: HashMap<PathBuf, (u64, Option<SystemTime>)> = HashMap::new();
        for path in paths {
            if self.settle {
                let sig = entry_sig(&path);
                if self.seen.get(&path) != Some(&sig) {
                    next_seen.insert(path, sig);
                    continue;
                }
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            match install_dropped(&path, &self.import_dir) {
                Ok(title) => match move_into(&path, &self.dir.join(INBOX_INSTALLED), &name) {
                    Ok(()) => events.push(InboxEvent::Installed { name, title }),
                    Err(e) => {
                        // ставить уже нечего, но и оставлять в инбоксе нельзя
                        self.failed.insert(path);
                        events.push(InboxEvent::Failed {
                            name,
                            reason: format!(
                                "Install failed: cannot move into {INBOX_INSTALLED} ({})",
                                short_reason(&format!("{e:#}"))
                            ),
                        });
                    }
                },
                Err(reason) => {
                    if move_into(&path, &self.dir.join(INBOX_FAILED), &name).is_err() {
                        // не смогли убрать — больше не пробуем
                        self.failed.insert(path);
                    }
                    events.push(InboxEvent::Failed { name, reason });
                }
            }
        }
        self.seen = next_seen;
        events
    }
}

/// Фоновый поток: опрос раз в секунду, события — в канал (UI не блокируется).
/// Поток завершается сам, когда приёмник событий брошен.
pub fn spawn_inbox_watcher(import_dir: impl Into<PathBuf>) -> Receiver<InboxEvent> {
    spawn_inbox_watcher_in(INBOX_DIR, import_dir)
}

/// То же, но с явной папкой инбокса (тесты и `--inbox-test`).
pub fn spawn_inbox_watcher_in(
    inbox_dir: impl Into<PathBuf>,
    import_dir: impl Into<PathBuf>,
) -> Receiver<InboxEvent> {
    let (tx, rx) = mpsc::channel();
    let inbox_dir = inbox_dir.into();
    let import_dir = import_dir.into();
    std::thread::spawn(move || {
        if let Err(e) = ensure_inbox(&inbox_dir) {
            eprintln!("inbox: cannot prepare {}: {e:#}", inbox_dir.display());
        }
        let mut inbox = Inbox::new(inbox_dir, import_dir);
        loop {
            for ev in inbox.scan() {
                if tx.send(ev).is_err() {
                    return;
                }
            }
            std::thread::sleep(INBOX_POLL);
        }
    });
    rx
}

/// «Подпись» элемента инбокса: размер+время файла, а для папки — количество
/// файлов, их суммарный размер и самый свежий mtime (папка, которую ещё
/// копируют, меняется между опросами и не ставится на половине).
fn entry_sig(path: &Path) -> (u64, Option<SystemTime>) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, None);
    };
    if meta.is_dir() {
        let mut files = 0u64;
        let mut bytes = 0u64;
        let mut newest: Option<SystemTime> = None;
        walk_sig(path, &mut files, &mut bytes, &mut newest);
        let sum = files
            .wrapping_mul(1_000_003)
            .wrapping_add(bytes.rotate_left(17));
        (sum, newest)
    } else {
        (meta.len(), meta.modified().ok())
    }
}

fn walk_sig(path: &Path, files: &mut u64, bytes: &mut u64, newest: &mut Option<SystemTime>) {
    let Ok(rd) = std::fs::read_dir(path) else {
        return;
    };
    for entry in rd.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            walk_sig(&entry.path(), files, bytes, newest);
        } else if meta.is_file() {
            *files += 1;
            *bytes = bytes.wrapping_add(meta.len());
            if let Ok(t) = meta.modified() {
                *newest = Some(match *newest {
                    Some(prev) => prev.max(t),
                    None => t,
                });
            }
        }
    }
}

fn remove_path(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path)?,
        Ok(_) => std::fs::remove_file(path)?,
        Err(_) => {}
    }
    Ok(())
}

/// Перенести источник в `_installed`/`_failed` (одноимённый — перезаписать).
fn move_into(src: &Path, dst_dir: &Path, name: &str) -> Result<()> {
    std::fs::create_dir_all(dst_dir)?;
    let dest = dst_dir.join(name);
    remove_path(&dest)?;
    if std::fs::rename(src, &dest).is_ok() {
        return Ok(());
    }
    // разные файловые системы: копируем и удаляем источник
    if src.is_dir() {
        std::fs::create_dir_all(&dest)?;
        let mut copied = 0u64;
        copy_tree(src, &dest, &mut copied)?;
        std::fs::remove_dir_all(src)?;
    } else {
        std::fs::copy(src, &dest)?;
        std::fs::remove_file(src)?;
    }
    Ok(())
}

// ===== диалог выбора файла (Wayland-совместимый путь) =====

/// Выбрать пак через родной диалог. Linux/Wayland — `kdialog`/`zenity`;
/// Windows — системный `IFileOpenDialog` через rfd (kdialog/zenity там нет).
/// `None` — отмена, пустой ответ или нет ни одного диалога.
pub fn pick_pack() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        return rfd::FileDialog::new()
            .set_title(PICK_TITLE)
            .add_filter("Deemo pack", &["deemopack", "zip"])
            .pick_file();
    }
    #[cfg(not(windows))]
    {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let dir = cwd.to_string_lossy().to_string();
        match run_open_dialog(
            "kdialog",
            &["--title", PICK_TITLE, "--getopenfilename", &dir, "*.deemopack *.zip"],
        ) {
            Ok(picked) => picked,
            Err(_) => run_open_dialog(
                "zenity",
                &[
                    "--file-selection",
                    &format!("--title={PICK_TITLE}"),
                    "--file-filter=.deemopack | *.deemopack *.zip",
                ],
            )
            .ok()
            .flatten(),
        }
    }
}

/// Запустить диалог. `Err` — программу не удалось запустить (тогда пробуем
/// следующий диалог); `Ok(None)` — пользователь отменил.
#[cfg_attr(windows, allow(dead_code))]
fn run_open_dialog(cmd: &str, args: &[&str]) -> std::io::Result<Option<PathBuf>> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(parse_dialog_stdout(&out.stdout))
}

/// Ответ диалога: первая непустая строка без пробелов по краям.
#[cfg_attr(windows, allow(dead_code))]
fn parse_dialog_stdout(bytes: &[u8]) -> Option<PathBuf> {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(PathBuf::from(line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("deemo-drop-{tag}-{}", std::process::id()));
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

    fn manifest(id: &str, title: &str) -> String {
        format!(
            r#"{{"format_version":1,"id":"{id}","title":"{title}","composer":"C","charts":[{{"diff":"hard","path":"charts/hard.json"}}]}}"#
        )
    }

    /// Собрать zip в памяти (store-метод, без сжатия).
    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data) in files {
            let crc = crc32fast::hash(data);
            let off = out.len() as u32;
            out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // store
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
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

    #[test]
    fn ids_from_nasty_file_names_are_safe() {
        let cases = [
            ("My Song (Hard Mix).deemopack", "my_song_hard_mix"),
            ("../../evil.deemopack", "evil"),
            ("..\u{2044}..\u{2044}up.deemopack", "up"),
            ("AD3QUIMI.DEEMOPACK", "ad3quimi"),
            ("a  b.deemopack", "a_b"),
            ("x..y.deemopack", "x_y"),
            ("-a-.deemopack", "a"),
            ("-.deemopack", "pack"),
            (".deemopack", "deemopack"),
            ("日本語.deemopack", "pack"),
            ("😀.deemopack", "pack"),
            ("---.deemopack", "pack"),
        ];
        for (name, want) in cases {
            let id = file_pack_id(Path::new(name));
            assert_eq!(id, want, "id for {name:?}");
            assert!(crate::pack::safe_id(&id), "id {id:?} must be safe");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'),
                "id {id:?} has forbidden characters"
            );
        }
        // длинное имя подрезается до safe_id
        let long = format!("{}.deemopack", "A".repeat(200));
        let id = file_pack_id(Path::new(&long));
        assert!(id.len() <= ID_MAX);
        assert!(crate::pack::safe_id(&id));
    }

    #[test]
    fn detects_pack_directories_only_with_a_valid_manifest() {
        let root = temp_root("dircheck");
        let good = root.join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(good.join("pack.json"), manifest("good", "Good")).unwrap();
        assert!(is_pack_dir(&good));
        assert_eq!(pack_dir_manifest(&good).unwrap().title, "Good");

        let no_manifest = root.join("no_manifest");
        std::fs::create_dir_all(&no_manifest).unwrap();
        std::fs::write(no_manifest.join("hard.json"), br#"{"notes":[]}"#).unwrap();
        assert!(!is_pack_dir(&no_manifest), "auto-detect folders are not drop targets");

        let broken = root.join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("pack.json"), "not json").unwrap();
        assert!(!is_pack_dir(&broken));

        let file = root.join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(!is_pack_dir(&file), "a file is not a pack dir");
        assert!(!is_pack_dir(&root.join("missing")));

        assert_eq!(dir_pack_id(&good, &pack_dir_manifest(&good).unwrap()), "good");
        // небезопасный id из манифеста санитизируется
        let odd = root.join("odd folder");
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::write(odd.join("pack.json"), manifest("../Bad Id", "Bad")).unwrap();
        let m = pack_dir_manifest(&odd).unwrap();
        assert_eq!(dir_pack_id(&odd, &m), "bad_id");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn installs_a_deemopack_file_into_a_temp_import_dir() {
        let root = temp_root("file");
        let import = root.join("imported");
        let zip = make_zip(&[
            ("pack.json", manifest("demo", "Demo Song").as_bytes()),
            ("charts/hard.json", br#"{"notes":[{"time":1.0,"pos":0}]}"#),
        ]);
        let pack = root.join("Demo Song (Hard Mix).deemopack");
        std::fs::write(&pack, &zip).unwrap();

        let title = install_dropped(&pack, &import).unwrap();
        assert_eq!(title, "Demo Song");
        let id = file_pack_id(&pack);
        assert_eq!(id, "demo_song_hard_mix");
        assert!(import.join(&id).join("pack.json").is_file());
        assert!(import.join(&id).join("charts/hard.json").is_file());
        let m = crate::pack::manifest_of(&import.join(&id)).unwrap();
        assert_eq!(m.id, "demo");
        assert_eq!(m.charts.len(), 1);
        assert!(hidden(&import).is_empty(), "leftovers: {:?}", hidden(&import));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn installs_a_pack_directory_atomically() {
        let root = temp_root("folder");
        let import = root.join("imported");
        let src = root.join("Folder Name");
        std::fs::create_dir_all(src.join("charts")).unwrap();
        std::fs::write(src.join("pack.json"), manifest("frommanifest", "From Manifest")).unwrap();
        std::fs::write(src.join("charts/hard.json"), br#"{"notes":[]}"#).unwrap();

        let title = install_dropped(&src, &import).unwrap();
        assert_eq!(title, "From Manifest");
        assert!(import.join("frommanifest/pack.json").is_file());
        assert!(import.join("frommanifest/charts/hard.json").is_file());
        assert!(hidden(&import).is_empty(), "leftovers: {:?}", hidden(&import));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_bogus_files_without_leftovers() {
        let root = temp_root("bogus");
        let import = root.join("imported");
        std::fs::create_dir_all(&import).unwrap();

        let bogus = root.join("broken.deemopack");
        std::fs::write(&bogus, b"this is not a zip archive").unwrap();
        let err = install_dropped(&bogus, &import).unwrap_err();
        assert!(err.starts_with("Install failed:"), "toast text: {err}");
        assert!(std::fs::read_dir(&import).unwrap().next().is_none(), "no leftovers");

        let txt = root.join("notes.txt");
        std::fs::write(&txt, b"hello").unwrap();
        assert_eq!(
            install_dropped(&txt, &import).unwrap_err(),
            "Unsupported file: expected .deemopack"
        );
        // папка без манифеста — тоже ошибка без следов
        let folder = root.join("just-a-folder");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("readme.txt"), b"x").unwrap();
        let err = install_dropped(&folder, &import).unwrap_err();
        assert!(err.contains("no valid pack.json"), "toast text: {err}");
        assert!(hidden(&import).is_empty(), "leftovers: {:?}", hidden(&import));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_with_traversal_is_rejected() {
        let root = temp_root("traversal");
        let import = root.join("imported");
        let zip = make_zip(&[
            ("pack.json", manifest("evil", "Evil").as_bytes()),
            ("../escape.txt", b"x"),
        ]);
        let pack = root.join("evil.deemopack");
        std::fs::write(&pack, &zip).unwrap();
        let err = install_dropped(&pack, &import).unwrap_err();
        assert!(err.starts_with("Install failed:"), "toast text: {err}");
        assert!(!import.join("evil").exists());
        assert!(!root.join("escape.txt").exists());
        assert!(hidden(&import).is_empty(), "leftovers: {:?}", hidden(&import));
        let _ = std::fs::remove_dir_all(&root);
    }

    // ===== инбокс =====

    /// Инбокс + папка импорта в одном временном корне.
    fn inbox_root(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = temp_root(tag);
        let inbox = root.join("inbox");
        let import = root.join("imported");
        ensure_inbox(&inbox).unwrap();
        std::fs::create_dir_all(&import).unwrap();
        (root, inbox, import)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    #[test]
    fn inbox_scan_ignores_hidden_state_dirs_and_non_packs() {
        let (root, inbox, import) = inbox_root("inbox-filter");
        // скрытый пак, служебные папки, заметка и папка без манифеста
        std::fs::write(inbox.join(".hidden.deemopack"), b"x").unwrap();
        std::fs::write(inbox.join("_failed/old.deemopack"), b"x").unwrap();
        std::fs::write(inbox.join("notes.txt"), b"hello").unwrap();
        std::fs::create_dir_all(inbox.join("just-a-folder")).unwrap();
        std::fs::write(inbox.join("just-a-folder/readme.txt"), b"x").unwrap();

        let mut ib = Inbox::immediate(&inbox, &import);
        let events = ib.scan();
        assert!(events.is_empty(), "nothing to install, got {events:?}");
        // не-паки остаются на месте, установок и следов нет
        assert!(inbox.join("notes.txt").is_file());
        assert!(inbox.join("just-a-folder").is_dir());
        assert!(inbox.join(".hidden.deemopack").is_file());
        assert!(inbox.join("_failed/old.deemopack").is_file());
        assert!(names(&import).is_empty(), "nothing installed: {:?}", names(&import));

        assert!(!is_inbox_candidate(&inbox.join("_installed")));
        assert!(!is_inbox_candidate(&inbox.join("_failed")));
        assert!(!is_inbox_candidate(&inbox.join(".hidden.deemopack")));
        assert!(!is_inbox_candidate(&inbox.join("notes.txt")));
        assert!(!is_inbox_candidate(&inbox.join("just-a-folder")));
        // расширение регистронезависимо, но файл должен существовать
        std::fs::write(inbox.join("Song.DEEMOPACK"), b"x").unwrap();
        assert!(is_inbox_candidate(&inbox.join("Song.DEEMOPACK")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inbox_installs_moves_source_and_reuses_id_derivation() {
        let (root, inbox, import) = inbox_root("inbox-install");
        let zip = make_zip(&[
            ("pack.json", manifest("demo", "Demo Song").as_bytes()),
            ("charts/hard.json", br#"{"notes":[]}"#),
        ]);
        let name = "My Song (Hard Mix).deemopack";
        std::fs::write(inbox.join(name), &zip).unwrap();

        let mut ib = Inbox::immediate(&inbox, &import);
        let events = ib.scan();
        assert_eq!(
            events,
            vec![InboxEvent::Installed {
                name: name.to_string(),
                title: "Demo Song".to_string()
            }]
        );
        assert_eq!(events[0].toast(), "Installed Demo Song");

        // id — тот же, что при drag & drop
        let id = file_pack_id(Path::new(name));
        assert_eq!(id, "my_song_hard_mix");
        let dir = import.join(&id);
        assert!(dir.join("pack.json").is_file(), "pack not installed");
        assert!(dir.join("charts/hard.json").is_file());
        assert_eq!(crate::pack::manifest_of(&dir).unwrap().title, "Demo Song");
        assert!(hidden(&import).is_empty(), "leftovers: {:?}", hidden(&import));

        // источник уехал в _installed, в инбоксе его нет
        assert!(!inbox.join(name).exists(), "source must leave the inbox");
        assert!(inbox.join("_installed").join(name).is_file());

        // одноимённый файл перезаписывает прошлый (без дублей и ошибок)
        std::fs::write(inbox.join(name), &zip).unwrap();
        let events = ib.scan();
        assert_eq!(events.len(), 1, "second run: {events:?}");
        assert_eq!(names(&inbox.join("_installed")), vec![name.to_string()]);
        assert_eq!(names(&import), vec![id.clone()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inbox_installs_a_pack_directory() {
        let (root, inbox, import) = inbox_root("inbox-dir");
        let src = inbox.join("Folder Name");
        std::fs::create_dir_all(src.join("charts")).unwrap();
        std::fs::write(src.join("pack.json"), manifest("frommanifest", "From Manifest")).unwrap();
        std::fs::write(src.join("charts/hard.json"), br#"{"notes":[]}"#).unwrap();

        let mut ib = Inbox::immediate(&inbox, &import);
        let events = ib.scan();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].toast(), "Installed From Manifest");
        assert!(import.join("frommanifest/pack.json").is_file());
        assert!(!inbox.join("Folder Name").exists(), "source dir must move");
        assert!(inbox.join("_installed/Folder Name/pack.json").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inbox_failure_moves_source_to_failed_and_never_retries() {
        let (root, inbox, import) = inbox_root("inbox-fail");
        let bogus = inbox.join("broken.deemopack");
        std::fs::write(&bogus, b"this is not a zip archive").unwrap();

        let mut ib = Inbox::immediate(&inbox, &import);
        let events = ib.scan();
        assert_eq!(events.len(), 1, "{events:?}");
        let InboxEvent::Failed { name, reason } = &events[0] else {
            panic!("expected a failure, got {events:?}");
        };
        assert_eq!(name, "broken.deemopack");
        assert!(reason.starts_with("Install failed:"), "toast: {reason}");
        assert!(!bogus.exists(), "failed source must leave the inbox");
        assert!(inbox.join("_failed/broken.deemopack").is_file());
        assert!(hidden(&import).is_empty(), "leftovers: {:?}", hidden(&import));

        // повторный проход ничего не делает и ничего не тостит
        let again = ib.scan();
        assert!(again.is_empty(), "no retries: {again:?}");
        assert_eq!(names(&inbox.join("_failed")), vec!["broken.deemopack".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inbox_waits_for_a_settled_file_before_installing() {
        let (root, inbox, import) = inbox_root("inbox-settle");
        let zip = make_zip(&[("pack.json", manifest("late", "Late").as_bytes())]);
        std::fs::write(inbox.join("late.deemopack"), &zip).unwrap();

        let mut ib = Inbox::new(&inbox, &import); // боевой режим
        assert!(ib.scan().is_empty(), "first sighting only records the entry");
        let events = ib.scan();
        assert_eq!(events.len(), 1, "second poll installs: {events:?}");
        assert!(import.join("late/pack.json").is_file());
        assert!(inbox.join("_installed/late.deemopack").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inbox_does_not_install_a_dir_that_is_still_growing() {
        let (root, inbox, import) = inbox_root("inbox-dir-settle");
        let src = inbox.join("growing");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("pack.json"), manifest("grow", "Grow")).unwrap();

        let mut ib = Inbox::new(&inbox, &import);
        assert!(ib.scan().is_empty(), "first poll only records the folder");
        // копирование продолжается: папка меняется — ставить рано
        std::fs::create_dir_all(src.join("charts")).unwrap();
        std::fs::write(src.join("charts/hard.json"), br#"{"notes":[]}"#).unwrap();
        assert!(ib.scan().is_empty(), "a growing folder must not install");
        let events = ib.scan();
        assert_eq!(events.len(), 1, "settled folder installs: {events:?}");
        assert!(import.join("grow/pack.json").is_file());
        assert!(import.join("grow/charts/hard.json").is_file());
        assert!(!src.exists(), "source folder must move to _installed");
        assert!(inbox.join("_installed/growing/pack.json").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dialog_stdout_is_trimmed_and_cancels_are_empty() {
        assert_eq!(
            parse_dialog_stdout(b"/home/me/Song.deemopack\n"),
            Some(PathBuf::from("/home/me/Song.deemopack"))
        );
        assert_eq!(
            parse_dialog_stdout(b"  /home/me/a b.zip  \r\n"),
            Some(PathBuf::from("/home/me/a b.zip"))
        );
        assert_eq!(parse_dialog_stdout(b""), None);
        assert_eq!(parse_dialog_stdout(b"\n  \n"), None);
        // диалога нет — тихий None (никаких паник и зависаний)
        assert!(run_open_dialog("deemo-no-such-dialog-xyz", &[]).is_err());
    }
}
