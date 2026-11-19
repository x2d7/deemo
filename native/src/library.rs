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

    /// Подпись уровня сложности из `levels` (сырой строкой: бывает `01001100`).
    pub fn chart_level(&self, diff: usize) -> &str {
        self.levels.get(diff).map(String::as_str).unwrap_or("")
    }

    /// Имя сложности чарта (`easy`, `hard`, …) или пустая строка.
    pub fn chart_diff(&self, diff: usize) -> &str {
        self.charts.get(diff).map(|c| c.diff.as_str()).unwrap_or("")
    }
}

/// Одна строка библиотеки: конкретная сложность конкретной песни.
///
/// Раньше список был «песня + чипы сложностей»; теперь это плоский список
/// чартов (osu!-стиль), поэтому выбор и все данные правой панели — это пара
/// `(song, diff)`, а не только песня.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Entry {
    pub song: usize,
    pub diff: usize,
}

impl Entry {
    pub fn new(song: usize, diff: usize) -> Entry {
        Entry { song, diff }
    }
}

/// Все чарты библиотеки по одной строке на сложность.
pub fn entries(songs: &[Song]) -> Vec<Entry> {
    let mut out = Vec::new();
    for (si, s) in songs.iter().enumerate() {
        for d in 0..s.charts.len() {
            out.push(Entry::new(si, d));
        }
    }
    out
}

/// Рейтинг строки: звезда сервера, иначе числовой уровень из `levels`.
/// Нужен и подписям, и сортировке, и звёздному фильтру.
pub fn entry_rating(songs: &[Song], e: Entry, stars: &crate::stars::StarMap) -> f32 {
    let Some(song) = songs.get(e.song) else { return 0.0 };
    if let Some(star) = crate::stars::star(stars, &song.id, song.chart_diff(e.diff)) {
        return star;
    }
    song.chart_level(e.diff).parse::<f32>().unwrap_or(0.0)
}

/// Подпись сложности в строке: `★ 9.6`, если звезда известна, иначе уровень
/// (`9`). Формат звезды — одна десятка, без префикса «Lv».
pub fn entry_rating_label(songs: &[Song], e: Entry, stars: &crate::stars::StarMap) -> String {
    let Some(song) = songs.get(e.song) else { return String::new() };
    match crate::stars::star(stars, &song.id, song.chart_diff(e.diff)) {
        Some(star) => crate::stars::label(star),
        None => song.chart_level(e.diff).to_string(),
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
    /// Звёзды по возрастанию (уровень — фолбэк).
    Difficulty,
    /// Звёзды по убыванию.
    DifficultyDesc,
    Score,
}

impl SortBy {
    pub const ALL: [SortBy; 7] = [
        SortBy::Title,
        SortBy::Artist,
        SortBy::Album,
        SortBy::Length,
        SortBy::Difficulty,
        SortBy::DifficultyDesc,
        SortBy::Score,
    ];
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Title => "Title",
            SortBy::Artist => "Artist",
            SortBy::Album => "Album",
            SortBy::Length => "Length",
            SortBy::Difficulty => "Difficulty low",
            SortBy::DifficultyDesc => "Difficulty high",
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

/// Отсортировать и сгруппировать чарты. `score_of` — лучший счёт конкретной
/// сложности (для SortBy::Score), `stars` — кэш звёзд (или пустой).
///
/// Внутри одной песни сложности всегда идут по возрастанию рейтинга: так
/// подпись `Title A–Z` остаётся алфавитной, а строки одной песни читаются
/// сверху вниз от лёгкой к тяжёлой.
pub fn sort_group(
    songs: &[Song],
    sort: SortBy,
    group: GroupBy,
    stars: &crate::stars::StarMap,
    score_of: impl Fn(Entry) -> u64,
) -> Vec<(String, Vec<Entry>)> {
    let rating = |e: Entry| entry_rating(songs, e, stars);
    let mut idx = entries(songs);
    idx.sort_by(|&a, &b| {
        let sa = &songs[a.song];
        let sb = &songs[b.song];
        let ra = rating(a);
        let rb = rating(b);
        let by_entry = || {
            sa.title
                .to_lowercase()
                .cmp(&sb.title.to_lowercase())
                .then(ra.partial_cmp(&rb).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.song.cmp(&b.song))
                .then(a.diff.cmp(&b.diff))
        };
        let ord = match sort {
            SortBy::Title => by_entry(),
            SortBy::Artist => sa
                .composer
                .to_lowercase()
                .cmp(&sb.composer.to_lowercase())
                .then(by_entry()),
            SortBy::Album => sa
                .album
                .to_lowercase()
                .cmp(&sb.album.to_lowercase())
                .then(by_entry()),
            SortBy::Length => sa
                .duration
                .partial_cmp(&sb.duration)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(by_entry()),
            SortBy::Difficulty => ra
                .partial_cmp(&rb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(by_entry()),
            SortBy::DifficultyDesc => rb
                .partial_cmp(&ra)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(by_entry()),
            SortBy::Score => score_of(b).cmp(&score_of(a)).then(by_entry()),
        };
        ord.then(a.song.cmp(&b.song)).then(a.diff.cmp(&b.diff))
    });

    if group == GroupBy::None {
        return vec![("All songs".to_string(), idx)];
    }
    let key_of = |e: Entry| -> String {
        let s = &songs[e.song];
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
    let mut map: std::collections::HashMap<String, Vec<Entry>> = std::collections::HashMap::new();
    for e in idx {
        let k = key_of(e);
        if !map.contains_key(&k) {
            order.push(k.clone());
        }
        map.entry(k).or_default().push(e);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stars::StarMap;

    fn song(id: &str, title: &str, composer: &str, album: &str, levels: &[&str]) -> Song {
        Song {
            id: id.into(),
            title: title.into(),
            composer: composer.into(),
            album: album.into(),
            levels: levels.iter().map(|s| s.to_string()).collect(),
            charts: levels
                .iter()
                .enumerate()
                .map(|(i, _)| ChartRef {
                    diff: ["easy", "normal", "hard"][i.min(2)].to_string(),
                    path: String::new(),
                })
                .collect(),
            cover: None,
            audio: None,
            preview: None,
            duration: 100.0,
        }
    }

    fn star_map(pairs: &[(&str, &str, f32)]) -> StarMap {
        pairs
            .iter()
            .map(|(song, diff, s)| (crate::stars::key(song, diff), *s))
            .collect()
    }

    /// Каждая сложность песни — отдельная строка: никаких «песня + чипы».
    #[test]
    fn entries_are_one_row_per_chart() {
        let songs = vec![
            song("a", "Alpha", "C", "A", &["3", "6", "9"]),
            song("b", "Beta", "C", "A", &["1", "4"]),
        ];
        let e = entries(&songs);
        assert_eq!(e.len(), 5);
        assert_eq!(e[0], Entry::new(0, 0));
        assert_eq!(e[2], Entry::new(0, 2));
        assert_eq!(e[3], Entry::new(1, 0));
        assert_eq!(e[4], Entry::new(1, 1));
    }

    /// Difficulty low/high сортирует именно строки по звезде (уровень —
    /// фолбэк), а не песни по максимальному уровню.
    #[test]
    fn difficulty_sort_is_per_chart_by_star() {
        let songs = vec![
            song("a", "Alpha", "C", "A", &["9", "9"]),
            song("b", "Beta", "C", "A", &["2", "3"]),
        ];
        let stars = star_map(&[("a", "easy", 9.8), ("b", "easy", 1.2), ("b", "normal", 8.4)]);
        let groups = sort_group(&songs, SortBy::Difficulty, GroupBy::None, &stars, |_| 0);
        assert_eq!(
            groups[0].1,
            vec![Entry::new(1, 0), Entry::new(1, 1), Entry::new(0, 1), Entry::new(0, 0)],
            "1.2 < 8.4 < 9.0 (level fallback) < 9.8"
        );
        let groups = sort_group(&songs, SortBy::DifficultyDesc, GroupBy::None, &stars, |_| 0);
        assert_eq!(groups[0].1[0], Entry::new(0, 0), "9.8 first");
        assert_eq!(groups[0].1.last(), Some(&Entry::new(1, 0)));
    }

    /// Title A–Z: сначала название, внутри песни — по возрастанию звезды.
    #[test]
    fn title_sort_puts_charts_of_a_song_light_to_heavy() {
        let songs = vec![
            song("b", "Beta", "C", "A", &["7", "1"]),
            song("a", "Alpha", "C", "A", &["5"]),
        ];
        let stars = star_map(&[("b", "easy", 7.7), ("b", "normal", 1.1)]);
        let groups = sort_group(&songs, SortBy::Title, GroupBy::None, &stars, |_| 0);
        assert_eq!(
            groups[0].1,
            vec![Entry::new(1, 0), Entry::new(0, 1), Entry::new(0, 0)],
            "Alpha, then Beta normal (1.1) before Beta easy (7.7)"
        );
    }

    /// Поиск оставляет все сложности найденной песни; звёздный фильтр
    /// режет строки по отдельности.
    #[test]
    fn filters_apply_per_chart() {
        let songs = vec![
            song("a", "Alpha", "Composer", "Album", &["2", "4", "8"]),
            song("b", "Beta", "Other", "Album", &["3"]),
        ];
        let stars = star_map(&[("a", "easy", 2.0), ("a", "normal", 4.0), ("a", "hard", 8.0)]);
        let groups = sort_group(&songs, SortBy::Title, GroupBy::None, &stars, |_| 0);
        let found = filter_entries(groups.clone(), &songs, "alpha", &stars, None);
        assert_eq!(found[0].1.len(), 3, "search keeps every chart of the song");
        let filtered =
            filter_entries(groups, &songs, "", &stars, Some((5.0, 10.0)));
        assert_eq!(
            filtered[0].1,
            vec![Entry::new(0, 2)],
            "only the hard chart survives the star range"
        );
    }

    /// Нет звезды — сортировка/подпись берут уровень из `levels`.
    #[test]
    fn missing_stars_fall_back_to_levels() {
        let songs = vec![song("a", "Alpha", "C", "A", &["7", "9"])];
        let stars = StarMap::new();
        assert_eq!(entry_rating(&songs, Entry::new(0, 0), &stars), 7.0);
        assert_eq!(entry_rating_label(&songs, Entry::new(0, 1), &stars), "9");
        let known = star_map(&[("a", "normal", 9.6)]);
        assert_eq!(entry_rating_label(&songs, Entry::new(0, 1), &known), "★ 9.6");
    }
}

/// Фильтр по строкам: поиск по песне и (опционально) диапазон звёзд.
///
/// Звёздный диапазон применяется к каждой строке отдельно, поэтому у песни
/// может остаться hard и исчезнуть easy. Поиск по-прежнему совпадает с
/// песней и оставляет все её сложности.
pub fn filter_entries(
    groups: Vec<(String, Vec<Entry>)>,
    songs: &[Song],
    query: &str,
    stars: &crate::stars::StarMap,
    star_range: Option<(f32, f32)>,
) -> Vec<(String, Vec<Entry>)> {
    let q = query.trim().to_lowercase();
    if q.is_empty() && star_range.is_none() {
        return groups;
    }
    groups
        .into_iter()
        .filter_map(|(name, entries)| {
            let hit: Vec<Entry> = entries
                .into_iter()
                .filter(|&e| {
                    let Some(s) = songs.get(e.song) else { return false };
                    let song_hit = q.is_empty()
                        || s.title.to_lowercase().contains(&q)
                        || s.composer.to_lowercase().contains(&q)
                        || s.album.to_lowercase().contains(&q);
                    if !song_hit {
                        return false;
                    }
                    match star_range {
                        Some((min, max)) => {
                            let r = entry_rating(songs, e, stars);
                            r >= min && r <= max
                        }
                        None => true,
                    }
                })
                .collect();
            if hit.is_empty() { None } else { Some((name, hit)) }
        })
        .collect()
}