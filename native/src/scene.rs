//! Сборка кадра: спрайты сцены Deemo в мировых координатах.
//!
//! Геометрия повторяет Deenote (DeemoStage.unity + GameStageArgs.asset):
//! всё лежит на плоскости y = 0, камера в (0, 4.2007, −6.7610) под 20°.

use crate::chart::{Chart, NoteKind};
use crate::math::{perspective, project, view, Mat4};
use crate::stage::*;

/// Цвет link-линии (GridLineArgs.LinkLineColor).
const LINK_LINE_COLOR: [f32; 3] = [1.0, 233.0 / 255.0, 135.0 / 255.0];
/// GridLineArgs.LinkLineWidth, в пикселях экрана.
const LINK_LINE_WIDTH_PX: f32 = 2.0;
/// Такт дыхания фона (DeemoGameStageArgs.BackgroundMaskPeriod).
const BG_MASK_PERIOD: f32 = 2.0;
const BG_MASK_MIN_ALPHA: f32 = 0.3333333;
const BG_MASK_MAX_ALPHA: f32 = 1.0;
/// JudgeLinePeriod / JudgeLineHitEffectAlphaDecTime.
const JUDGE_LINE_PERIOD: f32 = 3.5;
const JUDGE_HIT_DEC_TIME: f32 = 0.5;

pub struct Scene<'a> {
    pub chart: &'a Chart,
    /// скорость падения (настройка игрока, 0.5..9.5)
    pub speed: f32,
    /// состояние игры (если идёт игра) — для скрытия сбитых нот и вспышек
    pub play: Option<&'a crate::play::Play>,
    /// направляющие линии от нот до линии нажатия
    pub guides: bool,
}

impl<'a> Scene<'a> {
    pub fn new(chart: &'a Chart, speed: f32) -> Self {
        Scene {
            chart,
            speed,
            play: None,
            guides: false,
        }
    }

    pub fn with_play(chart: &'a Chart, speed: f32, play: &'a crate::play::Play) -> Self {
        Scene {
            chart,
            speed,
            play: Some(play),
            guides: false,
        }
    }

    pub fn mvp(&self, aspect: f32) -> Mat4 {
        let p = perspective(CAMERA_FOV_Y, aspect, CAMERA_NEAR, CAMERA_FAR);
        let v = view(CAMERA_POS, CAMERA_PITCH);
        crate::math::mat4_mul(&p, &v)
    }

    /// Дыхание затемнения фона: 1 → максимум затемнения.
    pub fn bg_mask_alpha(&self, t: f32) -> f32 {
        let ratio = (t * (2.0 * std::f32::consts::PI / BG_MASK_PERIOD)).sin();
        let r = (ratio + 1.0) * 0.5;
        BG_MASK_MIN_ALPHA + (BG_MASK_MAX_ALPHA - BG_MASK_MIN_ALPHA) * r
    }

    /// Дыхание судейской линии.
    pub fn judge_breathe(&self, t: f32) -> f32 {
        let ratio = (t * (2.0 * std::f32::consts::PI / JUDGE_LINE_PERIOD)).sin();
        (ratio + 1.0) * 0.5
    }

    /// Альфа вспышки судейской линии от последней сыгранной ноты.
    pub fn judge_hit_alpha(&self, t: f32) -> f32 {
        let hit = match self.play {
            Some(p) => {
                if p.last_hit_time < 0.0 {
                    None
                } else {
                    Some(p.last_hit_time)
                }
            }
            None => self.chart.previous_hit_time(t),
        };
        match hit {
            Some(ht) => {
                let dt = t - ht;
                if dt < JUDGE_HIT_DEC_TIME {
                    (1.0 - dt / JUDGE_HIT_DEC_TIME).max(0.0).sqrt()
                } else {
                    0.0
                }
            }
            None => 0.0,
        }
    }

    /// Все спрайты кадра в порядке отрисовки.
    pub fn sprites(&self, t: f32, width: u32, height: u32) -> Vec<Sprite> {
        let aspect = width as f32 / height.max(1) as f32;
        let mvp = self.mvp(aspect);
        let mut out: Vec<Sprite> = Vec::with_capacity(256);

        // --- пюпитр: два нотоносца и боковые линии (sorting order -30) ---
        let stave_alpha = [1.0, 1.0, 1.0, 0.3254902];
        // Staves: pos z = 5.375, rot 90° X, scale 3.4; StaveLines scale 1.1
        // StaveUp/StaveDn: смещение ±0.263 в локальных единицах
        let stave_w = 10.24 * 1.1 * 3.4;
        let stave_h = 0.26 * 1.1 * 3.4;
        for dz in [0.263 * 3.4, -0.263 * 3.4] {
            out.push(Sprite::floor(
                Tex::Stave,
                0.0,
                5.375 + dz,
                stave_w,
                stave_h,
                stave_alpha,
            ));
        }
        // LineL / LineR: GP_line 0.04×0.51, scale (0.4, 1.7) × 3.4
        for x in [-1.338 * 3.4, 1.338 * 3.4] {
            out.push(Sprite::floor(
                Tex::Line,
                x,
                5.375,
                0.04 * 0.4 * 3.4,
                0.51 * 1.7 * 3.4,
                [1.0, 1.0, 1.0, 0.33333334],
            ));
        }

        // --- link-линии ---
        // Рисуются в sorting layer "Lines", то есть поверх нот.
        let mut links: Vec<Sprite> = Vec::new();
        {
            // пикселей на мировую единицу на глубине z (для толщины линии)
            let tan_half = (CAMERA_FOV_Y * 0.5).tan();
            let px_per_unit = height as f32 / (2.0 * tan_half);
            let note_alpha_of = |n: &crate::chart::Note| {
                note_alpha(n.time - t, self.speed, n.speed)
            };
            for link in &self.chart.links {
                let mut prev: Option<&crate::chart::Note> = None;
                for idx in &link.notes {
                    let Some(cur) = self.chart.notes.get(*idx) else {
                        prev = None;
                        continue;
                    };
                    if let Some(p) = prev {
                        let dt = cur.time - t;
                        if dt > appear_ahead(self.speed, cur.speed) {
                            prev = Some(cur);
                            continue;
                        }
                        let a = [note_x(p.pos), world_z(p.time - t, self.speed, p.speed)];
                        let b = [note_x(cur.pos), world_z(cur.time - t, self.speed, cur.speed)];
                        let a = [a[0], a[1].max(0.0)];
                        let b = [b[0], b[1].max(0.0)];
                        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, 0.0];
                        let depth = project(&mvp, mid)[2].max(0.5);
                        let world_per_px = depth / (px_per_unit * 1.0);
                        let alpha = note_alpha_of(p);
                        if alpha > 0.004 {
                            links.push(Sprite::floor_line(
                                Tex::White,
                                a,
                                b,
                                world_per_px * LINK_LINE_WIDTH_PX,
                                [LINK_LINE_COLOR[0], LINK_LINE_COLOR[1], LINK_LINE_COLOR[2], alpha],
                            ));
                        }
                    }
                    prev = Some(cur);
                }
            }
        }

        // --- ноты (sorting order -1), дальние первыми ---
        let mut notes: Vec<(f32, Sprite)> = Vec::new();
        let mut guide_lines: Vec<Sprite> = Vec::new();
        let mut fx_low: Vec<Sprite> = Vec::new(); // glow/circle — под линией
        let mut fx_high: Vec<Sprite> = Vec::new(); // wave/explosion — над линией
        for (i, n) in self.chart.notes.iter().enumerate() {
            let dt = n.time - t;
            // сбитая нота: не рисуем падающую, проигрываем эффекты попадания
            if let Some(p) = self.play {
                let st = &p.states[i];
                if st.judged && st.result != crate::play::Judge::Miss {
                    let age = t - st.hit_time;
                    // Perfect — ровно на линии; Good — там, где нота была в момент нажатия
                    let z = if st.result == crate::play::Judge::Perfect {
                        0.0
                    } else {
                        world_z(st.hit_dt, self.speed, n.speed)
                    };
                    // золотой луч только на Perfect; на Good — белый
                    let tint = if st.result == crate::play::Judge::Perfect {
                        [1.0, 0.7490196, 0.1764706]
                    } else {
                        [1.0, 1.0, 1.0]
                    };
                    hit_effect(
                        note_x(n.pos),
                        z,
                        n.size,
                        wave_color(n.kind),
                        tint,
                        age,
                        &mut fx_low,
                        &mut fx_high,
                    );
                    continue;
                }
            }
            if dt >= appear_ahead(self.speed, n.speed) || dt < -JUDGE_HIT_DEC_TIME {
                continue;
            }
            let alpha = note_alpha(dt, self.speed, n.speed);
            if alpha <= 0.004 && dt >= 0.0 {
                continue;
            }
            let z = world_z(dt.max(0.0), self.speed, n.speed);
            // направляющая от ноты до линии нажатия (опция)
            if self.guides && dt >= 0.0 && z > 0.05 {
                let gc = match n.kind {
                    NoteKind::Slide => [1.0, 0.91, 0.53, 0.28 * alpha],
                    _ => [1.0, 1.0, 1.0, 0.22 * alpha],
                };
                guide_lines.push(Sprite::floor_line(
                    Tex::White,
                    [note_x(n.pos), z],
                    [note_x(n.pos), 0.0],
                    0.05,
                    gc,
                ));
            }
            let (tex, base_w, base_h, k) = match n.kind {
                // SlideNoteSpritePrefab: 2.18×0.70, scale 0.975
                NoteKind::Slide => (Tex::SlideNote, 2.18, 0.70, 0.975),
                // NoSoundNoteSpritePrefab: 1.85×0.46, scale 1 (белый центр)
                NoteKind::NoSound => (Tex::NoSoundNote, 1.85, 0.46, 1.0),
                // BlackNoteSpritePrefab: 1.48×0.67, scale 1.5
                _ => (Tex::BlackNote, 1.48, 0.67, 1.5),
            };
            let s = Sprite::tilted(
                tex,
                [note_x(n.pos), 0.0, z],
                base_w * n.size * k,
                base_h * k,
                NOTE_PITCH,
                [0.5, 0.5],
                [1.0, 1.0, 1.0, alpha],
            );
            let depth = project(&mvp, s.origin)[2];
            notes.push((depth, s));
        }
        notes.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        out.extend(guide_lines);
        out.extend(notes.into_iter().map(|(_, s)| s));
        out.extend(links);
        out.extend(fx_low);

        // --- судейская линия (sorting order 0) ---
        // JudgeLine: 10.24×0.13 × scale (2, 1), rot 15°, пивот центр
        out.push(Sprite::tilted(
            Tex::JudgeLine,
            [0.0, 0.0, 0.0],
            10.24 * 2.0,
            0.13,
            15.0_f32.to_radians(),
            [0.5, 0.5],
            [1.0, 1.0, 1.0, 1.0],
        ));
        // JudgeLineBreatheEffect: 10.24×0.35 × scale (2, 0.5), пивот снизу
        out.push(Sprite::tilted(
            Tex::JudgeLineEf,
            [0.0, 0.0, 0.0],
            10.24 * 2.0,
            0.35 * 0.5,
            15.0_f32.to_radians(),
            [0.5, 0.0],
            [1.0, 1.0, 1.0, self.judge_breathe(t)],
        ));
        // JudgeLineHitEffect: тот же спрайт, scale (2, 2), пивот центр
        let hit = self.judge_hit_alpha(t);
        if hit > 0.004 {
            out.push(Sprite::tilted(
                Tex::JudgeLineEf,
                [0.0, 0.0, 0.0],
                10.24 * 2.0,
                0.35 * 2.0,
                15.0_f32.to_radians(),
                [0.5, 0.5],
                [1.0, 1.0, 1.0, hit],
            ));
        }

        // wave/explosion — поверх судейской линии (sorting order 1/2)
        out.extend(fx_high);

        out
    }
}

/// Курсор просмотра реплея: два вертикальных луча — левый (перо) и правый
/// (мышь), как маркеры ввода в игре. Никаких точек и хвоста из 12 точек:
/// несколько квадов на луч, без аллокаций на кадр.
///
/// На кадре попадания прилипший к `x_note` луч стоит ровно на ноте
/// (см. [`crate::replay::ReplayDriver::beam_x`]); второй идёт по своему треку.
pub fn push_replay_cursor(
    out: &mut Vec<Sprite>,
    driver: &crate::replay::ReplayDriver,
    t: f32,
) {
    use crate::replay::BeamSide;
    if !driver.has_cursor() {
        return;
    }
    for side in [BeamSide::Left, BeamSide::Right] {
        if let Some(px) = driver.beam_x(t, side) {
            push_beam(out, note_x(px), side);
        }
    }
}

/// Живые лучи Line cursor: правая рука — мышь, левая — перо (если планшет
/// присутствует). Тот же визуал, что у просмотра реплея.
pub fn push_live_cursor(out: &mut Vec<Sprite>, mouse_x: f32, pen_x: Option<f32>) {
    use crate::replay::BeamSide;
    push_beam(out, mouse_x, BeamSide::Right);
    if let Some(px) = pen_x {
        push_beam(out, px, BeamSide::Left);
    }
}

fn beam_tint(side: crate::replay::BeamSide) -> [f32; 4] {
    use crate::replay::BeamSide;
    match side {
        BeamSide::Left => [1.00, 0.66, 0.30, 1.0],
        BeamSide::Right => [0.42, 0.78, 1.00, 1.0],
    }
}

/// Один вертикальный луч: широкий ореол + тонкое ядро + мягкое свечение у
/// основания. Несколько квадов, без аллокаций.
fn push_beam(out: &mut Vec<Sprite>, x: f32, side: crate::replay::BeamSide) {
    let tint = beam_tint(side);
    out.push(Sprite::tilted(
        Tex::White,
        [x, 0.0, 0.0],
        0.09,
        3.8,
        0.0,
        [0.5, 0.0],
        [tint[0], tint[1], tint[2], 0.16],
    ));
    out.push(Sprite::tilted(
        Tex::White,
        [x, 0.0, 0.0],
        0.026,
        3.8,
        0.0,
        [0.5, 0.0],
        [tint[0], tint[1], tint[2], 0.78],
    ));
    let mut glow = Sprite::tilted(
        Tex::Glow,
        [x, 0.01, 0.0],
        0.52,
        0.34,
        std::f32::consts::FRAC_PI_2,
        [0.5, 0.5],
        [tint[0], tint[1], tint[2], 0.30],
    );
    glow.additive = false;
    out.push(glow);
}
