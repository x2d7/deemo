//! Игровые часы: lead-in до первой ноты + монотонное время от якоря.
//!
//! rodio отдаёт позицию «лестницей» (шаг = размер буфера), поэтому напрямую
//! брать get_pos нельзя — ноты будут дёргаться. Вместо этого время интегрируется
//! по монотонному wall-clock от якоря, а get_pos используется только для
//! привязки при старте и обнаружения seek/большого дрейфа.

use std::time::{Duration, Instant};

pub struct Clock {
    start: Instant,
    lead_in: f32,
    smooth: f32,
    started: bool,
    anchor_wall: Instant,
    anchor_audio: f32,
    pause_start: Option<Instant>,
    /// поправка синхронизации (сек): вычитается из игрового времени
    offset: f32,
}

impl Clock {
    pub fn new(lead_in: f32, offset: f32) -> Clock {
        let now = Instant::now();
        Clock {
            start: now,
            lead_in,
            smooth: -lead_in,
            started: false,
            anchor_wall: now,
            anchor_audio: 0.0,
            pause_start: None,
            offset,
        }
    }

    pub fn lead_in(&self) -> f32 {
        self.lead_in
    }

    /// Сколько секунд прошло с старта (без пауз).
    pub fn elapsed(&self) -> f32 {
        let extra = self
            .pause_start
            .map(|p| p.elapsed().as_secs_f32())
            .unwrap_or(0.0);
        self.start.elapsed().as_secs_f32() - extra
    }

    pub fn current(&self) -> f32 {
        self.smooth - self.offset
    }

    /// Время карты для произвольного wall-мгновения (без мутации часов).
    /// Используется для датировки сырых событий ввода в реплее.
    pub fn wall_to_time(&self, wall: Instant) -> f32 {
        let now = Instant::now();
        let dt = if wall >= now {
            (wall - now).as_secs_f32()
        } else {
            -(now - wall).as_secs_f32()
        };
        let base = if self.started {
            self.anchor_audio + (now - self.anchor_wall).as_secs_f32()
        } else {
            self.smooth
        };
        base + dt - self.offset
    }

    /// Продвинуть часы. audio — позиция музыки (сек), music_on — играет ли звук.
    pub fn time(&mut self, audio: f32, music_on: bool) -> f32 {
        let now = Instant::now();
        let elapsed = self.elapsed();
        if elapsed < self.lead_in || !music_on {
            // lead-in: музыка ещё не вступила, ноты падают сверху
            self.smooth = elapsed - self.lead_in;
            self.started = false;
            return self.smooth - self.offset;
        }

        if !self.started {
            self.anchor_wall = now;
            self.anchor_audio = audio;
            self.smooth = audio;
            self.started = true;
        } else {
            // монотонное wall-time от якоря (без «лестницы» get_pos)
            self.smooth = self.anchor_audio + (now - self.anchor_wall).as_secs_f32();
            if (audio - self.smooth).abs() > 0.3 {
                // seek / большой дрейф — перепривязка
                self.anchor_wall = now;
                self.anchor_audio = audio;
                self.smooth = audio;
            }
        }
        self.smooth - self.offset
    }

    pub fn pause(&mut self) {
        if self.pause_start.is_none() {
            self.pause_start = Some(Instant::now());
        }
    }

    pub fn resume(&mut self) {
        if let Some(p) = self.pause_start.take() {
            let d = p.elapsed();
            self.start += d;
            // продолжаем с замороженного значения, не снапая к get_pos
            self.anchor_wall = Instant::now();
            self.anchor_audio = self.smooth;
        }
    }

    #[allow(dead_code)]
    pub fn lead_in_duration(&self) -> Duration {
        Duration::from_secs_f32(self.lead_in)
    }
}