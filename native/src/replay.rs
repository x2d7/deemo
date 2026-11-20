//! Запись реплея: сырой ввод (мышь + планшет) и действия игры с временем
//! относительно карты. Сохраняется по F2 (в конце карты / на результате).
//!
//! Формат JSON: meta (настройки + результат), notes (карта для справки),
//! events (сырые события ввода и судейство). Время t — секунды карты.
//!
//! Здесь же компактные блобы `DMR1`/`DMR2`, которые уходят на сервер вместе
//! со скором и проигрываются в игре. `DMR2` (его и пишем) добавляет позицию
//! указателя на каждой ноте и сэмплированный трек курсора; `DMR1` читаем для
//! старых фикстур. Индекс ноты — индекс в [`Chart::notes`] (после stable sort).

use crate::chart::{Chart, NoteKind};
use crate::play::{Judge, Play, CLICK_WINDOW};
use crate::stage::note_x;
use crate::tablet::RawTabletEvent;
use serde_json::{json, Value};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// ===== компактный реплей DMR1 / DMR2 =====

/// Магия DMR1 (старые фикстуры и записи): только тайминг.
pub const DMR1_MAGIC: [u8; 4] = *b"DMR1";
/// Магия DMR2: тайминг + позиция указателя в ноте + один трек (уже не пишем).
pub const DMR2_MAGIC: [u8; 4] = *b"DMR2";
/// Магия DMR3: два независимых трека — правая рука (мышь) и левая (перо).
pub const DMR3_MAGIC: [u8; 4] = *b"DMR3";
/// Дельта/позиция «ноты не было» (промах, нет указателя).
pub const DMR1_MISS: i8 = -128;
/// Предел обычной дельты/координаты.
const DMR_MAX: i8 = 127;
/// `dt_ms = 255` — следом идёт u16 LE (порядок как у сервера: dt, u16, x).
const TRACK_DT_ESCAPE: u8 = 255;
/// Больше сервер не принимает (на каждый трек).
pub const TRACK_MAX_SAMPLES: usize = 20_000;
/// Тот же предел, что у сервера: один блоб не больше 256 КиБ.
const MAX_BLOB_BYTES: usize = 256 * 1024;
/// Время трека ограничено шестью часами (как на сервере).
const MAX_TRACK_MS: u32 = 6 * 60 * 60 * 1000;
/// Период обязательного сэмпла. Чуть меньше 80 мс, чтобы кадр 60 Гц не
/// оставлял дыру больше 80 мс.
pub const TRACK_INTERVAL: f32 = 0.060;
/// Сэмпл сверх периода, если указатель ушёл на 0.1 pos от прошлого.
pub const TRACK_POS_STEP: f32 = 0.1;
/// Сколько после попадания маркер держится ровно на `x_note` (press proof).
pub const CURSOR_HOLD: f32 = 0.05;
/// Сколько после попадания маркер этой руки остаётся в pressed-состоянии
/// (`stage::marker` шире и ярче), как при зажатой кнопке в игре.
pub const MARKER_PRESS: f32 = 0.10;

/// Версия разобранного блоба.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayFormat {
    Dmr1,
    Dmr2,
    Dmr3,
}

/// Какой руке принадлежит луч. Левая — перо/планшет, правая — мышь.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeamSide {
    Left,
    Right,
}

/// Одна нота: тайминг + позиция указателя в момент судейства.
/// `x` — в координатах сцены (chart pos, -2..2), как в контракте; -128 — мисс.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteShot {
    pub delta: i8,
    pub x: i8,
}

impl NoteShot {
    pub fn is_miss(&self) -> bool {
        self.delta == DMR1_MISS
    }

    /// Позиция указателя в координатах сцены; None — попадания не было.
    pub fn x_pos(&self) -> Option<f32> {
        if self.x == DMR1_MISS {
            None
        } else {
            Some(q_to_pos(self.x))
        }
    }
}

/// Один сэмпл трека: время карты и позиция указателя в координатах сцены
/// (chart pos, -2..2). Мировая X получается через [`crate::stage::note_x`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackPoint {
    pub t: f32,
    pub x: f32,
}

/// Разобранный блоб: ноты + два независимых трека.
///
/// `DMR1` — без треков, `DMR2` — один трек (исторически указатель; читаем как
/// левый луч), `DMR3` — правая (мышь) и левая (перо) руки отдельно.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayBlob {
    pub format: ReplayFormat,
    pub notes: Vec<NoteShot>,
    /// Левая рука — перо/планшет.
    pub left: Vec<TrackPoint>,
    /// Правая рука — мышь.
    pub right: Vec<TrackPoint>,
}

impl ReplayBlob {
    pub fn note_count(&self) -> usize {
        self.notes.len()
    }

    pub fn has_cursor(&self) -> bool {
        self.format != ReplayFormat::Dmr1 && !(self.left.is_empty() && self.right.is_empty())
    }

    /// Мир → chart pos. Контракт клампит x в [-2,2] — это диапазон `Note::pos`,
    /// а не world x (у нот он ±3.26); поэтому в блоб едет именно pos.
    fn world_to_pos(x: f32) -> f32 {
        let scale = note_x(1.0);
        if !x.is_finite() || scale <= 0.0 {
            0.0
        } else {
            x / scale
        }
    }

    /// Блоб всегда пишется как DMR3 (два трека: right, затем left).
    pub fn encode(&self) -> Vec<u8> {
        let count = self.notes.len().min(u16::MAX as usize);
        let mut out = Vec::with_capacity(
            6 + count * 2 + 12 + (self.left.len() + self.right.len()) * 3,
        );
        out.extend_from_slice(&DMR3_MAGIC);
        out.extend_from_slice(&(count as u16).to_le_bytes());
        for shot in self.notes.iter().take(count) {
            out.push(shot.delta as u8);
            out.push(shot.x as u8);
        }
        encode_track(&mut out, &self.right);
        encode_track(&mut out, &self.left);
        out
    }

    /// Имя сохранено для старых вызовов; формат на проводе теперь DMR3.
    pub fn encode_dmr2(&self) -> Vec<u8> {
        self.encode()
    }

    /// Строгий разбор DMR1/DMR2/DMR3. Хвостовые байты, битая длина трека,
    /// слишком много сэмплов и время вне 6 часов — ошибка.
    pub fn decode(bytes: &[u8]) -> Result<ReplayBlob, String> {
        if bytes.len() < 6 {
            return Err("replay is shorter than its header".into());
        }
        if bytes.len() > MAX_BLOB_BYTES {
            return Err("replay is too large".into());
        }
        let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        if &bytes[0..4] == &DMR1_MAGIC {
            let len = 6 + count;
            if bytes.len() != len {
                return Err("replay note count does not match its size".into());
            }
            return Ok(ReplayBlob {
                format: ReplayFormat::Dmr1,
                notes: bytes[6..]
                    .iter()
                    .map(|b| NoteShot {
                        delta: *b as i8,
                        x: DMR1_MISS,
                    })
                    .collect(),
                left: Vec::new(),
                right: Vec::new(),
            });
        }
        let dmr2 = &bytes[0..4] == &DMR2_MAGIC;
        if !dmr2 && &bytes[0..4] != &DMR3_MAGIC {
            return Err("replay magic is not DMR1/DMR2/DMR3".into());
        }
        let mut pos = 6usize;
        let note_bytes = count
            .checked_mul(2)
            .ok_or_else(|| "replay note table is too large".to_string())?;
        if bytes.len() < pos + note_bytes {
            return Err("replay is shorter than its note table".into());
        }
        let mut notes = Vec::with_capacity(count);
        for _ in 0..count {
            notes.push(NoteShot {
                delta: bytes[pos] as i8,
                x: bytes[pos + 1] as i8,
            });
            pos += 2;
        }
        let (left, right) = if dmr2 {
            // старый DMR2: единственный трек исторически мышиный → правый
            (Vec::new(), decode_track(bytes, &mut pos)?)
        } else {
            // контракт DMR3: сначала правая рука, затем левая
            let right = decode_track(bytes, &mut pos)?;
            let left = decode_track(bytes, &mut pos)?;
            (left, right)
        };
        if pos != bytes.len() {
            return Err("trailing bytes after the replay".into());
        }
        Ok(ReplayBlob {
            format: if dmr2 { ReplayFormat::Dmr2 } else { ReplayFormat::Dmr3 },
            notes,
            left,
            right,
        })
    }
}

/// Записать один трек: `u16 count, u32 first_t_ms, samples[dt,x]`.
fn encode_track(out: &mut Vec<u8>, track: &[TrackPoint]) {
    let points = track
        .len()
        .min(TRACK_MAX_SAMPLES)
        .min(u16::MAX as usize);
    out.extend_from_slice(&(points as u16).to_le_bytes());
    let first_ms = if points == 0 {
        0
    } else {
        (track[0].t.max(0.0) * 1000.0)
            .round()
            .min(MAX_TRACK_MS as f32) as u32
    };
    out.extend_from_slice(&first_ms.to_le_bytes());
    let mut prev = first_ms as i64;
    for (i, p) in track.iter().take(points).enumerate() {
        let t_ms = (p.t.max(0.0) * 1000.0)
            .round()
            .max(0.0)
            .min(MAX_TRACK_MS as f32) as i64;
        // время монотонно: unsigned dt не должен уходить назад
        let t_ms = t_ms.max(prev);
        let dt = if i == 0 {
            0
        } else {
            (t_ms - prev).min(u16::MAX as i64) as u32
        };
        if dt >= TRACK_DT_ESCAPE as u32 {
            out.push(TRACK_DT_ESCAPE);
            out.extend_from_slice(&(dt as u16).to_le_bytes());
        } else {
            out.push(dt as u8);
        }
        out.push(pos_to_q(p.x) as u8);
        prev = t_ms;
    }
}

/// Прочитать один трек начиная с `pos`.
fn decode_track(bytes: &[u8], pos: &mut usize) -> Result<Vec<TrackPoint>, String> {
    if bytes.len() < *pos + 6 {
        return Err("replay track header is truncated".into());
    }
    let track_count = u16::from_le_bytes(
        bytes[*pos..*pos + 2]
            .try_into()
            .map_err(|_| "replay track header is truncated".to_string())?,
    ) as usize;
    *pos += 2;
    if track_count > TRACK_MAX_SAMPLES {
        return Err("replay track has too many samples".into());
    }
    let first_t = u32::from_le_bytes(
        bytes[*pos..*pos + 4]
            .try_into()
            .map_err(|_| "replay track header is truncated".to_string())?,
    );
    *pos += 4;
    if first_t > MAX_TRACK_MS {
        return Err("replay track time is out of range".into());
    }
    let mut t = first_t;
    let mut track = Vec::with_capacity(track_count);
    for _ in 0..track_count {
        let dt_byte = *bytes
            .get(*pos)
            .ok_or_else(|| "replay track is truncated".to_string())? as u32;
        *pos += 1;
        let dt = if dt_byte == TRACK_DT_ESCAPE as u32 {
            let raw: [u8; 2] = bytes
                .get(*pos..*pos + 2)
                .ok_or_else(|| "replay track dt is truncated".to_string())?
                .try_into()
                .unwrap();
            *pos += 2;
            u16::from_le_bytes(raw) as u32
        } else {
            dt_byte
        };
        t = t
            .checked_add(dt)
            .ok_or_else(|| "replay track time overflows".to_string())?;
        if t > MAX_TRACK_MS {
            return Err("replay track time is out of range".into());
        }
        let xb = *bytes
            .get(*pos)
            .ok_or_else(|| "replay track is truncated".to_string())? as i8;
        *pos += 1;
        track.push(TrackPoint {
            t: t as f32 / 1000.0,
            x: q_to_pos(xb),
        });
    }
    Ok(track)
}

/// Квантование chart pos (-2..2) в i8. -128 не используется: это мисс.
fn pos_to_q(pos: f32) -> i8 {
    if !pos.is_finite() {
        return 0;
    }
    ((pos.clamp(-2.0, 2.0) / 2.0 * 127.0).round() as i32).clamp(-127, 127) as i8
}

/// Обратное квантование; -128 (не должно встречаться в треке) даёт -2.0.
fn q_to_pos(q: i8) -> f32 {
    (q.max(-127) as f32 / 127.0) * 2.0
}

/// DMR3 по сыгранному плею. `right` — трек мыши, `left` — трек пера.
/// `None`, если чарт и плей разной длины (например, чарт перезагрузили).
pub fn dmr3_from_play(
    chart: &Chart,
    play: &Play,
    right: &[TrackPoint],
    left: &[TrackPoint],
) -> Option<ReplayBlob> {
    if chart.notes.len() != play.states.len() || chart.notes.len() > u16::MAX as usize {
        return None;
    }
    let notes = chart
        .notes
        .iter()
        .zip(&play.states)
        .map(|(n, st)| {
            if !st.judged || st.result == Judge::Miss {
                // мисс: ни времени, ни указателя
                NoteShot {
                    delta: DMR1_MISS,
                    x: DMR1_MISS,
                }
            } else {
                let delta = ((st.hit_time - n.time) * 1000.0)
                    .round()
                    .clamp(-(DMR_MAX as f32), DMR_MAX as f32) as i8;
                let x = if st.pointer_x.is_finite() {
                    pos_to_q(ReplayBlob::world_to_pos(st.pointer_x))
                } else {
                    // попадание без записанной позиции (не должно случаться):
                    // всё равно даём кадр нажатия — ставим x самой ноты
                    pos_to_q(n.pos)
                };
                NoteShot { delta, x }
            }
        })
        .collect();
    Some(ReplayBlob {
        format: ReplayFormat::Dmr3,
        notes,
        left: left.iter().take(TRACK_MAX_SAMPLES).copied().collect(),
        right: right.iter().take(TRACK_MAX_SAMPLES).copied().collect(),
    })
}

/// Тело поля `replay` для `POST /api/scores`: base64 от DMR3.
pub fn dmr3_base64(
    chart: &Chart,
    play: &Play,
    right: &[TrackPoint],
    left: &[TrackPoint],
) -> Option<String> {
    dmr3_from_play(chart, play, right, left).map(|b| base64_encode(&b.encode()))
}

/// Стандартный base64 (RFC 4648) с `=`. Сервер принимает ровно его.
pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Обратный разбор base64 (для тестов и локальных фикстур).
#[allow(dead_code)]
pub fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    fn sextet(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4 + 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for b in text.bytes() {
        if b.is_ascii_whitespace() || b == b'=' {
            continue;
        }
        let v = sextet(b).ok_or_else(|| "bad base64 character".to_string())? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
            acc &= (1u32 << bits) - 1;
        }
    }
    Ok(out)
}

/// Проигрывание записанного плея: дельты + позиции + кто уже применён.
///
/// Порядок применения — по времени срабатывания (`note.time + delta`), а не
/// по индексу: соседние дельты (±127 мс) могут поменять ноты местами. Каждая
/// нота применяется ровно один раз, даже если кадр пропустил её время.
#[derive(Debug)]
pub struct ReplayDriver {
    pub nick: String,
    notes: Vec<NoteShot>,
    applied: Vec<bool>,
    order: Vec<usize>,
    times: Vec<f32>,
    /// Левая (перо) и правая (мышь) дорожки в chart pos.
    left: Vec<TrackPoint>,
    right: Vec<TrackPoint>,
    has_cursor: bool,
    /// Последнее реальное попадание на каждую руку (snap + press).
    last_hits: [Option<HitSnap>; 2],
    cursor: usize,
}

#[derive(Clone, Copy, Debug)]
struct HitSnap {
    t: f32,
    x: f32,
}

fn side_index(side: BeamSide) -> usize {
    match side {
        BeamSide::Left => 0,
        BeamSide::Right => 1,
    }
}

impl ReplayDriver {
    pub fn new(chart: &Chart, blob: &ReplayBlob, nick: &str) -> Result<ReplayDriver, String> {
        if blob.notes.len() != chart.notes.len() {
            return Err(format!(
                "replay has {} notes, chart has {}",
                blob.notes.len(),
                chart.notes.len()
            ));
        }
        // Промах не хранит время попадания: показываем его там же, где игра
        // сама ставит авто-Miss — через click-окно после ноты.
        let times: Vec<f32> = chart
            .notes
            .iter()
            .zip(&blob.notes)
            .map(|(n, shot)| {
                if shot.is_miss() {
                    n.time + CLICK_WINDOW
                } else {
                    n.time + shot.delta as f32 / 1000.0
                }
            })
            .collect();
        let mut order: Vec<usize> = (0..chart.notes.len()).collect();
        order.sort_by(|&a, &b| {
            times[a]
                .partial_cmp(&times[b])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(&b))
        });
        Ok(ReplayDriver {
            nick: nick.to_string(),
            notes: blob.notes.clone(),
            applied: vec![false; chart.notes.len()],
            order,
            times,
            left: blob.left.clone(),
            right: blob.right.clone(),
            has_cursor: blob.has_cursor(),
            last_hits: [None, None],
            cursor: 0,
        })
    }

    pub fn has_cursor(&self) -> bool {
        self.has_cursor
    }

    /// (левая, правая) дорожки в chart pos.
    pub fn tracks(&self) -> (&[TrackPoint], &[TrackPoint]) {
        (&self.left, &self.right)
    }

    /// Время последнего срабатывания (для выхода из просмотра с хвостом).
    pub fn end(&self) -> f32 {
        self.order.last().map(|&i| self.times[i]).unwrap_or(0.0)
    }

    #[allow(dead_code)]
    pub fn applied_count(&self) -> usize {
        self.applied.iter().filter(|a| **a).count()
    }

    #[allow(dead_code)]
    pub fn applied(&self, i: usize) -> bool {
        self.applied.get(i).copied().unwrap_or(false)
    }

    /// X маркера в chart pos: на кадре попадания своей рукой маркер стоит
    /// ровно на `x_note` короткое время; иначе — интерполяция своего трека.
    pub fn beam_x(&self, t: f32, side: BeamSide) -> Option<f32> {
        if !self.has_cursor {
            return None;
        }
        if let Some(snap) = self.last_hits[side_index(side)] {
            if t >= snap.t && t <= snap.t + CURSOR_HOLD {
                return Some(snap.x);
            }
        }
        let track = match side {
            BeamSide::Left => &self.left,
            BeamSide::Right => &self.right,
        };
        track_x(track, t)
    }

    /// Позиция маркера и его pressed-состояние для конкретной руки.
    pub fn marker_state(&self, t: f32, side: BeamSide) -> Option<(f32, bool)> {
        let x = self.beam_x(t, side)?;
        let pressed = self
            .last_hits[side_index(side)]
            .is_some_and(|s| t >= s.t && t <= s.t + MARKER_PRESS);
        Some((x, pressed))
    }

    /// Применить все ноты, время которых наступило. Возвращает число
    /// применённых в этом вызове (0 — кадр ничего не сдвинул).
    pub fn apply(&mut self, play: &mut Play, chart: &Chart, t: f32) -> usize {
        let mut applied = 0;
        while self.cursor < self.order.len() {
            let i = self.order[self.cursor];
            if self.times[i] > t {
                break;
            }
            self.cursor += 1;
            if self.applied[i] {
                continue;
            }
            self.applied[i] = true;
            let note = &chart.notes[i];
            let shot = self.notes[i];
            let hit_time = if shot.is_miss() {
                note.time + CLICK_WINDOW
            } else {
                note.time + shot.delta as f32 / 1000.0
            };
            let result = play.replay_hit(chart, i, hit_time);
            if result != Judge::Miss {
                if let Some(x) = shot.x_pos() {
                    // прилипает ближайший к x_note луч; если трек один —
                    // всегда он, второй продолжает свою дорожку
                    let l = track_x(&self.left, hit_time);
                    let r = track_x(&self.right, hit_time);
                    let side = match (l, r) {
                        (Some(l), Some(r)) => {
                            if (l - x).abs() <= (r - x).abs() {
                                BeamSide::Left
                            } else {
                                BeamSide::Right
                            }
                        }
                        (Some(_), None) => BeamSide::Left,
                        (None, Some(_)) => BeamSide::Right,
                        (None, None) => BeamSide::Left,
                    };
                    self.last_hits[side_index(side)] = Some(HitSnap { t: hit_time, x });
                }
            }
            applied += 1;
        }
        applied
    }
}

/// Интерполяция одного трека; до первого/после последнего — зажим на краях.
fn track_x(track: &[TrackPoint], t: f32) -> Option<f32> {
    let first = track.first()?;
    let last = track.last()?;
    if t <= first.t {
        return Some(first.x);
    }
    if t >= last.t {
        return Some(last.x);
    }
    let i = track.partition_point(|p| p.t <= t);
    let a = track.get(i.checked_sub(1)?)?;
    let b = track.get(i)?;
    let span = (b.t - a.t).max(1e-6);
    let f = ((t - a.t) / span).clamp(0.0, 1.0);
    Some(a.x + (b.x - a.x) * f)
}

// источники
const SRC_MOUSE: u8 = 0;
// типы событий
const EV_MOTION: u8 = 0;
const EV_DOWN: u8 = 1;
const EV_UP: u8 = 2;
const EV_REPORT: u8 = 3;
const EV_TAP: u8 = 4;
const EV_SLIDE: u8 = 5;

/// Компактное событие (Copy, без heap-аллокаций на каждый ввод).
#[derive(Clone, Copy)]
struct Ev {
    t: f32,
    src: u8,
    ev: u8,
    x: f32,
    dx: f32,
    x_raw: i32,
    p: i32,
    hover: bool,
    press: bool,
    xn: f32,
    judge: i8, // -1 нет, 0 Perfect, 1 Good, 2 Miss
    err_ms: f32,
    raw: [u8; 16],
    raw_len: u8,
}

impl Default for Ev {
    fn default() -> Self {
        Ev {
            t: 0.0,
            src: SRC_MOUSE,
            ev: EV_MOTION,
            x: 0.0,
            dx: 0.0,
            x_raw: 0,
            p: 0,
            hover: false,
            press: false,
            xn: 0.0,
            judge: -1,
            err_ms: 0.0,
            raw: [0; 16],
            raw_len: 0,
        }
    }
}

fn src_name(s: u8) -> &'static str {
    match s {
        0 => "mouse",
        1 => "parblo",
        2 => "gaomon",
        _ => "?",
    }
}

fn judge_name(j: i8) -> &'static str {
    match j {
        0 => "Perfect",
        1 => "Good",
        2 => "Miss",
        _ => "",
    }
}

fn judge_code(j: Judge) -> i8 {
    match j {
        Judge::Perfect => 0,
        Judge::Good => 1,
        Judge::Miss => 2,
    }
}

fn kind_name(k: NoteKind) -> &'static str {
    match k {
        NoteKind::Normal => "normal",
        NoteKind::Slide => "slide",
        NoteKind::Swipe => "swipe",
        NoteKind::NoSound => "nosound",
    }
}

/// Мировая X → chart pos для треков.
fn world_to_chart_pos(x: f32) -> f32 {
    let scale = note_x(1.0);
    if x.is_finite() && scale > 0.0 {
        x / scale
    } else {
        0.0
    }
}

/// Общее правило сэмплирования одного указателя: старт, ≤80 мс (пишем 60),
/// |Δx| ≥ 0.1 pos, финал; кап 20 000.
fn push_track(track: &mut Vec<TrackPoint>, t: f32, pos: f32, force: bool) {
    let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
    let Some(last) = track.last().copied() else {
        if track.len() < TRACK_MAX_SAMPLES {
            track.push(TrackPoint { t, x: pos });
        }
        return;
    };
    // время трека монотонно, даже если часы дёрнулись
    let t = t.max(last.t);
    if force && (t - last.t).abs() < 1e-4 && (pos - last.x).abs() < 1e-4 {
        return;
    }
    let due = force
        || t - last.t >= TRACK_INTERVAL
        || (pos - last.x).abs() >= TRACK_POS_STEP;
    if !due {
        return;
    }
    if track.len() >= TRACK_MAX_SAMPLES {
        // «один в конце» должен остаться даже на капе
        if force {
            let n = track.len();
            track[n - 1] = TrackPoint { t, x: pos };
        }
        return;
    }
    track.push(TrackPoint { t, x: pos });
}

pub struct Replay {
    events: Vec<Ev>,
    song_id: String,
    diff: String,
    title: String,
    speed: f32,
    offset_ms: f32,
    tablet_zoom: f32,
    x_tolerance: f32,
    mouse_offset_ms: f32,
    tablet_offset_ms: f32,
    notes: Vec<Value>,
    /// независимые треки в chart pos: правая рука (мышь) и левая (перо);
    /// старт, каждые ~60 мс, при сдвиге ≥0.1 pos и финальный сэмпл.
    mouse_track: Vec<TrackPoint>,
    pen_track: Vec<TrackPoint>,
    started: Instant,
}

impl Replay {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        song_id: &str,
        diff: &str,
        title: &str,
        chart: &Chart,
        speed: f32,
        offset_ms: f32,
        tablet_zoom: f32,
        x_tolerance: f32,
    ) -> Replay {
        let notes = chart
            .notes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                json!({"i": i, "t": n.time, "x": note_x(n.pos), "kind": kind_name(n.kind)})
            })
            .collect();
        Replay {
            events: Vec::new(),
            song_id: song_id.to_string(),
            diff: diff.to_string(),
            title: title.to_string(),
            speed,
            offset_ms,
            tablet_zoom,
            x_tolerance,
            mouse_offset_ms: 0.0,
            tablet_offset_ms: 0.0,
            notes,
            mouse_track: Vec::new(),
            pen_track: Vec::new(),
            started: Instant::now(),
        }
    }

    /// Сэмпл трека мыши (правая рука). `x` — мировая X, внутри chart pos.
    pub fn track_mouse(&mut self, t: f32, x: f32) {
        let pos = world_to_chart_pos(x);
        push_track(&mut self.mouse_track, t, pos, false);
    }

    /// Сэмпл трека пера (левая рука), в том числе при hover.
    pub fn track_pen(&mut self, t: f32, x: f32) {
        let pos = world_to_chart_pos(x);
        push_track(&mut self.pen_track, t, pos, false);
    }

    /// Финальные сэмплы обеих рук. Перо без свежей позиции продлеваем его
    /// последним известным x, чтобы дорожка доехала до конца.
    pub fn track_end(&mut self, t: f32, mouse_x: f32, pen_x: Option<f32>) {
        let m = world_to_chart_pos(mouse_x);
        push_track(&mut self.mouse_track, t, m, true);
        if let Some(px) = pen_x {
            let p = world_to_chart_pos(px);
            push_track(&mut self.pen_track, t, p, true);
        } else if let Some(last) = self.pen_track.last().copied() {
            push_track(&mut self.pen_track, t, last.x, true);
        }
    }

    /// (левая, правая) дорожки в chart pos для DMR3.
    pub fn tracks(&self) -> (&[TrackPoint], &[TrackPoint]) {
        (&self.pen_track, &self.mouse_track)
    }

    /// Раздельные оффсеты ввода (для meta реплея).
    pub fn set_input_offsets(&mut self, mouse: f32, tablet: f32) {
        self.mouse_offset_ms = mouse;
        self.tablet_offset_ms = tablet;
    }

    pub fn mouse_motion(&mut self, t: f32, dx: f32, x: f32) {
        self.events.push(Ev {
            t,
            src: SRC_MOUSE,
            ev: EV_MOTION,
            dx,
            x,
            ..Default::default()
        });
    }

    pub fn mouse_button(&mut self, t: f32, down: bool, x: f32) {
        self.events.push(Ev {
            t,
            src: SRC_MOUSE,
            ev: if down { EV_DOWN } else { EV_UP },
            x,
            ..Default::default()
        });
    }

    pub fn tablet(&mut self, t: f32, e: &RawTabletEvent) {
        let src = match e.kind {
            "parblo" => 1,
            "gaomon" => 2,
            _ => 3,
        };
        self.events.push(Ev {
            t,
            src,
            ev: EV_REPORT,
            x_raw: e.x_raw,
            p: e.pressure,
            hover: e.hover,
            press: e.press,
            xn: e.x_norm,
            raw: e.raw,
            raw_len: e.raw_len,
            ..Default::default()
        });
    }

    pub fn action(&mut self, t: f32, src: u8, tap: bool, x: f32, judge: Option<Judge>, err_ms: f32) {
        self.events.push(Ev {
            t,
            src,
            ev: if tap { EV_TAP } else { EV_SLIDE },
            x,
            judge: judge.map(judge_code).unwrap_or(-1),
            err_ms,
            ..Default::default()
        });
    }

    fn ev_json(e: &Ev) -> Value {
        let mut v = json!({"t": e.t, "src": src_name(e.src)});
        match e.ev {
            EV_MOTION => {
                v["ev"] = json!("motion");
                v["dx"] = json!(e.dx);
                v["x"] = json!(e.x);
            }
            EV_DOWN | EV_UP => {
                v["ev"] = json!(if e.ev == EV_DOWN { "down" } else { "up" });
                v["x"] = json!(e.x);
            }
            EV_REPORT => {
                let raw: String = e.raw[..e.raw_len as usize]
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                v["ev"] = json!("report");
                v["x_raw"] = json!(e.x_raw);
                v["p"] = json!(e.p);
                v["hover"] = json!(e.hover);
                v["press"] = json!(e.press);
                v["xn"] = json!(e.xn);
                v["raw"] = json!(raw);
            }
            EV_TAP | EV_SLIDE => {
                v["ev"] = json!(if e.ev == EV_TAP { "tap" } else { "slide" });
                v["x"] = json!(e.x);
                if e.judge >= 0 {
                    v["judge"] = json!(judge_name(e.judge));
                    v["err_ms"] = json!(e.err_ms);
                }
            }
            _ => {}
        }
        v
    }

    /// Сохранить реплей в data/replays/<song>_<diff>_<ts>.json.
    pub fn save(&self, play: &Play) -> anyhow::Result<std::path::PathBuf> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let dir = "data/replays";
        std::fs::create_dir_all(dir)?;
        let path = format!("{}/{}_{}_{}.json", dir, self.song_id, self.diff, ts);
        let events: Vec<Value> = self.events.iter().map(Self::ev_json).collect();
        let doc = json!({
            "meta": {
                "song": self.song_id,
                "diff": self.diff,
                "title": self.title,
                "speed": self.speed,
                "offset_ms": self.offset_ms,
                "perfect_ms": crate::play::PERFECT_MS,
                "good_ms": crate::play::GOOD_MS,
                "tablet_zoom": self.tablet_zoom,
                "x_tolerance": self.x_tolerance,
                "mouse_offset_ms": self.mouse_offset_ms,
                "tablet_offset_ms": self.tablet_offset_ms,
                "timestamp": ts,
                "duration_s": self.started.elapsed().as_secs_f32(),
                "score": play.score,
                "max_combo": play.max_combo,
                "counts": {"perfect": play.counts[0], "good": play.counts[1], "miss": play.counts[2]},
                "accuracy": play.accuracy(),
                "mean_err_ms": play.mean_error() * 1000.0,
                "event_count": self.events.len(),
            },
            "notes": self.notes,
            "events": events,
        });
        std::fs::write(&path, serde_json::to_string(&doc)?)?;
        Ok(path.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::{Chart, Note, NoteKind};
    use crate::play::{Judge, Play};

    fn chart_with(times: &[f32]) -> Chart {
        Chart {
            speed: 6.0,
            notes: times
                .iter()
                .map(|&t| Note {
                    time: t,
                    pos: 0.0,
                    size: 1.0,
                    speed: 1.0,
                    kind: NoteKind::Normal,
                    hold: 0.0,
                    sounds: vec![],
                })
                .collect(),
            links: vec![],
        }
    }

    fn judged(play: &mut Play, i: usize, result: Judge, hit_time: f32) {
        play.states[i].judged = true;
        play.states[i].result = result;
        play.states[i].hit_time = hit_time;
        play.states[i].pointer_x = if result == Judge::Miss {
            f32::NAN
        } else {
            crate::stage::note_x(0.0)
        };
    }

    fn judged_at(play: &mut Play, i: usize, result: Judge, hit_time: f32, pointer_x: f32) {
        judged(play, i, result, hit_time);
        play.states[i].pointer_x = pointer_x;
    }

    /// DMR3: ноты, оба трека и escape dt > 254 переживают round-trip.
    #[test]
    fn dmr3_round_trip_keeps_notes_and_two_tracks() {
        let chart = chart_with(&[1.0, 2.0, 3.0]);
        let mut play = Play::new(&chart);
        judged_at(&mut play, 0, Judge::Perfect, 1.020, note_x(2.0));
        judged_at(&mut play, 1, Judge::Good, 1.950, note_x(-2.0));
        judged_at(&mut play, 2, Judge::Perfect, 3.100, note_x(0.0));
        // x — уже квантованные края, чтобы сравнение было точным
        let right = vec![
            TrackPoint { t: 0.5, x: 0.0 },
            TrackPoint { t: 0.75, x: 2.0 },
            TrackPoint { t: 1.5, x: -2.0 },
        ];
        let left = vec![
            TrackPoint { t: 0.5, x: -2.0 },
            TrackPoint { t: 1.25, x: q_to_pos(64) },
        ];
        let blob = dmr3_from_play(&chart, &play, &right, &left).expect("matching lengths");
        assert_eq!(blob.format, ReplayFormat::Dmr3);
        assert_eq!(
            blob.notes.iter().map(|n| n.delta).collect::<Vec<_>>(),
            vec![20, -50, 100]
        );
        assert_eq!(
            blob.notes.iter().map(|n| n.x).collect::<Vec<_>>(),
            vec![127, -127, 0]
        );
        let bytes = blob.encode();
        assert_eq!(&bytes[0..4], b"DMR3");
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 3);
        let back = ReplayBlob::decode(&bytes).expect("decode");
        assert_eq!(back, blob, "notes, x and both tracks round-trip exactly");
        assert_eq!(back.note_count(), chart.notes.len());
        assert!(back.has_cursor());
    }

    /// Точная раскладка DMR3: right-трек, затем left-трек; escape `dt` — это
    /// u16 между dt-байтом и x (так его читает сервер).
    #[test]
    fn dmr3_wire_layout_is_exact() {
        let blob = ReplayBlob {
            format: ReplayFormat::Dmr3,
            notes: vec![
                NoteShot { delta: 5, x: 10 },
                NoteShot { delta: DMR1_MISS, x: DMR1_MISS },
            ],
            right: vec![
                TrackPoint { t: 1.0, x: 0.0 },
                TrackPoint { t: 1.316, x: q_to_pos(5) },
                TrackPoint { t: 1.326, x: q_to_pos(-5) },
            ],
            left: vec![
                TrackPoint { t: 1.2, x: q_to_pos(3) },
                TrackPoint { t: 1.207, x: q_to_pos(-3) },
            ],
        };
        let expected: Vec<u8> = vec![
            b'D', b'M', b'R', b'3', 2, 0, // magic + note_count
            5, 10, // note 0: delta 5, x 10
            0x80, 0x80, // note 1: miss / no x
            // right
            3, 0, // right_count
            0xE8, 0x03, 0x00, 0x00, // first_t_ms = 1000
            0, 0, // dt=0, x=0
            255, 0x3C, 0x01, 5, // dt=255+316, x=5
            10, 0xFB, // dt=10, x=-5
            // left
            2, 0, // left_count
            0xB0, 0x04, 0x00, 0x00, // first_t_ms = 1200
            0, 3, // dt=0, x=3
            7, 0xFD, // dt=7, x=-3
        ];
        assert_eq!(blob.encode(), expected);
        let back = ReplayBlob::decode(&expected).expect("decode exact layout");
        assert_eq!(back.notes, blob.notes);
        assert_eq!(back.right.len(), 3);
        assert_eq!(back.left.len(), 2);
        assert!((back.right[1].t - 1.316).abs() < 1e-4);
        assert_eq!(back.right[1].x, q_to_pos(5));
        assert_eq!(back.right[2].x, q_to_pos(-5));
        assert_eq!(back.left[1].x, q_to_pos(-3));
    }

    /// Старый DMR2 (один трек) читается как мышиный — правый маркер.
    #[test]
    fn dmr2_single_track_decodes_as_the_mouse_marker() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"DMR2");
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 10, 0x40]);
        let blob = ReplayBlob::decode(&bytes).expect("old DMR2");
        assert_eq!(blob.format, ReplayFormat::Dmr2);
        assert!(blob.left.is_empty());
        assert_eq!(blob.right.len(), 2);
        let chart = chart_with(&[1.0]);
        let driver = ReplayDriver::new(&chart, &blob, "old").unwrap();
        assert_eq!(driver.beam_x(0.0, BeamSide::Right), Some(0.0));
        assert_eq!(driver.beam_x(10.0, BeamSide::Right), Some(q_to_pos(0x40)));
        assert_eq!(driver.beam_x(0.0, BeamSide::Left), None);
    }

    /// Хит с указателем за пределами сцены зажимается в [-2,2] pos; мисс
    /// пишет -128 в оба поля. DMR1 по-прежнему читается, но без лучей.
    #[test]
    fn dmr3_clamps_x_and_marks_misses_dmr1_still_decodes() {
        let chart = chart_with(&[1.0, 2.0, 3.0]);
        let mut play = Play::new(&chart);
        judged_at(&mut play, 0, Judge::Perfect, 1.0, 100.0);
        judged_at(&mut play, 1, Judge::Perfect, 2.0, -100.0);
        judged(&mut play, 2, Judge::Miss, 3.3);
        let blob = dmr3_from_play(&chart, &play, &[], &[]).unwrap();
        assert_eq!(blob.notes[0].x, 127);
        assert_eq!(blob.notes[1].x, -127);
        assert_eq!(blob.notes[2].delta, DMR1_MISS);
        assert_eq!(blob.notes[2].x, DMR1_MISS);
        assert_eq!(blob.notes[2].x_pos(), None);
        assert!((blob.notes[0].x_pos().unwrap() - 2.0).abs() < 1e-6);
        assert!((blob.notes[1].x_pos().unwrap() + 2.0).abs() < 1e-6);

        // попадание без pointer_x всё равно получает x ноты (press proof),
        // а не -128: -128 остаётся только у мисса
        let mut no_ptr = Play::new(&chart);
        no_ptr.states[0].judged = true;
        no_ptr.states[0].result = Judge::Perfect;
        no_ptr.states[0].hit_time = 1.0;
        no_ptr.states[0].pointer_x = f32::NAN;
        let fallback = dmr3_from_play(&chart, &no_ptr, &[], &[]).unwrap();
        assert_eq!(fallback.notes[0].x, 0, "x ноты (pos 0)");
        assert_ne!(fallback.notes[0].x, DMR1_MISS);

        // DMR1: длина 6 + note_count, x нет, треков нет
        let mut v1 = Vec::new();
        v1.extend_from_slice(b"DMR1");
        v1.extend_from_slice(&3u16.to_le_bytes());
        v1.extend_from_slice(&[5u8, (-5i8) as u8, (-128i8) as u8]);
        let old = ReplayBlob::decode(&v1).expect("old DMR1 fixture");
        assert_eq!(old.format, ReplayFormat::Dmr1);
        assert!(!old.has_cursor());
        assert!(old.left.is_empty() && old.right.is_empty());
        assert_eq!(
            old.notes.iter().map(|n| n.delta).collect::<Vec<_>>(),
            vec![5, -5, DMR1_MISS]
        );
        assert!(old.notes.iter().all(|n| n.x == DMR1_MISS));
    }

    /// DMR3: битые длины/хвосты и слишком длинный трек отвергаются;
    /// note-count мисматч ловит драйвер.
    #[test]
    fn dmr3_rejects_bad_lengths_and_oversized_tracks() {
        let good = ReplayBlob {
            format: ReplayFormat::Dmr3,
            notes: vec![NoteShot { delta: 5, x: 10 }],
            right: vec![
                TrackPoint { t: 1.0, x: 0.0 },
                TrackPoint { t: 1.5, x: 1.0 },
            ],
            left: vec![TrackPoint { t: 1.0, x: -1.0 }],
        }
        .encode();
        let mut wrong_count = good.clone();
        wrong_count[4] = 2; // note_count = 2, table is still 1
        assert!(ReplayBlob::decode(&wrong_count).is_err());
        assert!(ReplayBlob::decode(&good[..good.len() - 1]).is_err());
        assert!(ReplayBlob::decode(&good[..8]).is_err());
        assert!(ReplayBlob::decode(&good[..10]).is_err());
        let mut extra = good.clone();
        extra.push(0);
        assert!(ReplayBlob::decode(&extra).is_err());
        assert!(ReplayBlob::decode(b"DMR0\x00\x00").is_err());
        assert!(ReplayBlob::decode(b"DMR3").is_err());
        // right_count 20001 без данных: ошибка до чтения сэмплов
        let mut huge = Vec::new();
        huge.extend_from_slice(b"DMR3");
        huge.extend_from_slice(&0u16.to_le_bytes());
        huge.extend_from_slice(&(TRACK_MAX_SAMPLES as u16 + 1).to_le_bytes());
        huge.extend_from_slice(&0u32.to_le_bytes());
        assert!(ReplayBlob::decode(&huge).is_err());
        // слишком длинные треки не проходят и при кодировании (кап 20k)
        let long: Vec<TrackPoint> = (0..TRACK_MAX_SAMPLES + 50)
            .map(|i| TrackPoint { t: i as f32 * 0.1, x: 0.0 })
            .collect();
        let blob = ReplayBlob {
            format: ReplayFormat::Dmr3,
            notes: vec![],
            right: long.clone(),
            left: long,
        };
        let back = ReplayBlob::decode(&blob.encode()).unwrap();
        assert_eq!(back.right.len(), TRACK_MAX_SAMPLES);
        assert_eq!(back.left.len(), TRACK_MAX_SAMPLES);
    }

    /// DMR1-плей в просмотре не получает лучей — только старый бейдж.
    #[test]
    fn dmr1_play_has_no_beams() {
        let chart = chart_with(&[1.0]);
        let mut v1 = Vec::new();
        v1.extend_from_slice(b"DMR1");
        v1.extend_from_slice(&1u16.to_le_bytes());
        v1.push(0u8);
        let blob = ReplayBlob::decode(&v1).unwrap();
        let mut driver = ReplayDriver::new(&chart, &blob, "old").unwrap();
        let mut play = Play::new(&chart);
        driver.apply(&mut play, &chart, 1.0);
        assert_eq!(play.states[0].result, Judge::Perfect);
        assert!(!driver.has_cursor());
        assert_eq!(driver.beam_x(1.0, BeamSide::Left), None);
        assert_eq!(driver.beam_x(1.0, BeamSide::Right), None);
        let (l, r) = driver.tracks();
        assert!(l.is_empty() && r.is_empty());
    }

    /// На кадре попадания прилипает ближайший луч (right к x_note), второй
    /// продолжает свой трек; на миссе ничего не прилипает.
    #[test]
    fn replay_beams_snap_the_nearest_track_on_the_hit_frame() {
        let chart = chart_with(&[1.0, 2.0]);
        let x_hit = 1.5;
        let blob = ReplayBlob {
            format: ReplayFormat::Dmr3,
            notes: vec![
                NoteShot { delta: 0, x: pos_to_q(x_hit) },
                NoteShot { delta: DMR1_MISS, x: DMR1_MISS },
            ],
            // right проходит около x_hit, left стоит далеко слева
            right: vec![
                TrackPoint { t: 0.5, x: 0.0 },
                TrackPoint { t: 0.75, x: 2.0 },
                TrackPoint { t: 2.5, x: -2.0 },
            ],
            left: vec![
                TrackPoint { t: 0.0, x: -2.0 },
                TrackPoint { t: 3.0, x: -2.0 },
            ],
        };
        let mut driver = ReplayDriver::new(&chart, &blob, "kid").unwrap();
        let mut play = Play::new(&chart);
        driver.apply(&mut play, &chart, 1.0);
        let want = q_to_pos(pos_to_q(x_hit));
        let (rx, rp) = driver.marker_state(1.0, BeamSide::Right).expect("right marker");
        assert_eq!(rx, want, "hit is exact");
        assert!(rp, "right marker is pressed on the hit frame");
        let (lx, lp) = driver.marker_state(1.0, BeamSide::Left).expect("left marker");
        assert_eq!(lx, -2.0, "left keeps its track");
        assert!(!lp);
        // pressed-окно шире snap-окна; после него маркер возвращается на трек
        let t_after = 1.0 + MARKER_PRESS + 0.01;
        assert!(!driver.marker_state(t_after, BeamSide::Right).unwrap().1);
        let want_after = 2.0 - 4.0 * ((t_after - 0.75) / 1.75);
        assert!((driver.beam_x(t_after, BeamSide::Right).unwrap() - want_after).abs() < 1e-5);
        // мисс не двигает last_hits: к 2.3 прилипание давно кончилось
        driver.apply(&mut play, &chart, 2.3);
        assert_eq!(play.states[1].result, Judge::Miss);
        let after = driver.beam_x(2.3, BeamSide::Right).unwrap();
        assert!((after - -1.5428).abs() < 0.02, "right follows track: {after}");
        assert_eq!(driver.beam_x(2.3, BeamSide::Left), Some(-2.0));
    }

    /// Каждая рука ведёт свой маркер: попадание справа не двигает перо,
    /// попадание слева не двигает мышь; pressed-окно у каждой руки своё.
    #[test]
    fn marker_state_is_per_track_and_has_its_own_press_window() {
        let chart = chart_with(&[1.0, 1.5]);
        let blob = ReplayBlob {
            format: ReplayFormat::Dmr3,
            notes: vec![
                NoteShot { delta: 0, x: pos_to_q(0.0) },
                NoteShot { delta: 0, x: pos_to_q(2.0) },
            ],
            right: vec![
                TrackPoint { t: 0.5, x: 0.0 },
                TrackPoint { t: 2.0, x: 0.0 },
            ],
            left: vec![
                TrackPoint { t: 0.5, x: 2.0 },
                TrackPoint { t: 2.0, x: 2.0 },
            ],
        };
        let mut driver = ReplayDriver::new(&chart, &blob, "kid").unwrap();
        let mut play = Play::new(&chart);
        driver.apply(&mut play, &chart, 1.6);
        let (rx, rp) = driver.marker_state(1.6, BeamSide::Right).unwrap();
        let (lx, lp) = driver.marker_state(1.6, BeamSide::Left).unwrap();
        assert_eq!(rx, 0.0, "right track keeps its mouse X");
        assert!(!rp, "right press window (1.0..1.1) is over");
        assert_eq!(lx, 2.0, "left marker follows the pen hit");
        assert!(lp, "left marker is pressed on its hit");
        // press window exactly 100 ms
        assert!(driver.marker_state(1.0 + MARKER_PRESS, BeamSide::Right).unwrap().1);
        assert!(!driver.marker_state(1.0 + MARKER_PRESS + 0.001, BeamSide::Right).unwrap().1);
    }

    /// Сквозной тест записи: hit → x_note в DMR3; после decode драйвер
    /// прилипает правым лучом к той же позиции.
    #[test]
    fn dmr3_encode_then_play_reports_the_same_hit_x() {
        let chart = chart_with(&[1.0]);
        let mut play = Play::new(&chart);
        judged_at(&mut play, 0, Judge::Perfect, 1.0, note_x(2.0));
        let blob = dmr3_from_play(
            &chart,
            &play,
            &[TrackPoint { t: 0.0, x: 0.0 }],
            &[TrackPoint { t: 0.0, x: -2.0 }],
        )
        .unwrap();
        let back = ReplayBlob::decode(&blob.encode()).unwrap();
        let mut driver = ReplayDriver::new(&chart, &back, "kid").unwrap();
        let mut replay_play = Play::new(&chart);
        driver.apply(&mut replay_play, &chart, 1.0);
        assert_eq!(driver.beam_x(1.0, BeamSide::Right), Some(2.0));
        assert_eq!(driver.beam_x(1.0, BeamSide::Left), Some(-2.0));
    }

    /// Оба трека интерполируются независимо и зажимаются на краях.
    #[test]
    fn beams_interpolate_each_track_and_clamp_at_the_ends() {
        let chart = Chart::empty();
        let blob = ReplayBlob {
            format: ReplayFormat::Dmr3,
            notes: vec![],
            left: vec![
                TrackPoint { t: 0.5, x: 0.0 },
                TrackPoint { t: 0.8, x: 2.0 },
                TrackPoint { t: 1.1, x: -2.0 },
            ],
            right: vec![
                TrackPoint { t: 0.0, x: -1.0 },
                TrackPoint { t: 2.0, x: 1.0 },
            ],
        };
        let driver = ReplayDriver::new(&chart, &blob, "kid").unwrap();
        assert_eq!(driver.beam_x(0.0, BeamSide::Left), Some(0.0), "left clamps");
        assert!((driver.beam_x(0.65, BeamSide::Left).unwrap() - 1.0).abs() < 1e-6);
        assert!((driver.beam_x(0.95, BeamSide::Left).unwrap() - 0.0).abs() < 1e-6);
        assert_eq!(driver.beam_x(9.0, BeamSide::Left), Some(-2.0));
        assert!((driver.beam_x(1.0, BeamSide::Right).unwrap() - 0.0).abs() < 1e-6);
        assert_eq!(driver.beam_x(9.0, BeamSide::Right), Some(1.0));
    }

    /// Сэмплирование каждого указателя отдельно: старт, 60 мс, сдвиг ≥0.1,
    /// конец и кап на КАЖДЫЙ трек.
    #[test]
    fn track_sampling_is_independent_per_pointer() {
        let chart = Chart::empty();
        let mut r = Replay::new("s", "hard", "T", &chart, 6.5, 0.0, 1.0, 0.75);
        // отрицательный lead-in зажимается в 0; это стартовые сэмплы
        r.track_mouse(-2.0, 0.0);
        r.track_pen(-2.0, note_x(-1.0));
        let (left, right) = r.tracks();
        assert_eq!(right.len(), 1, "mouse start");
        assert_eq!(left.len(), 1, "pen start");
        assert_eq!(right[0].t, 0.0);
        assert!((left[0].x + 1.0).abs() < 1e-5);
        r.track_mouse(0.03, 0.0);
        assert_eq!(r.tracks().1.len(), 1, "period not reached");
        r.track_mouse(0.061, 0.0);
        assert_eq!(r.tracks().1.len(), 2, "next regular mouse sample");
        r.track_mouse(0.062, note_x(0.2));
        assert_eq!(r.tracks().1.len(), 3);
        assert!((r.tracks().1[2].x - 0.2).abs() < 1e-5, "mouse pos");
        // перо двигается независимо и не трогает мышь
        r.track_pen(0.061, note_x(0.5));
        assert_eq!(r.tracks().0.len(), 2);
        assert!((r.tracks().0[1].x - 0.5).abs() < 1e-5);
        assert_eq!(r.tracks().1.len(), 3, "pen does not touch mouse");
        // конец ставится принудительно в оба трека
        r.track_end(0.064, note_x(0.2), Some(note_x(0.5)));
        assert_eq!(r.tracks().1.len(), 4);
        assert_eq!(r.tracks().0.len(), 3);
        // кап: больше 20 000 не пускаем, но конец всё равно обновляет последний
        let mut big = Replay::new("s", "hard", "T", &chart, 6.5, 0.0, 1.0, 0.75);
        for i in 0..TRACK_MAX_SAMPLES + 100 {
            big.track_mouse(i as f32 * 0.1, note_x((i % 2) as f32));
        }
        assert_eq!(big.tracks().1.len(), TRACK_MAX_SAMPLES);
        assert!(big.tracks().0.is_empty());
        let last_before = *big.tracks().1.last().unwrap();
        big.track_end(30_000.0, note_x(1.0), Some(note_x(-1.0)));
        assert_eq!(big.tracks().1.len(), TRACK_MAX_SAMPLES, "cap is hard");
        assert_ne!(big.tracks().1.last().unwrap().t, last_before.t, "end still lands");
        assert_eq!(big.tracks().0.len(), 1, "pen gets its own end sample");
    }

    /// Драйвер идёт по фактическому времени срабатывания, а не по индексам:
    /// поздний delta у первой ноты не должен обгонять раннюю вторую.
    #[test]
    fn replay_driver_applies_notes_in_time_order_exactly_once() {
        let chart = chart_with(&[1.0, 1.05, 2.0]);
        let blob = ReplayBlob {
            format: ReplayFormat::Dmr3,
            notes: vec![
                NoteShot { delta: 127, x: 0 },
                NoteShot { delta: -100, x: 0 },
                NoteShot { delta: DMR1_MISS, x: DMR1_MISS },
            ],
            left: vec![],
            right: vec![],
        };
        let mut driver = ReplayDriver::new(&chart, &blob, "piano_kid").expect("driver");
        let mut play = Play::new(&chart);
        assert!((driver.end() - (2.0 + CLICK_WINDOW)).abs() < 1e-4);

        // до первой ноты — тишина
        assert_eq!(driver.apply(&mut play, &chart, 0.90), 0);
        // 1.05 − 0.100 = 0.950: вторая нота срабатывает раньше первой
        assert_eq!(driver.apply(&mut play, &chart, 1.00), 1);
        assert!(play.states[1].judged, "early note is applied first");
        assert!(!play.states[0].judged, "late first note waits for its time");
        // 1.0 + 0.127 = 1.127
        assert_eq!(driver.apply(&mut play, &chart, 1.20), 1);
        assert!(play.states[0].judged);
        // промах появляется через click-окно
        assert_eq!(driver.apply(&mut play, &chart, 2.10), 0);
        assert_eq!(driver.apply(&mut play, &chart, 2.40), 1);
        assert!(play.states[2].judged);
        assert_eq!(play.states[2].result, Judge::Miss);
        // повторный вызов уже ничего не трогает
        assert_eq!(driver.apply(&mut play, &chart, 99.0), 0);
        assert_eq!(driver.applied_count(), 3);
        assert!(driver.applied(0) && driver.applied(1) && driver.applied(2));
        assert_eq!(play.counts.iter().sum::<u32>(), 3);
        // ранняя нота на 100 мс — Good, поздняя на 127 мс — уже Miss
        assert_eq!(play.states[1].result, Judge::Good);
        assert_eq!(play.states[0].result, Judge::Miss);
    }

    #[test]
    fn replay_driver_rejects_a_note_count_mismatch() {
        let chart = chart_with(&[1.0, 2.0]);
        let blob = ReplayBlob {
            format: ReplayFormat::Dmr3,
            notes: vec![NoteShot { delta: 0, x: 0 }],
            left: vec![],
            right: vec![],
        };
        let err = ReplayDriver::new(&chart, &blob, "x").unwrap_err();
        assert!(err.contains("2"), "error mentions the chart size: {err}");
        // и заодно базовый случай, когда у чарта нет нот
        let empty = chart_with(&[]);
        let empty_blob = ReplayBlob {
            format: ReplayFormat::Dmr3,
            notes: vec![],
            left: vec![],
            right: vec![],
        };
        assert!(ReplayDriver::new(&empty, &empty_blob, "x").is_ok());
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn record_and_save() {
        let chart = Chart::empty();
        let play = Play::new(&chart);
        let mut r = Replay::new("testsong", "hard", "Test Song", &chart, 6.5, -26.0, 1.0, 0.75);
        r.mouse_motion(1.0, 3.0, 0.5);
        r.mouse_button(1.1, true, 0.5);
        r.action(1.1, 0, true, 0.5, Some(Judge::Perfect), -3.0);
        r.mouse_button(1.2, false, 0.5);
        let ev = RawTabletEvent {
            wall: Instant::now(),
            kind: "parblo",
            x_raw: 12345,
            pressure: 200,
            hover: false,
            press: true,
            x_norm: 0.6,
            raw: [0xa1, 0x00, 0x39, 0x30, 0, 0, 0xc8, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            raw_len: 10,
        };
        r.tablet(1.3, &ev);
        r.action(1.3, 1, true, 0.8, Some(Judge::Good), 20.0);
        let path = r.save(&play).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["meta"]["song"], "testsong");
        assert_eq!(doc["meta"]["diff"], "hard");
        assert_eq!(doc["meta"]["perfect_ms"], 50.0);
        assert_eq!(doc["meta"]["good_ms"], 120.0);
        assert_eq!(doc["meta"]["event_count"], 6);
        let events = doc["events"].as_array().unwrap();
        assert_eq!(events[0]["ev"], "motion");
        assert_eq!(events[0]["src"], "mouse");
        assert_eq!(events[2]["ev"], "tap");
        assert_eq!(events[2]["judge"], "Perfect");
        assert_eq!(events[4]["ev"], "report");
        assert_eq!(events[4]["src"], "parblo");
        assert_eq!(events[4]["x_raw"], 12345);
        assert_eq!(events[4]["raw"], "a10039300000c8000000");
        assert_eq!(events[5]["judge"], "Good");
        let _ = std::fs::remove_file(&path);
    }
}