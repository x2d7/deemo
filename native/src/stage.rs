//! Константы и формулы сцены Deemo, вытащенные из Deenote
//! (Assets/Scenes/GameStages/DeemoStage.unity + Assets/Resources/GamePlay/Deemo/*.asset).

/// Ширина нотного поля в мировых единицах (GameStageArgs.NotePanelWidth).
pub const NOTE_PANEL_WIDTH: f32 = 6.52;
/// Длина поля по Z (GameStageArgs.NotePanelBaseLength) — от судейской линии до спавна.
pub const NOTE_PANEL_BASE_LENGTH: f32 = 63.4;
/// GameStageArgs.NoteFadeInRangePercent
pub const NOTE_FADE_IN_RANGE_PERCENT: f32 = 0.6556875;
/// EntityArgs.StageMaxPositionWidth — координата pos ∈ [-2, 2].
pub const STAGE_MAX_POSITION: f32 = 2.0;

/// Main Camera: m_LocalPosition, m_LocalRotation (w=0.9848078, x=0.17364816) → 20°.
pub const CAMERA_POS: [f32; 3] = [0.0, 4.200734, -6.7610025];
pub const CAMERA_PITCH: f32 = 20.0 * std::f32::consts::PI / 180.0;
/// Camera.field of view (вертикальный).
pub const CAMERA_FOV_Y: f32 = 36.0 * std::f32::consts::PI / 180.0;
pub const CAMERA_NEAR: f32 = 0.3;
pub const CAMERA_FAR: f32 = 1000.0;

/// GameStageArgs.NoteTimeToZBaseMultipliers — для скоростей 1.0, 1.5, ... 9.5.
const TIME_TO_Z: [f32; 18] = [
    8.91, 7.0, 6.39, 5.86, 5.86, 5.86, 6.1, 6.6, 6.8, 7.45, 8.0, 9.0, 10.0, 11.1, 13.0, 14.0, 15.0,
    17.4,
];

/// PiecewiseLinearFunction с fillHorizontalBothSide.
pub fn z_multiplier(speed: f32) -> f32 {
    let t = (speed - 1.0) / 0.5;
    if t <= 0.0 {
        return TIME_TO_Z[0];
    }
    let last = TIME_TO_Z.len() - 1;
    if t >= last as f32 {
        return TIME_TO_Z[last];
    }
    let i = t.floor() as usize;
    let f = t - i as f32;
    TIME_TO_Z[i] + (TIME_TO_Z[i + 1] - TIME_TO_Z[i]) * f
}

/// ConvertNoteCoordPositionToWorldX
pub fn note_x(pos: f32) -> f32 {
    pos * (NOTE_PANEL_WIDTH / (STAGE_MAX_POSITION * 2.0))
}

/// StageNoteActiveAheadTime — сколько секунд нота летит до линии.
pub fn active_ahead_time(speed: f32) -> f32 {
    NOTE_PANEL_BASE_LENGTH / z_multiplier(speed) / speed
}

/// Время, за которое нота с noteSpeed = s долетает до линии.
pub fn appear_ahead(fall_speed: f32, note_speed: f32) -> f32 {
    active_ahead_time(fall_speed) / note_speed
}

/// Затухание к дальнему краю (SetNoteSpriteAlpha) для dt = note.Time − music.Time.
pub fn note_alpha(dt: f32, fall_speed: f32, note_speed: f32) -> f32 {
    let appear = appear_ahead(fall_speed, note_speed);
    let fade_end = appear * (1.0 - NOTE_FADE_IN_RANGE_PERCENT);
    // MapTo(dt, appear, fade_end, 0, 1)
    if appear <= fade_end {
        return 1.0;
    }
    let a = (dt - appear) / (fade_end - appear);
    a.clamp(0.0, 1.0)
}

/// Z ноты: ConvertNoteCoordTimeToWorldZ.
pub fn world_z(dt: f32, fall_speed: f32, note_speed: f32) -> f32 {
    fall_speed * note_speed * z_multiplier(fall_speed) * dt
}

/// Смещение подъёма ноты: CameraRotation (20°) × NoteSprite (20°) в префабе Note.
pub const NOTE_PITCH: f32 = 40.0 * std::f32::consts::PI / 180.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tex {
    Bg,
    BgMask,
    JudgeLine,
    JudgeLineEf,
    Stave,
    Line,
    BlackNote,
    SlideNote,
    NoSoundNote,
    HoldBody,
    White,
    Glow,
    Circle,
    Wave,
    /// градиентная направляющая засечки (яркая у линии, затухает вдаль)
    Marker,
    /// покадровый взрыв ноты, 0..14 → note_explosion01..15
    Explosion(u8),
}

/// Четырёхугольник в мировых координатах (спрайт).
pub struct Sprite {
    pub tex: Tex,
    /// точка привязки (пивот) в мире
    pub origin: [f32; 3],
    /// размер в мировых единицах (ширина, высота)
    pub size: [f32; 2],
    /// единичный вектор оси X спрайта
    pub right: [f32; 3],
    /// единичный вектор оси Y спрайта (верх текстуры)
    pub up: [f32; 3],
    /// 0..1: где находится origin внутри спрайта
    pub pivot: [f32; 2],
    pub color: [f32; 4],
    /// аддитивный блендинг (для свечения)
    pub additive: bool,
}

impl Sprite {
    /// Спрайт, лежащий на полу (y = 0): верх текстуры уходит вдаль (+Z).
    pub fn floor(tex: Tex, x: f32, z: f32, w: f32, h: f32, color: [f32; 4]) -> Sprite {
        Sprite {
            tex,
            origin: [x, 0.0, z],
            size: [w, h],
            right: [1.0, 0.0, 0.0],
            up: [0.0, 0.0, 1.0],
            pivot: [0.5, 0.5],
            color,
            additive: false,
        }
    }

    /// Спрайт, наклонённый вокруг X на pitch (верх уходит вдаль).
    pub fn tilted(
        tex: Tex,
        pos: [f32; 3],
        w: f32,
        h: f32,
        pitch: f32,
        pivot: [f32; 2],
        color: [f32; 4],
    ) -> Sprite {
        let (s, c) = pitch.sin_cos();
        Sprite {
            tex,
            origin: pos,
            size: [w, h],
            right: [1.0, 0.0, 0.0],
            up: [0.0, c, s],
            pivot,
            color,
            additive: false,
        }
    }

    /// Полоса на полу между двумя точками (используется для link-линий).
    pub fn floor_line(
        tex: Tex,
        a: [f32; 2],
        b: [f32; 2],
        width: f32,
        color: [f32; 4],
    ) -> Sprite {
        let dx = b[0] - a[0];
        let dz = b[1] - a[1];
        let len = (dx * dx + dz * dz).sqrt().max(1e-5);
        // перпендикуляр в плоскости пола
        let nx = -dz / len;
        let nz = dx / len;
        Sprite {
            tex,
            origin: [a[0], 0.0, a[1]],
            size: [width, len],
            right: [nx, 0.0, nz],
            up: [dx / len, 0.0, dz / len],
            pivot: [0.5, 0.0],
            color,
            additive: false,
        }
    }
}

/// Наклон эффектов взрыва/волны: только CameraRotation (20°), как в префабе Note.
const EFFECT_PITCH: f32 = 20.0 * std::f32::consts::PI / 180.0;
/// Полная длительность HitEffect (GameStageArgs.HitEffectTime).
pub const HIT_EFFECT_TIME: f32 = 0.8;

/// Эффекты попадания Deenote (DeemoGameStageNoteController.SetHitEffect).
/// age = t − hit_time; z — где нота была в момент попадания (Perfect → 0, линия).
/// glow/circle кладутся в `low` (под линией), wave/explosion в `high`.
pub fn hit_effect(
    x: f32,
    z: f32,
    size: f32,
    wave_color: [f32; 4],
    tint: [f32; 3],
    age: f32,
    low: &mut Vec<Sprite>,
    high: &mut Vec<Sprite>,
) {
    if age < 0.0 || age > HIT_EFFECT_TIME {
        return;
    }
    let pos = [x, 0.0, z];
    let floor_pitch = std::f32::consts::FRAC_PI_2;

    // Glow: GPeffect_wave1, size (0.32,1.28), scale (6.5, height*13), на полу
    {
        let ratio = if age <= 0.1333333 {
            age / 0.1333333
        } else {
            1.0 - (age - 0.1333333) / 0.6666667
        }
        .clamp(0.0, 1.0);
        let w = 0.32 * 6.5;
        let h = 1.28 * ratio * 13.0;
        // GlowEffectMaterial._GlowColor = (1, 0.749, 0.176, 0.659), аддитивный бленд.
        // Цвет по судейству: Perfect — золотой, Good — белый (tint).
        let mut glow = Sprite::tilted(
            Tex::Glow,
            pos,
            w,
            h,
            floor_pitch,
            [0.5, 0.5],
            [tint[0], tint[1], tint[2], ratio * 0.65882355],
        );
        glow.additive = true;
        low.push(glow);
    }
    // Circle: shockwave, size 2.12, scale = ratio^0.6 * 2, alpha = (1-ratio)^0.33
    {
        let ratio = (age / 0.5).clamp(0.0, 1.0);
        let s = ratio.powf(0.6) * 2.0;
        let alpha = (1.0 - ratio).powf(0.33);
        let w = 2.12 * s;
        low.push(Sprite::tilted(Tex::Circle, pos, w, w, floor_pitch, [0.5, 0.5], [1.0, 1.0, 1.0, alpha]));
    }
    // Wave: shockwave3, size (2.19,1.13), scale = size*(0.8, sqrt(lin)*1.5), pitch 20°
    {
        let lin = if age <= 0.1 {
            age / 0.1
        } else {
            1.0 - (age - 0.1) / 0.25
        }
        .clamp(0.0, 1.0);
        let ratio = lin.sqrt();
        let w = 2.19 * 0.8 * size;
        let h = 1.13 * ratio * 1.5 * size;
        high.push(Sprite::tilted(Tex::Wave, pos, w, h, EFFECT_PITCH, [0.5, 0.5], wave_color));
    }
    // Explosion: покадровый note_explosion01..15, scale = size*1.6, pitch 20°
    {
        let frame = (age / 0.45 * 17.0).floor() as i32;
        if (1..=15).contains(&frame) {
            let w = 1.2 * 1.6 * size;
            let h = 1.0 * 1.6 * size;
            high.push(Sprite::tilted(
                Tex::Explosion((frame - 1) as u8),
                pos,
                w,
                h,
                EFFECT_PITCH,
                [0.5, 0.5],
                [1.0, 1.0, 1.0, 1.0],
            ));
        }
    }
}

/// Цвет волны по типу ноты (WaveColor из GameStageArgs).
pub fn wave_color(kind: crate::chart::NoteKind) -> [f32; 4] {
    match kind {
        crate::chart::NoteKind::Slide => [0.9058824, 0.882353, 0.5686275, 0.79607844],
        crate::chart::NoteKind::Swipe => [0.41320753, 0.5556929, 1.0, 1.0],
        _ => [0.0, 0.0, 0.0, 0.6],
    }
}

/// Засечка на судейской линии: вертикальная риска в мировой точке x.
/// press — нажим стилусом (ярче и выше), hover — тусклее.
pub fn marker(x: f32, color: [f32; 4], press: bool) -> Sprite {
    // продолговатая направляющая от линии до места спавна нот (с перспективой)
    let w = if press { 0.26 } else { 0.18 };
    let a = if press { 0.6 } else { 0.35 };
    let len = NOTE_PANEL_BASE_LENGTH;
    Sprite::floor(
        Tex::Marker,
        x,
        len * 0.5,
        w,
        len,
        [color[0], color[1], color[2], a],
    )
}
