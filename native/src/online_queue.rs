//! Очередь отправки рекордов: запись сначала ложится на диск, и только потом
//! делается попытка загрузить её на сервер. Удаляется запись лишь после 2xx —
//! так рекорд не теряется при таймауте, обрыве связи или выходе из игры сразу
//! после результата.
//!
//! Формат `data/online_queue.json`:
//! `{"pending":[{...тело запроса /api/scores..., "server":"http://...", "token":"..."}]}`
//! `server`/`token` — под которыми рекорд был сыгран: переключение сервера не
//! должно отправить старые рекорды на новый хост, поэтому такие записи остаются
//! в очереди до возврата на свой сервер.
//!
//! Сеть — только в фоновом потоке: главный поток кладёт запись в файл,
//! отправляет команду и забирает события через `mpsc` (как `auth_rx`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Файл очереди (относительно рабочей папки игры).
pub const QUEUE_PATH: &str = "data/online_queue.json";
/// Предел очереди: старые записи вытесняются, чтобы файл не рос бесконечно.
pub const MAX_ENTRIES: usize = 500;
/// Таймаут одного POST /api/scores (как было у старого fire-and-forget).
pub const POST_TIMEOUT: Duration = Duration::from_secs(6);
/// Паузы между попытками: всего 3 попытки (0.5 с и 2 с ожидания).
pub const RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(500), Duration::from_secs(2)];

/// Один отложенный рекорд: тело POST /api/scores (все поля как раньше) плюс
/// сервер и токен, под которыми он был сыгран.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingScore {
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub token: String,
    /// Поля запроса (song, diff, score, ...). `flatten` держит их рядом с
    /// server/token в одном объекте очереди.
    #[serde(flatten)]
    pub body: Map<String, Value>,
}

impl PendingScore {
    pub fn new(server: &str, token: &str, body: Map<String, Value>) -> PendingScore {
        PendingScore {
            server: normalize_server(server),
            token: token.to_string(),
            body,
        }
    }

    /// URL отправки: `/api/scores` на записанном (не текущем) сервере.
    pub fn url(&self) -> String {
        format!("{}/api/scores", normalize_server(&self.server))
    }

    /// Песня из тела (для логов/тестов).
    pub fn song(&self) -> &str {
        self.body.get("song").and_then(|v| v.as_str()).unwrap_or("")
    }
}

/// Хвостовой слэш и пробелы не должны мешать сравнению серверов.
pub fn normalize_server(server: &str) -> String {
    server.trim().trim_end_matches('/').to_string()
}

/// Один и тот же сервер с точностью до хвостового слэша/регистра.
pub fn same_server(a: &str, b: &str) -> bool {
    normalize_server(a).eq_ignore_ascii_case(&normalize_server(b))
}

#[derive(Serialize, Deserialize, Default)]
struct QueueFile {
    #[serde(default)]
    pending: Vec<PendingScore>,
}

/// Прочитать очередь. Битый/чужой файл — пустая очередь, не паника.
pub fn load_entries(path: &Path) -> Vec<PendingScore> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<QueueFile>(&text) {
        Ok(file) => sanitize(file.pending),
        Err(e) => {
            eprintln!("online queue: ignoring corrupt {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Убрать записи без сервера/песни и уложиться в предел очереди (старые —
/// первыми). Битые объекты в файле не должны вечно занимать очередь.
pub fn sanitize(mut entries: Vec<PendingScore>) -> Vec<PendingScore> {
    entries.retain(|e| {
        !normalize_server(&e.server).is_empty() && !e.song().trim().is_empty()
    });
    if entries.len() > MAX_ENTRIES {
        let excess = entries.len() - MAX_ENTRIES;
        entries.drain(..excess);
    }
    entries
}

/// Записать очередь атомарно: временный файл + rename.
pub fn save_entries(path: &Path, entries: &[PendingScore]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let text = serde_json::to_string_pretty(&QueueFile {
        pending: entries.to_vec(),
    })
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Добавить запись; если очередь переполнена — вытеснить самые старые.
/// Возвращает число вытесненных записей.
pub fn push_capped(entries: &mut Vec<PendingScore>, rec: PendingScore, cap: usize) -> usize {
    let cap = cap.max(1);
    entries.push(rec);
    if entries.len() > cap {
        let excess = entries.len() - cap;
        entries.drain(..excess);
        excess
    } else {
        0
    }
}

/// Удалить ровно одну (первую совпавшую) запись; true — если что-то убрали.
pub fn remove_one(entries: &mut Vec<PendingScore>, rec: &PendingScore) -> bool {
    if let Some(i) = entries.iter().position(|e| e == rec) {
        entries.remove(i);
        true
    } else {
        false
    }
}

/// Записи, которые можно отправлять на `server` (остальные серверы не трогаем).
pub fn due_for_server(entries: &[PendingScore], server: &str) -> Vec<PendingScore> {
    entries
        .iter()
        .filter(|e| same_server(&e.server, server))
        .cloned()
        .collect()
}

/// Один POST. `true` только для 2xx: 3xx/4xx/5xx и транспортные ошибки
/// оставляют запись в очереди.
fn post_score(agent: &ureq::Agent, rec: &PendingScore) -> bool {
    let body = Value::Object(rec.body.clone());
    match agent
        .post(&rec.url())
        .set("Authorization", &format!("Bearer {}", rec.token))
        .send_json(body)
    {
        Ok(resp) => (200..300).contains(&resp.status()),
        Err(_) => false,
    }
}

/// До `retry.len() + 1` попыток с паузами между ними.
fn post_with_retry(agent: &ureq::Agent, rec: &PendingScore, retry: &[Duration]) -> bool {
    if post_score(agent, rec) {
        return true;
    }
    for delay in retry {
        if !delay.is_zero() {
            std::thread::sleep(*delay);
        }
        if post_score(agent, rec) {
            return true;
        }
    }
    false
}

/// Отправить все записи, записанные под `server`. Успешные удаляются из
/// вектора и из файла; неуспешные остаются. Возвращает (отправлено, не вышло).
///
/// Чистая от сети точки зрения тестируемость: `retry` задаёт политику повторов
/// (production — [`RETRY_DELAYS`], в тестах можно пустой/нулевой).
pub fn flush_pass(
    entries: &Mutex<Vec<PendingScore>>,
    path: &Path,
    server: &str,
    retry: &[Duration],
) -> (usize, usize) {
    let due = {
        let guard = entries.lock().unwrap();
        due_for_server(&guard, server)
    };
    if due.is_empty() {
        return (0, 0);
    }
    let agent = ureq::AgentBuilder::new().timeout(POST_TIMEOUT).build();
    let mut uploaded = 0usize;
    let mut failed = 0usize;
    for rec in due {
        if post_with_retry(&agent, &rec, retry) {
            let mut guard = entries.lock().unwrap();
            if remove_one(&mut guard, &rec) {
                if let Err(e) = save_entries(path, &guard) {
                    eprintln!("online queue: cannot rewrite {}: {e}", path.display());
                }
            }
            uploaded += 1;
        } else {
            eprintln!(
                "online queue: {} / {} stays queued (upload failed)",
                rec.song(),
                rec.body.get("diff").and_then(|v| v.as_str()).unwrap_or("")
            );
            failed += 1;
        }
    }
    (uploaded, failed)
}

/// Итог одного прохода очереди для UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueEvent {
    /// Номер команды (см. [`OnlineQueue::enqueue`] / [`OnlineQueue::flush`]).
    pub seq: u64,
    pub uploaded: usize,
    pub failed: usize,
    /// `true` — проход сразу после нового результата, `false` — фоновый.
    pub immediate: bool,
}

struct FlushCmd {
    server: String,
    seq: u64,
    immediate: bool,
}

/// Живая очередь: файл + фоновый поток отправки + события для тостов.
///
/// Главный поток при `enqueue` синхронно дописывает файл (это дёшево), поэтому
/// запись уже на диске до любой сетевой попытки; поток лишь отправляет и
/// вычёркивает успешные. Все изменения списка идут под одним мьютексом.
pub struct OnlineQueue {
    entries: Arc<Mutex<Vec<PendingScore>>>,
    path: PathBuf,
    tx: Sender<FlushCmd>,
    rx: Receiver<QueueEvent>,
    next_seq: u64,
}

impl OnlineQueue {
    pub fn open(path: impl Into<PathBuf>) -> OnlineQueue {
        let path = path.into();
        let entries = Arc::new(Mutex::new(load_entries(&path)));
        let (tx, cmd_rx) = channel::<FlushCmd>();
        let (evt_tx, rx) = channel::<QueueEvent>();
        let state = entries.clone();
        let worker_path = path.clone();
        std::thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                let (uploaded, failed) =
                    flush_pass(&state, &worker_path, &cmd.server, &RETRY_DELAYS);
                let _ = evt_tx.send(QueueEvent {
                    seq: cmd.seq,
                    uploaded,
                    failed,
                    immediate: cmd.immediate,
                });
            }
        });
        OnlineQueue {
            entries,
            path,
            tx,
            rx,
            next_seq: 0,
        }
    }

    /// Сохранить запись на диск и сразу попробовать отправить.
    /// Возвращает номер команды для сопоставления с [`QueueEvent`].
    pub fn enqueue(&mut self, rec: PendingScore, server: &str) -> u64 {
        {
            let mut guard = self.entries.lock().unwrap();
            let dropped = push_capped(&mut guard, rec, MAX_ENTRIES);
            if dropped > 0 {
                eprintln!("online queue: dropped {dropped} oldest entries (cap {MAX_ENTRIES})");
            }
            if let Err(e) = save_entries(&self.path, &guard) {
                eprintln!("online queue: cannot write {}: {e}", self.path.display());
            }
        }
        self.request_flush(server, true)
    }

    /// Фоновая отправка без привязки к только что сыгранному результату.
    pub fn flush(&mut self, server: &str) -> u64 {
        self.request_flush(server, false)
    }

    fn request_flush(&mut self, server: &str, immediate: bool) -> u64 {
        self.next_seq += 1;
        let seq = self.next_seq;
        let _ = self.tx.send(FlushCmd {
            server: normalize_server(server),
            seq,
            immediate,
        });
        seq
    }

    /// Неблокирующее событие фонового потока (вызывать каждый кадр).
    pub fn poll(&self) -> Option<QueueEvent> {
        self.rx.try_recv().ok()
    }

    /// Есть ли в очереди запись, равная данной (т.е. ещё не отправлена).
    pub fn contains(&self, rec: &PendingScore) -> bool {
        self.entries.lock().unwrap().iter().any(|e| e == rec)
    }

    /// Есть ли что отправлять на этот сервер (для периодического фона).
    pub fn has_due(&self, server: &str) -> bool {
        let guard = self.entries.lock().unwrap();
        guard.iter().any(|e| same_server(&e.server, server))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "deemo-queue-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("online_queue.json")
    }

    fn rec(server: &str, token: &str, song: &str, score: u64) -> PendingScore {
        let body = serde_json::json!({
            "song": song,
            "diff": "hard",
            "score": score,
            "accuracy": 99.4,
            "max_combo": 512,
            "perfect": 512,
            "good": 0,
            "miss": 0,
            "rank": "S",
            "full_combo": true,
            "speed": 4.0,
            "timestamp": 1_794_940_377u64,
        });
        PendingScore::new(server, token, body.as_object().unwrap().clone())
    }

    /// Мини-сервер: на каждый запрос отвечает очередным кодом и запоминает
    /// полученный запрос целиком.
    fn stub(codes: &[u16]) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let codes: Vec<u16> = codes.to_vec();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for code in codes {
                let Ok((mut sock, _)) = listener.accept() else {
                    break;
                };
                let _ = sock.set_read_timeout(Some(Duration::from_secs(10)));
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    match sock.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if request_complete(&buf) {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                seen.push(String::from_utf8_lossy(&buf).to_string());
                let body = if (200..300).contains(&code) {
                    r#"{"record":true,"best":999999}"#
                } else {
                    r#"{"error":"stub"}"#
                };
                let resp = format!(
                    "HTTP/1.1 {code} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            }
            seen
        });
        (format!("http://{addr}"), handle)
    }

    fn request_complete(buf: &[u8]) -> bool {
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            return false;
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
        let len = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        buf.len() >= end + 4 + len
    }

    #[test]
    fn queue_append_persist_and_reload() {
        let path = temp_path("roundtrip");
        let mut entries = Vec::new();
        assert_eq!(push_capped(&mut entries, rec("http://a:1", "tk", "first", 10), MAX_ENTRIES), 0);
        assert_eq!(push_capped(&mut entries, rec("http://a:1", "tk", "second", 20), MAX_ENTRIES), 0);
        save_entries(&path, &entries).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"pending\""));
        assert!(text.contains("\"server\": \"http://a:1\"") || text.contains("\"server\":\"http://a:1\""));
        assert!(text.contains("\"song\": \"first\"") || text.contains("\"song\":\"first\""));

        let loaded = load_entries(&path);
        assert_eq!(loaded, entries);
        assert_eq!(loaded[0].song(), "first");
        assert_eq!(loaded[1].body["score"], 20);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_queue_file_starts_empty() {
        let path = temp_path("corrupt");
        for garbage in ["not json at all", "[]", "{\"pending\":\"nope\"}", "{\"pending\":[{\"score\":1}]}"] {
            std::fs::write(&path, garbage).unwrap();
            assert!(load_entries(&path).is_empty(), "garbage must not crash: {garbage}");
        }
        // отсутствующий файл — тоже пустая очередь
        std::fs::remove_file(&path).unwrap();
        assert!(load_entries(&path).is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn queue_cap_drops_oldest_entries() {
        let mut entries = Vec::new();
        for i in 0..(MAX_ENTRIES + 2) {
            push_capped(
                &mut entries,
                rec("http://a:1", "tk", "song", i as u64),
                MAX_ENTRIES,
            );
        }
        assert_eq!(entries.len(), MAX_ENTRIES, "queue must stay capped");
        assert_eq!(entries[0].body["score"], 2, "oldest entries are evicted");
        assert_eq!(
            entries[MAX_ENTRIES - 1].body["score"],
            (MAX_ENTRIES + 1) as u64,
            "newest entry survives"
        );
        // загрузка тоже подрезает сверхдлинный файл
        let long = sanitize(vec![rec("http://a:1", "tk", "s", 1); MAX_ENTRIES + 5]);
        assert_eq!(long.len(), MAX_ENTRIES);
    }

    #[test]
    fn remove_one_only_drops_the_matching_entry() {
        let a = rec("http://a:1", "tk", "a", 1);
        let b = rec("http://a:1", "tk", "b", 2);
        let mut entries = vec![a.clone(), b.clone()];
        assert!(remove_one(&mut entries, &b));
        assert_eq!(entries, vec![a.clone()]);
        assert!(!remove_one(&mut entries, &b), "second removal is a no-op");
        assert_eq!(entries, vec![a]);
    }

    #[test]
    fn server_mismatch_is_kept_and_not_due() {
        let a = rec("http://old.example:8080/", "tk", "old", 1);
        let b = rec("http://new.example", "tk", "new", 2);
        let entries = vec![a.clone(), b.clone()];
        assert!(same_server("http://old.example:8080/", "http://old.example:8080"));
        let due = due_for_server(&entries, "http://new.example");
        assert_eq!(due, vec![b]);
        assert!(due_for_server(&entries, "http://third.example").is_empty());
        // нормализация при записи: хвостовой слэш убирается
        assert_eq!(a.server, "http://old.example:8080");
    }

    #[test]
    fn flush_uploads_and_removes_the_entry() {
        let path = temp_path("upload");
        let (server, handle) = stub(&[200]);
        let entry = rec(&server, "tok-42", "ad3quimi", 999_192);
        let entries = Mutex::new(vec![entry.clone()]);
        let (uploaded, failed) = flush_pass(&entries, &path, &server, &[]);
        assert_eq!((uploaded, failed), (1, 0), "2xx must count as uploaded");
        assert!(entries.lock().unwrap().is_empty(), "entry removed in memory");
        save_entries(&path, &entries.lock().unwrap()).unwrap();
        assert!(load_entries(&path).is_empty(), "entry removed on disk");

        let reqs = handle.join().unwrap();
        assert_eq!(reqs.len(), 1);
        let req = &reqs[0];
        assert!(req.starts_with("POST /api/scores HTTP/1.1"), "request: {req}");
        assert!(
            req.to_ascii_lowercase().contains("authorization: bearer tok-42"),
            "token must be sent: {req}"
        );
        assert!(req.contains("\"song\":\"ad3quimi\""), "body: {req}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn flush_retries_then_succeeds() {
        let path = temp_path("retry");
        let (server, handle) = stub(&[500, 502, 200]);
        let entries = Mutex::new(vec![rec(&server, "tk", "retry-song", 7)]);
        let (uploaded, failed) = flush_pass(&entries, &path, &server, &[Duration::ZERO, Duration::ZERO]);
        assert_eq!((uploaded, failed), (1, 0));
        assert!(entries.lock().unwrap().is_empty());
        assert_eq!(handle.join().unwrap().len(), 3, "three attempts");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn flush_keeps_entry_when_server_is_unreachable() {
        let path = temp_path("down");
        let dead = "http://127.0.0.1:1";
        let entries = Mutex::new(vec![rec(dead, "tk", "stays", 5)]);
        save_entries(&path, &entries.lock().unwrap()).unwrap();
        let (uploaded, failed) = flush_pass(&entries, &path, dead, &[]);
        assert_eq!((uploaded, failed), (0, 1));
        assert_eq!(entries.lock().unwrap().len(), 1, "entry stays in memory");
        let loaded = load_entries(&path);
        assert_eq!(loaded.len(), 1, "entry stays on disk");
        assert_eq!(loaded[0].song(), "stays");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn flush_does_not_touch_entries_of_other_servers() {
        let path = temp_path("mismatch");
        let (server, handle) = stub(&[200]);
        let mine = rec(&server, "tk", "mine", 1);
        let other = rec("http://127.0.0.1:1", "tk", "other", 2);
        let entries = Mutex::new(vec![other.clone(), mine.clone()]);
        save_entries(&path, &entries.lock().unwrap()).unwrap();
        let (uploaded, failed) = flush_pass(&entries, &path, &server, &[]);
        assert_eq!((uploaded, failed), (1, 0));
        let left = entries.lock().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0], other, "entry for another server must be kept");
        assert_eq!(handle.join().unwrap().len(), 1, "only the matching entry was sent");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn only_2xx_counts_as_a_successful_upload() {
        for (code, want) in [(200u16, true), (201, true), (302, false), (500, false)] {
            let (server, handle) = stub(&[code]);
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .build();
            let entry = rec(&server, "tk", "status", 1);
            assert_eq!(post_score(&agent, &entry), want, "status {code}");
            let _ = handle.join();
        }
    }

    /// Дождаться события фонового потока очереди (как это делает игровой цикл).
    fn wait_event(q: &OnlineQueue, timeout: Duration) -> Option<QueueEvent> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Some(ev) = q.poll() {
                return Some(ev);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn live_queue_persists_before_the_upload_and_reports_success() {
        let path = temp_path("live-ok");
        let (server, handle) = stub(&[200]);
        let mut q = OnlineQueue::open(&path);
        let entry = rec(&server, "tk", "live-song", 999_192);
        let seq = q.enqueue(entry.clone(), &server);
        // запись уже в файле — до всякой сети
        let on_disk = load_entries(&path);
        assert_eq!(on_disk.len(), 1, "record must be persisted before upload");
        assert_eq!(on_disk[0], entry);

        let ev = wait_event(&q, Duration::from_secs(5)).expect("queue event");
        assert_eq!(ev.seq, seq);
        assert!(ev.immediate);
        assert_eq!((ev.uploaded, ev.failed), (1, 0));
        assert!(!q.contains(&entry), "uploaded record leaves the queue");
        assert!(load_entries(&path).is_empty(), "uploaded record leaves the file");
        assert_eq!(handle.join().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn live_queue_keeps_the_record_when_the_server_is_down() {
        let path = temp_path("live-down");
        let dead = "http://127.0.0.1:1";
        let mut q = OnlineQueue::open(&path);
        let entry = rec(dead, "tk", "keep-me", 5);
        q.enqueue(entry.clone(), dead);
        assert_eq!(load_entries(&path).len(), 1, "queued before the network attempt");
        // 3 попытки в рабочем потоке: 0.5 с + 2 с (плюс мгновенный отказ)
        let ev = wait_event(&q, Duration::from_secs(10)).expect("queue event");
        assert_eq!((ev.uploaded, ev.failed), (0, 1));
        assert!(q.contains(&entry), "failed record stays queued");
        assert_eq!(load_entries(&path), vec![entry], "failed record stays on disk");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Живой сервер: требует DEEMO_LIVE_NICK/DEEMO_LIVE_PW (фейковый аккаунт),
    /// отправляет ОДИН пробный рекорд на вымышленную песню `zzqueueprobe`.
    /// Запускать явно: `cargo test --release live_server_accepts -- --ignored`.
    #[test]
    #[ignore = "network: creates/logs into a throwaway account and submits one probe record"]
    fn live_server_accepts_a_queued_score() {
        let server = std::env::var("DEEMO_LIVE_SERVER")
            .unwrap_or_else(|_| crate::online::DEFAULT_SERVER.to_string());
        let nick = std::env::var("DEEMO_LIVE_NICK").expect("DEEMO_LIVE_NICK");
        let pw = std::env::var("DEEMO_LIVE_PW").expect("DEEMO_LIVE_PW");
        let url = |p: &str| format!("{}{p}", normalize_server(&server));
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build();
        let auth_body = serde_json::json!({"nickname": nick, "password": pw});
        let resp = match agent.post(&url("/api/register")).send_json(auth_body.clone()) {
            Ok(r) => r,
            // аккаунт уже создан прошлым прогоном — просто входим
            Err(ureq::Error::Status(409, _)) => agent
                .post(&url("/api/login"))
                .send_json(auth_body)
                .expect("login"),
            Err(e) => panic!("register: {e}"),
        };
        let v: serde_json::Value = resp.into_json().expect("auth json");
        let token = v["token"].as_str().expect("token").to_string();

        let body = serde_json::json!({
            "song": "zzqueueprobe", "diff": "hard", "score": 424242,
            "accuracy": 50.0, "max_combo": 1, "perfect": 1, "good": 0, "miss": 0,
            "rank": "D", "full_combo": false, "speed": 4.0,
            "timestamp": 1_794_940_377u64,
        });
        let rec = PendingScore::new(
            &server,
            &token,
            body.as_object().cloned().unwrap_or_default(),
        );
        let path = temp_path("live-server");
        let mut q = OnlineQueue::open(&path);
        q.enqueue(rec.clone(), &server);
        let ev = wait_event(&q, Duration::from_secs(30)).expect("queue event");
        assert_eq!((ev.uploaded, ev.failed), (1, 0), "live upload must succeed");
        assert!(!q.contains(&rec));
        assert!(load_entries(&path).is_empty());
        // сервер действительно сохранил запись
        let hist: serde_json::Value = agent
            .get(&url("/api/my/history?song=zzqueueprobe"))
            .set("Authorization", &format!("Bearer {token}"))
            .call()
            .expect("history")
            .into_json()
            .expect("history json");
        let records = hist["history"].as_array().expect("history array");
        assert!(
            records
                .iter()
                .any(|r| r["song"] == "zzqueueprobe" && r["score"] == 424242),
            "record missing on server: {hist}"
        );
        println!("live queue: uploaded for {nick} on {server}: {ev:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
