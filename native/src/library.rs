//! Библиотека песен: data/library.json + сортировка/группировка (osu!lazer-style).

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct ChartRef {
    pub diff: String,
    pub path: String,
}

#[derive(Deserialize, Clone)]
pub struct Song {
    pub id: String,
    pub title: String,
    pub composer: String,
    pub album: String,
    #[serde(default)]
    pub levels: Vec<String>,
    pub charts: Vec<ChartRef>,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub audio: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub duration: f32,
}

/// Подписи корзин длительности, индекс = [`Song::length_bucket_index`].
const LENGTH_BUCKETS: [&str; 5] = ["< 1 min", "1–2 min", "2–3 min", "3–4 min", "4+ min"];

impl Song {
    /// Максимальный уровень сложности (число).
    pub fn max_level(&self) -> u32 {
        self.levels
            .iter()
            .filter_map(|l| l.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
    }
    /// Индекс корзины длительности: 0 — до минуты, 4 — четыре и больше.
    pub fn length_bucket_index(&self) -> usize {
        let d = self.duration;
        if d < 60.0 {
            0
        } else if d < 120.0 {
            1
        } else if d < 180.0 {
            2
        } else if d < 240.0 {
            3
        } else {
            4
        }
    }

    pub fn length_bucket(&self) -> &'static str {
        LENGTH_BUCKETS[self.length_bucket_index()]
    }
}

pub fn load() -> Result<Vec<Song>> {
    let text = std::fs::read_to_string("data/library.json").context("no data/library.json")?;
    let mut songs: Vec<Song> = serde_json::from_str(&text).context("library.json does not parse")?;
    // подмешиваем импортированные паки
    for m in crate::pack::installed() {
        songs.push(pack_to_song(&m));
    }
    Ok(songs)
}

/// Преобразовать манифест пака в песню (пути с префиксом data/imported/<id>/).
fn pack_to_song(m: &crate::pack::PackManifest) -> Song {
    let base = format!("{}/{}", crate::pack::IMPORT_DIR, m.id);
    let charts = m
        .charts
        .iter()
        .map(|c| ChartRef {
            diff: c.diff.clone(),
            path: format!("{base}/{}", c.path),
        })
        .collect();
    Song {
        id: m.id.clone(),
        title: m.title.clone(),
        composer: m.composer.clone(),
        album: if m.album.is_empty() { "Imported".into() } else { m.album.clone() },
        levels: m.levels.clone(),
        charts,
        cover: m.cover.as_ref().map(|p| format!("{base}/{p}")),
        audio: m.audio.as_ref().map(|p| format!("{base}/{p}")),
        preview: m.preview.as_ref().map(|p| format!("{base}/{p}")),
        duration: m.duration,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Title,
    Artist,
    Album,
    Length,
    Difficulty,
    Score,
}

impl SortBy {
    pub const ALL: [SortBy; 6] = [
        SortBy::Title,
        SortBy::Artist,
        SortBy::Album,
        SortBy::Length,
        SortBy::Difficulty,
        SortBy::Score,
    ];
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Title => "Title",
            SortBy::Artist => "Artist",
            SortBy::Album => "Album",
            SortBy::Length => "Length",
            SortBy::Difficulty => "Difficulty",
            SortBy::Score => "Record",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    None,
    Album,
    Artist,
    Length,
}

impl GroupBy {
    pub const ALL: [GroupBy; 4] = [GroupBy::None, GroupBy::Album, GroupBy::Artist, GroupBy::Length];
    pub fn label(self) -> &'static str {
        match self {
            GroupBy::None => "No groups",
            GroupBy::Album => "Album",
            GroupBy::Artist => "Artist",
            GroupBy::Length => "Length",
        }
    }
}

/// Отсортировать и сгруппировать песни. score_of — лучший счёт песни (для SortBy::Score).
pub fn sort_group(
    songs: &[Song],
    sort: SortBy,
    group: GroupBy,
    score_of: impl Fn(usize) -> u64,
) -> Vec<(String, Vec<usize>)> {
    let mut idx: Vec<usize> = (0..songs.len()).collect();
    idx.sort_by(|&a, &b| {
        let sa = &songs[a];
        let sb = &songs[b];
        let ord = match sort {
            SortBy::Title => sa.title.to_lowercase().cmp(&sb.title.to_lowercase()),
            SortBy::Artist => sa
                .composer
                .to_lowercase()
                .cmp(&sb.composer.to_lowercase())
                .then(sa.title.to_lowercase().cmp(&sb.title.to_lowercase())),
            SortBy::Album => sa
                .album
                .to_lowercase()
                .cmp(&sb.album.to_lowercase())
                .then(sa.title.to_lowercase().cmp(&sb.title.to_lowercase())),
            SortBy::Length => sa.duration.partial_cmp(&sb.duration).unwrap_or(std::cmp::Ordering::Equal),
            SortBy::Difficulty => sa.max_level().cmp(&sb.max_level()),
            SortBy::Score => score_of(b).cmp(&score_of(a)),
        };
        ord.then(a.cmp(&b))
    });

    if group == GroupBy::None {
        return vec![("All songs".to_string(), idx)];
    }
    let key_of = |i: usize| -> String {
        let s = &songs[i];
        match group {
            GroupBy::Album => s.album.clone(),
            GroupBy::Artist => {
                if s.composer.is_empty() { "Unknown".into() } else { s.composer.clone() }
            }
            GroupBy::Length => s.length_bucket().to_string(),
            GroupBy::None => String::new(),
        }
    };
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for i in idx {
        let k = key_of(i);
        if !map.contains_key(&k) {
            order.push(k.clone());
        }
        map.entry(k).or_default().push(i);
    }
    // для длины — упорядочить бакеты по возрастанию (по индексу, не по подписи)
    if group == GroupBy::Length {
        order.sort_by_key(|k| {
            LENGTH_BUCKETS
                .iter()
                .position(|b| b == k)
                .unwrap_or(LENGTH_BUCKETS.len() - 1)
        });
    }
    order
        .into_iter()
        .map(|k| {
            let v = map.remove(&k).unwrap();
            (k, v)
        })
        .collect()
}