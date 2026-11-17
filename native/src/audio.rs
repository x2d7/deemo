//! Воспроизведение музыки (rodio) + часы, привязанные к позиции аудио.

use anyhow::{Context, Result};
use rodio::Source;
use std::fs::File;
use std::io::BufReader;
use std::time::Duration;

pub struct MusicPlayer {
    sink: rodio::Sink,
    _rodio: Option<rodio::OutputStream>,
    _custom: Option<(
        rodio::cpal::Stream,
        std::sync::Arc<rodio::dynamic_mixer::DynamicMixerController<f32>>,
    )>,
    duration: f32,
    loaded: bool,
}

impl MusicPlayer {
    pub fn new() -> Result<MusicPlayer> {
        // Low-latency backend: свой cpal-поток с маленьким буфером + микшер rodio.
        // Default-поток rodio даёт задержку ~100-200 мс (большой буфер ALSA/PipeWire).
        if let Ok(mp) = Self::new_low_latency() {
            return Ok(mp);
        }
        let (stream, handle) =
            rodio::OutputStream::try_default().context("no audio device")?;
        let sink = rodio::Sink::try_new(&handle).context("sink was not created")?;
        sink.pause();
        eprintln!("[audio] fallback: rodio default (high latency)");
        Ok(MusicPlayer {
            sink,
            _rodio: Some(stream),
            _custom: None,
            duration: 0.0,
            loaded: false,
        })
    }

    fn new_low_latency() -> Result<MusicPlayer> {
        use rodio::cpal::{self, traits::DeviceTrait, traits::HostTrait, traits::StreamTrait};
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("no audio device")?;
        let def = device.default_output_config().context("no output config")?;
        let channels = def.channels();
        let rate = def.sample_rate();
        let (mixer_tx, mut mixer_rx) =
            rodio::dynamic_mixer::mixer::<f32>(channels, rate.0);
        let (sink, queue_rx) = rodio::Sink::new_idle();
        mixer_tx.add(queue_rx);
        let config = cpal::StreamConfig {
            channels,
            sample_rate: rate,
            buffer_size: cpal::BufferSize::Fixed(2048),
        };
        let stream = device
            .build_output_stream::<f32, _, _>(
                &config,
                move |data: &mut [f32], _| {
                    for d in data.iter_mut() {
                        *d = mixer_rx.next().unwrap_or(0.0);
                    }
                },
                |err| eprintln!("[audio] stream error: {err}"),
                None,
            )
            .context("low-latency stream was not built")?;
        stream.play().context("stream did not start")?;
        sink.pause();
        eprintln!(
            "[audio] low-latency: 2048-frame buffer (~{:.1} ms), {} Hz",
            2048.0 / rate.0 as f32 * 1000.0,
            rate.0
        );
        Ok(MusicPlayer {
            sink,
            _rodio: None,
            _custom: Some((stream, mixer_tx)),
            duration: 0.0,
            loaded: false,
        })
    }

    pub fn load(&mut self, path: &str) -> Result<()> {
        let file = File::open(path).with_context(|| format!("no audio {path}"))?;
        let decoder = rodio::Decoder::new(BufReader::new(file))
            .with_context(|| format!("cannot decode {path}"))?;
        self.duration = decoder.total_duration().map(|d| d.as_secs_f32()).unwrap_or(0.0);
        self.sink.stop();
        // stop() делает sink непригодным? нет — stop очищает и оставляет usable.
        self.sink.append(decoder);
        self.sink.pause();
        self.loaded = true;
        Ok(())
    }

    pub fn play(&self) {
        if self.loaded {
            self.sink.play();
        }
    }

    pub fn pause(&self) {
        self.sink.pause();
    }

    pub fn is_playing(&self) -> bool {
        self.loaded && !self.sink.is_paused() && !self.sink.empty()
    }

    /// Музыка доиграна (очередь sink пуста).
    pub fn is_empty(&self) -> bool {
        self.loaded && self.sink.empty()
    }

    /// Позиция воспроизведения в секундах (часы игры).
    pub fn time(&self) -> f32 {
        if !self.loaded {
            return 0.0;
        }
        self.sink.get_pos().as_secs_f32()
    }

    pub fn seek(&self, t: f32) {
        if self.loaded {
            let _ = self.sink.try_seek(Duration::from_secs_f32(t.max(0.0)));
        }
    }

    pub fn duration(&self) -> f32 {
        self.duration
    }

    pub fn set_volume(&self, v: f32) {
        // DEEMO_MUTE=1 — тихий headless-прогон для автоскриншотов; часы
        // продолжают идти от аудиопотока, просто громкость 0.
        let v = if std::env::var_os("DEEMO_MUTE").is_some() {
            0.0
        } else {
            v
        };
        self.sink.set_volume(v.clamp(0.0, 1.0));
    }
}