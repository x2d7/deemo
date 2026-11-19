//! Минимальный разбор HID report descriptor: абсолютные поля пера.
//!
//! На Windows планшет может быть виден не только как vendor-коллекция с
//! байтами 0xA0/0xA1, но и как стандартная HID-коллекция digitizer
//! (usage page 0x0D). Раскладка её репорта задана дескриптором, поэтому
//! смещения X/Y/давления/кончика берём из дескриптора, а не угадываем.
//!
//! Модуль чистый (никаких Win32-вызовов): его гоняют тесты на синтетических
//! дескрипторах без железа. Windows-бэкенд только кормит сюда байты.

// На Linux модуль нужен только тестам — не засоряем сборку предупреждениями.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::BTreeMap;

/// Поле внутри репорта: смещение в битах от начала данных (после Report ID).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub bit_offset: u32,
    pub bit_size: u8,
    pub logical_min: i32,
    pub logical_max: i32,
    pub usage_page: u16,
    pub usage: u16,
}

impl Field {
    pub fn end_bit(&self) -> u32 {
        self.bit_offset + self.bit_size as u32
    }

    /// Прочитать значение поля из данных репорта (после Report ID).
    ///
    /// HID пакует поля младшим битом вперёд, значение — little-endian по
    /// битам; отрицательные logical minimum расширяются знаком.
    pub fn read(&self, data: &[u8]) -> Option<i32> {
        let size = self.bit_size as usize;
        if size == 0 || size > 32 {
            return None;
        }
        let start = self.bit_offset as usize;
        if start.checked_add(size)? > data.len().checked_mul(8)? {
            return None;
        }
        let mut v: u32 = 0;
        for i in 0..size {
            let bit = start + i;
            let b = (data[bit / 8] >> (bit % 8)) & 1;
            v |= (b as u32) << i;
        }
        if self.logical_min < 0 && size < 32 && (v & (1u32 << (size - 1))) != 0 {
            v |= u32::MAX << size;
        }
        Some(v as i32)
    }

    /// Нормализовать значение в 0..1 по logical range.
    pub fn norm(&self, value: i32) -> Option<f32> {
        let span = i64::from(self.logical_max) - i64::from(self.logical_min);
        if span <= 0 {
            return None;
        }
        let rel = i64::from(value) - i64::from(self.logical_min);
        Some((rel as f32 / span as f32).clamp(0.0, 1.0))
    }

    /// Пересчитать значение в единицы `0..units_max` (давление игры 0..8191).
    pub fn units(&self, value: i32, units_max: i32) -> i32 {
        let span = i64::from(self.logical_max) - i64::from(self.logical_min);
        if span <= 0 || units_max <= 0 {
            return 0;
        }
        let rel = (i64::from(value) - i64::from(self.logical_min)).clamp(0, span);
        ((rel * i64::from(units_max)) / span) as i32
    }

    /// Краткое описание для `tablet_probe.log`.
    pub fn describe(&self) -> String {
        format!(
            "bit {}+{} logical {}..{} page 0x{:04x}/usage 0x{:02x}",
            self.bit_offset,
            self.bit_size,
            self.logical_min,
            self.logical_max,
            self.usage_page,
            self.usage
        )
    }
}

/// Один input-репорт digitizer-коллекции.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigitizerReport {
    pub report_id: Option<u8>,
    pub x: Field,
    pub y: Option<Field>,
    pub pressure: Option<Field>,
    pub tip: Option<Field>,
    pub in_range: Option<Field>,
}

impl DigitizerReport {
    /// Минимальная длина (в байтах) репорта с учётом Report ID.
    pub fn min_report_len(&self) -> usize {
        let mut bits = self.x.end_bit();
        for f in [self.y, self.pressure, self.tip, self.in_range].into_iter().flatten() {
            bits = bits.max(f.end_bit());
        }
        bits.div_ceil(8) as usize + usize::from(self.report_id.is_some())
    }

    pub fn describe(&self) -> String {
        let mut s = match self.report_id {
            Some(id) => format!("report 0x{id:02x}: x {}", self.x.describe()),
            None => format!("report without id: x {}", self.x.describe()),
        };
        for (name, f) in [
            ("y", self.y),
            ("pressure", self.pressure),
            ("tip", self.tip),
            ("in_range", self.in_range),
        ] {
            if let Some(f) = f {
                s.push_str(&format!("; {name} {}", f.describe()));
            }
        }
        s.push_str(&format!("; >= {} bytes", self.min_report_len()));
        s
    }
}

/// Раскладка всех digitizer-репортов устройства.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigitizerLayout {
    pub reports: Vec<DigitizerReport>,
}

/// Разобранный отчёт пера из стандартной HID-коллекции.
#[derive(Clone, Copy, Debug)]
pub struct PenReading {
    pub report_id: Option<u8>,
    pub x_raw: i32,
    pub y_raw: Option<i32>,
    /// Давление в единицах игры 0..8191.
    pub pressure: i32,
    pub tip: bool,
    pub in_range: bool,
    /// X, нормализованный по logical range в 0..1.
    pub x_norm: f32,
}

impl DigitizerLayout {
    /// Разобрать report descriptor. Возвращает только репорты, где есть
    /// абсолютный X и признак контакта/давления; остальные (мышь, кнопки)
    /// неинтересны.
    pub fn parse(desc: &[u8]) -> Option<DigitizerLayout> {
        let mut parser = Parser::default();
        parser.run(desc);
        let mut reports = Vec::new();
        for (report_id, fields) in parser.inputs {
            let x = fields
                .iter()
                .find(|f| f.usage_page == 0x01 && f.usage == 0x30)
                .copied();
            let Some(x) = x else { continue };
            let y = fields
                .iter()
                .find(|f| f.usage_page == 0x01 && f.usage == 0x31)
                .copied();
            let pressure = fields
                .iter()
                .find(|f| f.usage_page == 0x0d && f.usage == 0x30)
                .copied();
            let tip = fields
                .iter()
                .find(|f| f.usage_page == 0x0d && f.usage == 0x42)
                .copied();
            let in_range = fields
                .iter()
                .find(|f| f.usage_page == 0x0d && f.usage == 0x32)
                .copied();
            if tip.is_none() && pressure.is_none() && in_range.is_none() {
                continue;
            }
            reports.push(DigitizerReport {
                report_id,
                x,
                y,
                pressure,
                tip,
                in_range,
            });
        }
        if reports.is_empty() {
            None
        } else {
            Some(DigitizerLayout { reports })
        }
    }

    /// Декодировать входной репорт. `buf` — как отдал hidapi: с Report ID
    /// первым байтом, если устройство использует report ID.
    pub fn decode(&self, buf: &[u8]) -> Option<PenReading> {
        // Сначала точное совпадение report ID, потом репорт без ID.
        for with_id in [true, false] {
            for r in &self.reports {
                let data = match (r.report_id, with_id) {
                    (Some(id), true) if buf.first() == Some(&id) => &buf[1..],
                    (None, false) => buf,
                    _ => continue,
                };
                let Some(x_raw) = r.x.read(data) else { continue };
                let Some(x_norm) = r.x.norm(x_raw) else { continue };
                let y_raw = r.y.and_then(|f| f.read(data));
                let pressure = r
                    .pressure
                    .and_then(|f| f.read(data))
                    .map(|v| r.pressure.map(|f| f.units(v, 8191)).unwrap_or(0))
                    .unwrap_or(0);
                let tip = r
                    .tip
                    .and_then(|f| f.read(data))
                    .map(|v| v != 0)
                    .unwrap_or(pressure > 0);
                let in_range = r
                    .in_range
                    .and_then(|f| f.read(data))
                    .map(|v| v != 0)
                    .unwrap_or(true);
                return Some(PenReading {
                    report_id: r.report_id,
                    x_raw,
                    y_raw,
                    pressure,
                    tip,
                    in_range,
                    x_norm,
                });
            }
        }
        None
    }

    /// Однострочная расшифровка для лога/пробы.
    pub fn describe(&self) -> String {
        self.reports
            .iter()
            .map(DigitizerReport::describe)
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

#[derive(Clone, Copy, Debug)]
struct Globals {
    usage_page: u16,
    report_size: u32,
    report_count: u32,
    logical_min: i32,
    logical_max: i32,
    report_id: Option<u8>,
}

impl Default for Globals {
    fn default() -> Self {
        Globals {
            usage_page: 0,
            report_size: 0,
            report_count: 0,
            logical_min: 0,
            logical_max: 0,
            report_id: None,
        }
    }
}

#[derive(Default)]
struct Parser {
    inputs: BTreeMap<Option<u8>, Vec<Field>>,
    input_bits: BTreeMap<Option<u8>, u32>,
}

impl Parser {
    fn run(&mut self, desc: &[u8]) {
        let mut g = Globals::default();
        let mut stack: Vec<Globals> = Vec::new();
        let mut cursor: u32 = 0;
        let mut usages: Vec<(u16, u16)> = Vec::new();
        let mut usage_min: Option<(u16, u16)> = None;
        let mut usage_max: Option<(u16, u16)> = None;
        let mut i = 0usize;
        while i < desc.len() {
            let prefix = desc[i];
            i += 1;
            if prefix == 0xfe {
                // long item: size, tag, data
                if i + 2 > desc.len() {
                    break;
                }
                let size = desc[i] as usize;
                i += 2;
                i = (i + size).min(desc.len());
                continue;
            }
            let size = match prefix & 0x03 {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => 4,
            };
            if i + size > desc.len() {
                break;
            }
            let data = &desc[i..i + size];
            i += size;
            let uval = unsigned(data);
            let sval = signed(data);
            let typ = (prefix >> 2) & 0x03;
            let tag = (prefix >> 4) & 0x0f;
            match typ {
                0 => match tag {
                    0x8 => {
                        // Input
                        let flags = uval;
                        let constant = flags & 0x01 != 0;
                        let variable = flags & 0x02 != 0;
                        if !constant && variable {
                            let count = g.report_count;
                            for k in 0..count {
                                let pair = usages
                                    .get(k as usize)
                                    .copied()
                                    .or_else(|| usage_min.map(|(p, u)| (p, u.wrapping_add(k as u16))));
                                if let Some((page, usage)) = pair {
                                    if usage != 0 {
                                        self.inputs.entry(g.report_id).or_default().push(Field {
                                            bit_offset: cursor,
                                            bit_size: g.report_size.min(32) as u8,
                                            logical_min: g.logical_min,
                                            logical_max: g.logical_max,
                                            usage_page: page,
                                            usage,
                                        });
                                    }
                                }
                                cursor = cursor.saturating_add(g.report_size);
                            }
                        } else {
                            cursor = cursor.saturating_add(g.report_size.saturating_mul(g.report_count));
                        }
                        self.input_bits
                            .entry(g.report_id)
                            .and_modify(|b| *b = (*b).max(cursor))
                            .or_insert(cursor);
                        clear_locals(&mut usages, &mut usage_min, &mut usage_max);
                    }
                    0xa | 0xc => {
                        // Collection / End Collection: локальные usage сбрасываются
                        clear_locals(&mut usages, &mut usage_min, &mut usage_max);
                    }
                    _ => {
                        // Output/Feature живут в своих пространствах, курсор
                        // input-репорта не двигают.
                        clear_locals(&mut usages, &mut usage_min, &mut usage_max);
                    }
                },
                1 => match tag {
                    0x0 => g.usage_page = uval as u16,
                    0x1 => g.logical_min = sval,
                    0x2 => {
                        // Логический максимум по спецификации знаковый, но
                        // 8-битные устройства часто пишут 0xFF = 255.
                        g.logical_max = if sval < g.logical_min {
                            uval as i32
                        } else {
                            sval
                        };
                    }
                    0x7 => g.report_size = uval,
                    0x8 => {
                        g.report_id = match uval {
                            0 => None,
                            v => Some(v as u8),
                        };
                        // Каждый report ID начинает свою раскладку.
                        cursor = 0;
                    }
                    0x9 => g.report_count = uval,
                    0xa => stack.push(g),
                    0xb => {
                        if let Some(prev) = stack.pop() {
                            g = prev;
                        }
                    }
                    _ => {}
                },
                2 => match tag {
                    0x0 => usages.push(usage_pair(uval, g.usage_page)),
                    0x1 => usage_min = Some(usage_pair(uval, g.usage_page)),
                    0x2 => usage_max = Some(usage_pair(uval, g.usage_page)),
                    _ => {}
                },
                _ => {}
            }
            // usage_max пока нигде не нужен: диапазон строится из usage_min.
            let _ = usage_max;
        }
    }
}

fn clear_locals(
    usages: &mut Vec<(u16, u16)>,
    usage_min: &mut Option<(u16, u16)>,
    usage_max: &mut Option<(u16, u16)>,
) {
    usages.clear();
    *usage_min = None;
    *usage_max = None;
}

/// Значение local usage: 4 байта — page+usage, 2 байта — page в старшем байте.
fn usage_pair(v: u32, page: u16) -> (u16, u16) {
    if v > 0xffff {
        ((v >> 16) as u16, v as u16)
    } else if v > 0xff {
        ((v >> 8) as u16, (v & 0xff) as u16)
    } else {
        (page, v as u16)
    }
}

/// Оценка длин input-репортов из дескриптора: `(report_id, байты)`.
/// Нужна логу пробы, когда устройство ещё не присылало ни одного репорта.
pub fn input_report_lengths(desc: &[u8]) -> Vec<(Option<u8>, usize)> {
    let mut p = Parser::default();
    p.run(desc);
    p.input_bits
        .into_iter()
        .map(|(id, bits)| {
            (
                id,
                (bits as usize).div_ceil(8) + usize::from(id.is_some()),
            )
        })
        .collect()
}

fn unsigned(data: &[u8]) -> u32 {
    data.iter()
        .enumerate()
        .fold(0u32, |acc, (i, b)| acc | ((*b as u32) << (8 * i)))
}

fn signed(data: &[u8]) -> i32 {
    match data.len() {
        0 => 0,
        1 => data[0] as i8 as i32,
        2 => i16::from_le_bytes([data[0], data[1]]) as i32,
        3 => {
            let v = unsigned(data);
            if v & 0x0080_0000 != 0 {
                (v | 0xff00_0000) as i32
            } else {
                v as i32
            }
        }
        4 => unsigned(data) as i32,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{DigitizerLayout, Field};

    /// X 16 бит, Y 16 бит, tip 1, in_range 1, pressure 10.
    fn desc_digitizer(report_id: Option<u8>) -> Vec<u8> {
        let mut d = Vec::new();
        if let Some(id) = report_id {
            d.extend_from_slice(&[0x85, id]);
        }
        d.extend_from_slice(&[
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x30, // Usage (X)
            0x15, 0x00, // Logical Min 0
            0x26, 0xff, 0x7f, // Logical Max 32767
            0x75, 0x10, // Report Size 16
            0x95, 0x01, // Report Count 1
            0x81, 0x02, // Input (Data,Var,Abs)
            0x09, 0x31, // Usage (Y)
            0x81, 0x02, // Input
            0x05, 0x0d, // Usage Page (Digitizer)
            0x09, 0x42, // Usage (Tip Switch)
            0x15, 0x00, // Logical Min 0
            0x25, 0x01, // Logical Max 1
            0x75, 0x01, // Report Size 1
            0x95, 0x01, // Report Count 1
            0x81, 0x02, // Input
            0x09, 0x32, // Usage (In Range)
            0x81, 0x02, // Input
            0x09, 0x30, // Usage (Tip Pressure)
            0x26, 0xff, 0x07, // Logical Max 2047
            0x75, 0x0a, // Report Size 10
            0x95, 0x01, // Report Count 1
            0x81, 0x02, // Input
        ]);
        d
    }

    /// X=1000, Y=2000, tip=1, in_range=1, pressure=1023.
    fn payload() -> [u8; 6] {
        // x 0x03e8, y 0x07d0, tip bit32=1, in_range bit33=1,
        // pressure 1023 (10 бит) с бита 34.
        [0xe8, 0x03, 0xd0, 0x07, 0xff, 0x0f]
    }

    #[test]
    fn parses_standard_digitizer_without_report_id() {
        let layout = DigitizerLayout::parse(&desc_digitizer(None)).expect("layout");
        assert_eq!(layout.reports.len(), 1);
        let r = layout.reports[0];
        assert_eq!(r.report_id, None);
        assert_eq!((r.x.bit_offset, r.x.bit_size), (0, 16));
        assert_eq!((r.y.unwrap().bit_offset, r.y.unwrap().bit_size), (16, 16));
        assert_eq!((r.tip.unwrap().bit_offset, r.tip.unwrap().bit_size), (32, 1));
        assert_eq!(
            (r.in_range.unwrap().bit_offset, r.in_range.unwrap().bit_size),
            (33, 1)
        );
        assert_eq!(
            (r.pressure.unwrap().bit_offset, r.pressure.unwrap().bit_size),
            (34, 10)
        );
        assert_eq!(r.min_report_len(), 6);

        let p = layout.decode(&payload()).expect("reading");
        assert_eq!(p.x_raw, 1000);
        assert_eq!(p.y_raw, Some(2000));
        // 1023 из 2047 пересчитаны в игровые 0..8191
        assert_eq!(p.pressure, 4093);
        assert!(p.tip);
        assert!(p.in_range);
        assert!((p.x_norm - 1000.0 / 32767.0).abs() < 1e-6);
    }

    #[test]
    fn parses_digitizer_with_report_id() {
        let layout = DigitizerLayout::parse(&desc_digitizer(Some(3))).expect("layout");
        let r = layout.reports[0];
        assert_eq!(r.report_id, Some(3));
        assert_eq!(r.min_report_len(), 7);

        let mut buf = vec![3u8];
        buf.extend_from_slice(&payload());
        let p = layout.decode(&buf).expect("reading");
        assert_eq!(p.x_raw, 1000);
        assert_eq!(p.report_id, Some(3));
        // чужой report id — не наш репорт
        assert!(layout.decode(&[4, 0, 0, 0, 0, 0, 0]).is_none());
    }

    #[test]
    fn mouse_descriptor_without_digitizer_usages_is_ignored() {
        let mouse = [
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01,
            0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01,
            0x75, 0x05, 0x81, 0x03, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f,
            0x75, 0x08, 0x95, 0x02, 0x81, 0x06, 0xc0, 0xc0,
        ];
        assert!(DigitizerLayout::parse(&mouse).is_none());
    }

    #[test]
    fn truncated_or_empty_descriptor_does_not_panic() {
        assert!(DigitizerLayout::parse(&[]).is_none());
        for cut in 1..desc_digitizer(None).len() {
            let d = desc_digitizer(None);
            let _ = DigitizerLayout::parse(&d[..cut]);
        }
    }

    #[test]
    fn input_report_lengths_come_from_the_descriptor() {
        assert_eq!(
            super::input_report_lengths(&desc_digitizer(None)),
            vec![(None, 6)]
        );
        assert_eq!(
            super::input_report_lengths(&desc_digitizer(Some(3))),
            vec![(Some(3), 7)]
        );
        // мышиный дескриптор: 3 бита кнопок + 5 бит паддинга + 2 байта X/Y = 3 байта
        let mouse = [
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01,
            0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01,
            0x75, 0x05, 0x81, 0x03, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f,
            0x75, 0x08, 0x95, 0x02, 0x81, 0x06, 0xc0, 0xc0,
        ];
        assert_eq!(super::input_report_lengths(&mouse), vec![(None, 3)]);
        assert!(super::input_report_lengths(&[]).is_empty());
    }

    /// hidapi на Windows реконструирует дескриптор из preparsed data и может
    /// свернуть все поля в один широкий item (у Gaomon — 88 бит). Длина
    /// репорта должна остаться 11+1 байт, а не обрезаться.
    #[test]
    fn reconstructed_wide_item_keeps_the_report_length() {
        let gaomon = [
            0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x08, 0x09, 0x01, 0x15, 0x00, 0x25,
            0x00, 0x75, 0x58, 0x95, 0x01, 0x81, 0x02, 0xc0,
        ];
        assert_eq!(
            super::input_report_lengths(&gaomon),
            vec![(Some(8), 12)]
        );
    }

    #[test]
    fn field_read_sign_extends_and_bounds_checks() {
        let f = Field {
            bit_offset: 0,
            bit_size: 8,
            logical_min: -128,
            logical_max: 127,
            usage_page: 1,
            usage: 0x30,
        };
        assert_eq!(f.read(&[0x7f]), Some(127));
        assert_eq!(f.read(&[0xff]), Some(-1));
        // репорт короче поля — None, а не мусор
        assert_eq!(f.read(&[]), None);
        let g = Field {
            bit_offset: 4,
            bit_size: 16,
            ..f
        };
        assert_eq!(g.read(&[0x00, 0x10, 0x00]), Some(0x0100));
        assert_eq!(g.read(&[0x00, 0x10]), None);
    }

    #[test]
    fn pressure_scales_to_game_units() {
        let field = Field {
            bit_offset: 0,
            bit_size: 10,
            logical_min: 0,
            logical_max: 2047,
            usage_page: 0x0d,
            usage: 0x30,
        };
        assert_eq!(field.units(0, 8191), 0);
        assert_eq!(field.units(2047, 8191), 8191);
        assert_eq!(field.units(1023, 8191), 4093);
        // мусор за пределами logical range зажимается
        assert_eq!(field.units(9999, 8191), 8191);
    }

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    /// Настоящий digitizer-дескриптор Gaomon S620 с этой машины
    /// (`/sys/.../0003:256C:006D.0003/report_descriptor`): report id 10,
    /// кнопки 6 бит, in-range, X/Y по 16, давление 16 (0..8191).
    const GAOMON_DIGITIZER: &str = "\
        05 0d 09 02 a1 01 85 0a 09 20 a1 00 09 42 09 44 09 45 09 3c \
        15 00 25 01 75 01 95 06 81 02 09 32 75 01 95 01 81 02 81 03 \
        05 01 09 30 09 31 55 0d 65 33 26 ff 7f 35 00 46 00 08 75 10 \
        95 02 81 02 05 0d 09 30 26 ff 1f 75 10 95 01 81 02 09 3d 09 3e \
        15 81 25 7f 75 08 95 02 81 02 c0 c0";

    /// Настоящий digitizer-дескриптор Parblo Ninos N4
    /// (`/sys/.../0003:0483:A019.0027/report_descriptor`): report id 2,
    /// X 0..21600, Y 0..16200, давление 0..8191.
    const PARBLO_DIGITIZER: &str = "\
        05 0d 09 02 a1 01 85 09 c0 \
        05 0d 09 01 a1 01 85 02 09 20 a1 00 09 42 09 44 09 45 \
        15 00 25 01 75 01 95 03 81 02 95 02 81 03 09 32 95 01 81 02 \
        95 02 81 03 75 10 95 01 35 00 a4 05 01 09 30 65 13 55 0d \
        46 a0 0f 26 60 54 81 02 09 31 46 b8 0b 26 48 3f 81 02 b4 \
        09 30 45 00 26 ff 1f 81 42 09 3d 15 81 25 7f 75 08 95 01 81 02 \
        09 3e 15 81 25 7f 81 02 c0 c0";

    /// Настоящий vendor-дескриптор Gaomon S620
    /// (`/sys/.../0003:256C:006D.0002/report_descriptor`): один 88-битный
    /// opaque-репорт с report id 8, то есть 12 байт. Это не digitizer.
    #[test]
    fn real_gaomon_vendor_descriptor_is_not_a_digitizer() {
        let desc = hex("06 00 ff 09 01 a1 01 85 08 75 58 95 01 09 01 81 02 c0");
        assert!(DigitizerLayout::parse(&desc).is_none());
        assert_eq!(
            super::input_report_lengths(&desc),
            vec![(Some(0x08), 12)]
        );
    }

    #[test]
    fn real_gaomon_digitizer_descriptor_parses() {
        let desc = hex(GAOMON_DIGITIZER);
        let layout = DigitizerLayout::parse(&desc).expect("layout");
        assert_eq!(layout.reports.len(), 1);
        let r = layout.reports[0];
        assert_eq!(r.report_id, Some(0x0a));
        assert_eq!((r.x.bit_offset, r.x.bit_size), (8, 16));
        assert_eq!((r.tip.unwrap().bit_offset, r.tip.unwrap().bit_size), (0, 1));
        assert_eq!(
            (r.in_range.unwrap().bit_offset, r.in_range.unwrap().bit_size),
            (6, 1)
        );
        assert_eq!(
            (r.pressure.unwrap().bit_offset, r.pressure.unwrap().bit_size),
            (40, 16)
        );
        // минимум по X/Y/давлению = 7 байт + report id; полная длина с tilt — 10
        assert_eq!(r.min_report_len(), 8);
        assert_eq!(
            super::input_report_lengths(&desc),
            vec![(Some(0x0a), 10)]
        );

        // tip + in_range, X=10000, Y=5000, pressure=5000
        let report = hex("0a 41 10 27 88 13 88 13 00 00");
        let p = layout.decode(&report).expect("reading");
        assert_eq!(p.report_id, Some(0x0a));
        assert_eq!(p.x_raw, 10000);
        assert_eq!(p.y_raw, Some(5000));
        assert_eq!(p.pressure, 5000);
        assert!(p.tip);
        assert!(p.in_range);
        assert!((p.x_norm - 10000.0 / 32767.0).abs() < 1e-6);
    }

    #[test]
    fn real_parblo_digitizer_descriptor_parses() {
        let desc = hex(PARBLO_DIGITIZER);
        let layout = DigitizerLayout::parse(&desc).expect("layout");
        assert_eq!(layout.reports.len(), 1);
        let r = layout.reports[0];
        assert_eq!(r.report_id, Some(0x02));
        assert_eq!((r.x.bit_offset, r.x.bit_size), (8, 16));
        assert_eq!((r.tip.unwrap().bit_offset, r.tip.unwrap().bit_size), (0, 1));
        assert_eq!(
            (r.in_range.unwrap().bit_offset, r.in_range.unwrap().bit_size),
            (5, 1)
        );
        assert_eq!(
            (r.pressure.unwrap().bit_offset, r.pressure.unwrap().bit_size),
            (40, 16)
        );
        assert_eq!(r.x.logical_max, 21600);
        assert_eq!(r.y.unwrap().logical_max, 16200);
        assert_eq!(r.pressure.unwrap().logical_max, 8191);
        // минимум по X/Y/давлению = 7 байт + report id; полная длина с tilt — 10
        assert_eq!(r.min_report_len(), 8);
        assert_eq!(
            super::input_report_lengths(&desc),
            vec![(Some(0x02), 10)]
        );

        // tip + in_range, X=10000, Y=8000, pressure=4000
        let report = hex("02 21 10 27 40 1f a0 0f 00 00");
        let p = layout.decode(&report).expect("reading");
        assert_eq!(p.report_id, Some(0x02));
        assert_eq!(p.x_raw, 10000);
        assert_eq!(p.y_raw, Some(8000));
        assert_eq!(p.pressure, 4000);
        assert!(p.tip);
        assert!(p.in_range);
        assert!((p.x_norm - 10000.0 / 21600.0).abs() < 1e-6);
    }
}
