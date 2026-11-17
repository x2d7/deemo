//! Загрузка чартов Deemo (формат: notes / links / speed).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
pub struct RawChart {
    #[serde(default)]
    pub speed: f32,
    #[serde(default)]
    pub notes: Vec<RawNote>,
    #[serde(default)]
    pub links: Vec<RawLink>,
}

#[derive(Deserialize)]
pub struct RawNote {
    #[serde(rename = "_time", default)]
    pub time: f32,
    #[serde(rename = "$id", default)]
    pub id: serde_json::Value,
    #[serde(default)]
    pub pos: Option<f32>,
    #[serde(default)]
    pub size: Option<f32>,
    /// скорость ноты (DEEMO II), по умолчанию 1
    #[serde(default)]
    pub speed: Option<f32>,
    /// длительность hold-ноты
    #[serde(default)]
    pub duration: Option<f32>,
    #[serde(default)]
    pub swipe: Option<bool>,
    #[serde(default)]
    pub sounds: Option<Vec<RawSound>>,
}

#[derive(Deserialize, Clone, Copy)]
pub struct RawSound {
    #[serde(default)]
    pub d: f32,
    #[serde(default)]
    pub p: f32,
    #[serde(default)]
    pub v: f32,
    #[serde(default)]
    pub w: f32,
}

#[derive(Deserialize)]
pub struct RawLink {
    #[serde(default)]
    pub notes: Vec<RawRef>,
}

#[derive(Deserialize)]
pub struct RawRef {
    #[serde(rename = "$ref", default)]
    pub id: serde_json::Value,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoteKind {
    Normal,
    Slide,
    Swipe,
    /// нота без keysound (отдельный спрайт, судится как обычная)
    NoSound,
}

#[derive(Clone)]
pub struct Note {
    pub time: f32,
    pub pos: f32,
    pub size: f32,
    pub speed: f32,
    pub kind: NoteKind,
    pub hold: f32,
    pub sounds: Vec<RawSound>,
}

/// Связка слайд-нот: индексы в `Chart::notes` по порядку.
pub struct Link {
    pub notes: Vec<usize>,
}

pub struct Chart {
    pub speed: f32,
    pub notes: Vec<Note>,
    pub links: Vec<Link>,
}

fn id_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl Chart {
    pub fn empty() -> Chart {
        Chart {
            speed: 6.0,
            notes: Vec::new(),
            links: Vec::new(),
        }
    }

    pub fn load(path: &str) -> Result<Chart> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("chart does not read {path}"))?;
        let raw: RawChart = serde_json::from_str(&text)
            .with_context(|| format!("chart does not parse {path}"))?;

        // в какой связке состоит нота
        let mut link_of: HashMap<String, usize> = HashMap::new();
        for (li, link) in raw.links.iter().enumerate() {
            for r in &link.notes {
                link_of.insert(id_str(&r.id), li);
            }
        }

        // (id, нота) — потом сортируем по времени
        let mut raw_notes: Vec<(String, Note)> = Vec::with_capacity(raw.notes.len());
        for n in &raw.notes {
            // В JSON Deemo отсутствие pos — это не «нота без позиции», а
            // позиция 0 (центр): и Unity JsonUtility, и Newtonsoft оставляют
            // у отсутствующего float значение по умолчанию. Раньше такие
            // ноты выкидывались: Altale Hard терял 139 нот (985 вместо 1124),
            // Entrance Hard — 88.
            let pos = n.pos.unwrap_or(0.0);
            // ноты вне сцены — это фоновые звуки
            if pos.abs() > 2.0001 {
                continue;
            }
            let id = id_str(&n.id);
            let has_sound = n.sounds.as_ref().map(|s| !s.is_empty()).unwrap_or(false);
            let kind = if n.swipe.unwrap_or(false) {
                NoteKind::Swipe
            } else if link_of.contains_key(&id) {
                NoteKind::Slide
            } else if has_sound {
                NoteKind::Normal
            } else {
                NoteKind::NoSound
            };
            raw_notes.push((
                id,
                Note {
                    time: n.time,
                    pos,
                    size: n.size.unwrap_or(1.0),
                    speed: n.speed.unwrap_or(1.0),
                    kind,
                    hold: n.duration.unwrap_or(0.0),
                    sounds: n.sounds.clone().unwrap_or_default(),
                },
            ));
        }
        raw_notes.sort_by(|a, b| a.1.time.partial_cmp(&b.1.time).unwrap());

        let mut index_of: HashMap<String, usize> = HashMap::with_capacity(raw_notes.len());
        let mut notes = Vec::with_capacity(raw_notes.len());
        for (i, (id, note)) in raw_notes.into_iter().enumerate() {
            index_of.insert(id, i);
            notes.push(note);
        }

        // связки: id → индексы в уже отсортированном массиве
        let mut links: Vec<Link> = Vec::with_capacity(raw.links.len());
        for link in &raw.links {
            let mut idx = Vec::with_capacity(link.notes.len());
            for r in &link.notes {
                if let Some(&i) = index_of.get(&id_str(&r.id)) {
                    idx.push(i);
                }
            }
            if idx.len() >= 2 {
                links.push(Link { notes: idx });
            }
        }

        Ok(Chart {
            speed: if raw.speed > 0.0 { raw.speed } else { 6.0 },
            notes,
            links,
        })
    }

    /// Время последней сыгранной ноты (для вспышки судейской линии).
    pub fn previous_hit_time(&self, t: f32) -> Option<f32> {
        let mut hit = None;
        for n in &self.notes {
            if n.time <= t {
                hit = Some(n.time);
            } else {
                break;
            }
        }
        hit
    }
}
