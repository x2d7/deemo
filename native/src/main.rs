//! Deemo demo (native). Первый шаг: оффскрин-рендер сцены в PNG, чтобы сверять
//! картинку с оригиналом.

// Release-сборка под Windows — GUI-приложение: без этого за игрой висит
// чёрная консоль. Весь stdout/stderr уходит в deemo.log рядом с exe.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod assoc;
mod audio;
mod bg;
mod chart;
mod clock;
mod hid_desc;
mod library;
mod map_offsets;
mod math;
mod online;
mod online_queue;
mod pack;
mod pack_download;
mod pack_import;
mod play;
mod pp;
mod render;
mod replay;
mod results;
mod scene;
mod screens;
mod settings;
mod settings_screen;
mod stage;
mod stars;
mod tablet;
#[cfg(windows)]
mod tablet_win;
mod text;
mod theme;
mod ui;

use anyhow::{Context, Result};
use chart::Chart;
use render::Renderer;
use scene::Scene;

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    pub fn new(compatible_surface: Option<&wgpu::Surface<'_>>) -> Result<Gpu> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            },
        ))
        .map_err(|e| anyhow::anyhow!("no GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("deemo-device"),
            ..Default::default()
        }))
        .context("device was not created")?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
        })
    }
}

fn shot(
    out: &str,
    chart_path: &str,
    time: f32,
    speed: f32,
    width: u32,
    height: u32,
    autohit: bool,
    replay_path: Option<&str>,
    replay_nick: &str,
) -> Result<()> {
    let gpu = Gpu::new(None)?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(gpu.device, gpu.queue, format)?;

    let chart = Chart::load(chart_path)?;
    // --replay: заранее разобранный DMR1/DMR2/DMR3 вместо синтетического autohit-плея
    let blob = match replay_path {
        Some(path) => {
            let bytes = std::fs::read(path)
                .with_context(|| format!("replay does not read {path}"))?;
            let blob = replay::ReplayBlob::decode(&bytes).map_err(|e| anyhow::anyhow!(e))?;
            if blob.note_count() != chart.notes.len() {
                anyhow::bail!(
                    "replay has {} notes, chart has {}",
                    blob.note_count(),
                    chart.notes.len()
                );
            }
            Some(blob)
        }
        None => None,
    };
    let (play, driver) = if let Some(blob) = &blob {
        let mut p = play::Play::new(&chart);
        let mut d = replay::ReplayDriver::new(&chart, blob, replay_nick)
            .map_err(|e| anyhow::anyhow!(e))?;
        d.apply(&mut p, &chart, time);
        (Some(p), Some(d))
    } else if autohit {
        let offset: f32 = std::env::var("DEEMO_HIT_OFFSET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        let mut p = play::Play::new(&chart);
        for (i, n) in chart.notes.iter().enumerate() {
            if n.time <= time {
                p.states[i].judged = true;
                p.states[i].result = if offset.abs() <= 0.05 {
                    play::Judge::Perfect
                } else {
                    play::Judge::Good
                };
                p.states[i].hit_time = n.time - offset;
                p.states[i].hit_dt = offset;
            }
        }
        (Some(p), None)
    } else {
        (None, None)
    };
    let scene = match &play {
        Some(p) => Scene::with_play(&chart, speed, p),
        None => Scene::new(&chart, speed),
    };
    let aspect = width as f32 / height as f32;
    renderer.set_camera(&scene.mvp(aspect));
    let mut sprites = scene.sprites(time, width, height);
    // DMR3: два луча курсора — тот же код, что в игровом цикле
    if let Some(d) = &driver {
        scene::push_replay_cursor(&mut sprites, d, time);
    }
    let screen = [
        render::ScreenQuad {
            tex: stage::Tex::Bg,
            color: [1.0, 1.0, 1.0, 1.0],
        },
        render::ScreenQuad {
            tex: stage::Tex::BgMask,
            color: [1.0, 1.0, 1.0, scene.bg_mask_alpha(time)],
        },
    ];
    // метка просмотра — только для --replay; путь без него не меняется
    let mut ui = Vec::new();
    if let Some(d) = &driver {
        renderer.set_ui_size(width, height);
        let mut text = text::Text::new(&mut renderer)?;
        screens::draw_replay_badge(&text, &mut ui, &d.nick, time, d.end());
        text.flush(&mut renderer);
    }

    let texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shot"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = renderer
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("shot") });
    renderer.draw(&mut encoder, &view, &sprites, &screen, &ui);

    let padded_row = (width * 4).div_ceil(256) * 256;
    let readback = renderer.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    renderer.queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    renderer.device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: Some(std::time::Duration::from_secs(10)),
    })?;
    rx.recv()??;

    let data = slice.get_mapped_range()?;
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let start = (y * padded_row) as usize;
        pixels.extend_from_slice(&data[start..start + (width * 4) as usize]);
    }
    drop(data);
    readback.unmap();

    image::save_buffer(
        out,
        &pixels,
        width,
        height,
        image::ColorType::Rgba8,
    )
    .with_context(|| format!("could not save to {out}"))?;
    println!("frame saved: {out} ({width}x{height}, t={time}, speed={speed}, notes: {})", sprites.len());
    Ok(())
}

/// Проверка экрана настроек без окна: ввод текста (пробел/символы), backspace,
/// стрелки, тумблеры, защита от игровых хоткеев и запись/чтение settings.json.
/// Исходный data/settings.json восстанавливается.
fn settings_selftest() -> Result<()> {
    use settings_screen::{Row, SettingsScreen};
    use winit::keyboard::{Key, NamedKey};

    let saved = std::fs::read_to_string("data/settings.json").ok();
    let mut init = settings::Settings::default();
    init.speed = 6.5;
    init.sanitize();
    let on = online::Online::load();
    let mut sc = SettingsScreen::new(init, &on);
    let key_char = |c: char| -> Key { Key::Character(c.to_string().as_str().into()) };

    // 1) фокус поля + печать URL с пробелом и символами
    let url = "http://example.com:8090/api?x=1&y=2 a";
    sc.select(Row::Server, false);
    sc.server.clear();
    assert!(
        sc.on_key(&Key::Named(NamedKey::Enter), false, false).is_none(),
        "Enter on a text row must focus it, not act"
    );
    for c in url.chars() {
        sc.on_key(&key_char(c), false, false);
    }
    assert_eq!(sc.server_text(), url, "server field must accept space/symbols");

    // 2) backspace
    sc.on_key(&Key::Named(NamedKey::Backspace), false, false);
    assert_eq!(sc.server_text(), &url[..url.len() - 1], "backspace");

    // 3) стрелки меняют числовое значение
    sc.select(Row::Speed, false);
    let before = sc.draft.speed;
    sc.on_key(&Key::Named(NamedKey::ArrowRight), false, false);
    assert!(
        (sc.draft.speed - (before + 0.1)).abs() < 1e-3,
        "speed must grow by 0.1 ({} -> {})",
        before,
        sc.draft.speed
    );

    // 4) тумблер по Enter
    sc.select(Row::Guides, false);
    let g0 = sc.draft.note_guides;
    sc.on_key(&Key::Named(NamedKey::Enter), false, false);
    assert_ne!(sc.draft.note_guides, g0, "guides toggle");

    // 5) игровые хоткеи (g, space, [ ]) не срабатывают, пока поле в фокусе
    sc.select(Row::Server, false);
    sc.on_key(&Key::Named(NamedKey::Enter), false, false);
    let g1 = sc.draft.note_guides;
    let speed1 = sc.draft.speed;
    for c in "g s [ ]".chars() {
        sc.on_key(&key_char(c), false, false);
    }
    sc.on_key(&Key::Named(NamedKey::Space), false, false);
    assert_eq!(sc.draft.note_guides, g1, "hotkey 'g' must not fire while typing");
    assert!(
        (sc.draft.speed - speed1).abs() < 1e-6,
        "hotkey '['/']' must not change scroll speed while typing"
    );

    // 6) применение и запись настроек на диск (с восстановлением файла)
    let mut out = sc.draft;
    out.volume = 0.35;
    out.speed = 4.2;
    out.sanitize();
    settings::save(&out);
    let loaded = settings::load();
    assert!((loaded.volume - 0.35).abs() < 1e-4, "volume must persist");
    assert!((loaded.speed - 4.2).abs() < 1e-4, "speed must persist");
    // Окна судейства и сырые X планшетов зашиты в игру и не должны попадать
    // в settings.json (старые ключи при сохранении вычищаются).
    let raw = std::fs::read_to_string("data/settings.json")?;
    for gone in ["perfect_ms", "good_ms", "tablet_x_max_gaomon", "tablet_x_max_parblo"] {
        assert!(
            !raw.contains(gone),
            "{gone} must be hard-coded, not persisted/settings-editable"
        );
    }

    match saved {
        Some(t) => std::fs::write("data/settings.json", t)?,
        None => {
            let _ = std::fs::remove_file("data/settings.json");
        }
    }
    // 7) строка «Download all packs»: Enter просит загрузку, подписи состояний
    //    считаются без сети
    use settings_screen::{Action, PacksPhase, PacksUi};
    sc.select(Row::Download, false);
    assert_eq!(
        sc.on_key(&Key::Named(NamedKey::Enter), false, false),
        Some(Action::DownloadPacks),
        "Enter on the download row must request the pack download"
    );
    // 8) «Install pack from file…»: Enter/клик/стрелки и раскладка в карточке
    //    ONLINE на 1280×720 и 1920×1200 (не залезает на аккаунт и правую колонку)
    sc.select(Row::Download, false);
    assert_eq!(
        sc.on_key(&Key::Named(NamedKey::ArrowDown), false, false),
        None,
        "DOWN only moves the selection"
    );
    assert_eq!(
        sc.on_key(&Key::Named(NamedKey::Enter), false, false),
        Some(Action::InstallPack),
        "DOWN from the download row must land on the install row"
    );
    sc.select(Row::Install, false);
    assert_eq!(
        sc.on_key(&Key::Named(NamedKey::Enter), false, false),
        Some(Action::InstallPack),
        "Enter on the install row must open the file dialog request"
    );
    for (w, h) in [(1280.0f32, 720.0f32), (1920.0, 1200.0)] {
        sc.layout_for_test(w, h, false);
        let scale = crate::theme::ui_scale(w, h);
        let card = sc.card_rect("ONLINE").expect("ONLINE card");
        let download = sc.row_rect(Row::Download).expect("download row");
        let install = sc.row_rect(Row::Install).expect("install row");
        let help = sc.card_rect("HELP").expect("HELP card");
        assert!(
            install[1] >= download[1] + download[3],
            "install row must sit below the download row at {w}x{h}"
        );
        // строка аккаунта рисуется на card.y + 326s — она должна остаться ниже
        assert!(
            install[1] + install[3] <= card[1] + 326.0 * scale,
            "install row must not cover the account line at {w}x{h}"
        );
        assert!(
            install[1] + install[3] <= card[1] + card[3],
            "install row must stay inside the ONLINE card at {w}x{h}"
        );
        assert!(
            install[0] >= card[0] && install[0] + install[2] <= card[0] + card[2],
            "install row must keep the card margins at {w}x{h}"
        );
        assert!(
            install[0] + install[2] < help[0],
            "ONLINE rows must not reach the right column at {w}x{h}"
        );
    }
    sc.layout_for_test(1280.0, 720.0, true);
    assert!(
        sc.row_rect(Row::Install).is_some(),
        "install row must stay visible when signed in"
    );
    let mut ui = PacksUi { total: Some(447), installed: 12, ..Default::default() };
    assert_eq!(ui.value(), "447 packs · 12 installed", "idle shows count + installed");
    ui.phase = PacksPhase::Running;
    ui.prog = pack_download::Progress {
        done: 12,
        skipped: 3,
        total: 447,
        bytes: 42 * 1024 * 1024,
        current: "Amber".into(),
        ..Default::default()
    };
    assert_eq!(ui.value(), "15 / 447 · 3%", "running shows progress");
    assert_eq!(ui.detail(), "Amber · 42.0 MB", "running shows current pack + MB");
    assert!((ui.bar_percent() - 15.0 * 100.0 / 447.0).abs() < 1e-3, "bar follows percent");
    ui.phase = PacksPhase::Done;
    ui.prog.done = 440;
    ui.prog.skipped = 4;
    ui.prog.failed = vec![("ice10".into(), "download: timeout".into())];
    assert_eq!(ui.value(), "444 installed · 1 failed", "done shows installed + failed");
    assert_eq!(ui.detail(), "ice10: download: timeout", "first failure reason");
    ui.phase = PacksPhase::Cancelled;
    assert_eq!(ui.value(), "cancelled · 444 installed", "cancelled shows installed");
    // переходы фазы по последнему прогрессу
    let mut fin = PacksUi::default();
    fin.finish(pack_download::Progress {
        done: 3,
        skipped: 1,
        total: 10,
        cancelled: true,
        finished: true,
        ..Default::default()
    });
    assert_eq!(fin.phase, PacksPhase::Cancelled, "cancelled progress -> Cancelled");
    assert_eq!(fin.value(), "cancelled · 4 installed");
    fin.finish(pack_download::Progress {
        done: 9,
        skipped: 1,
        total: 10,
        finished: true,
        ..Default::default()
    });
    assert_eq!(fin.phase, PacksPhase::Done, "finished progress -> Done");
    assert_eq!(fin.value(), "10 installed · 0 failed");
    println!(
        "settings self-test: OK (typing, backspace, adjust, toggle, hotkey guard, persistence, pack download row, install row)"
    );
    Ok(())
}

/// Проверка поиска в библиотеке без окна: пробел, пунктуация, backspace,
/// русский ввод и лимит длины. Раньше пробел уходил в `toggle_pause`, а все
/// не-буквенные символы отбрасывались фильтром `is_alphanumeric`.
fn search_selftest() -> Result<()> {
    use crate::app::{library_search_input, library_search_key, push_search_text, SEARCH_MAX_CHARS};
    use winit::keyboard::{Key, NamedKey};

    let ch = |c: char| -> Key { Key::Character(c.to_string().as_str().into()) };

    // 1) Space приходит как NamedKey — в библиотеке он обязан попасть в поиск,
    //    а не в `toggle_pause` (это и был баг: «пробел не печатается»).
    let space = Key::Named(NamedKey::Space);
    assert_eq!(
        library_search_input(&space),
        Some(" ".to_string()),
        "space must be text input in the library search"
    );
    let mut search = String::new();
    assert!(
        library_search_key(&mut search, &space, true),
        "space must be consumed as text while the library is open"
    );
    assert_eq!(search, " ", "space must reach the query");
    // Вне библиотеки та же клавиша остаётся хоткеем паузы.
    let mut outside = String::new();
    assert!(
        !library_search_key(&mut outside, &space, false),
        "outside the library space must stay a playback hotkey"
    );
    assert!(outside.is_empty(), "hotkey must not write to the search");

    // 2) Служебные клавиши остаются хоткеями (выход из поиска/выбор песни).
    for named in [NamedKey::Backspace, NamedKey::Enter, NamedKey::Escape, NamedKey::ArrowLeft] {
        let key = Key::Named(named);
        assert_eq!(
            library_search_input(&key),
            None,
            "named key {named:?} must stay a shortcut"
        );
        let mut empty = String::new();
        assert!(
            !library_search_key(&mut empty, &key, true),
            "named key {named:?} must not be swallowed as text"
        );
    }

    // 3) Пробел и пунктуация печатаются целиком (раньше фильтр `is_alphanumeric`
    //    выбрасывал «!», «#», «(», «%», «[», «]» и всю остальную пунктуацию).
    // кириллица через кодпоинты: тот же многобайтный путь, что и при вводе
    let cyrillic = "\u{43f}\u{435}\u{441}\u{43d}\u{44f} \u{43d}\u{43e}\u{43c}\u{435}\u{440}";
    for text in ["long summer", "a!b#c(d)", "x%y^z&w*i", "[ ] ' . -", cyrillic] {
        let mut search = String::new();
        for c in text.chars() {
            let key = if c == ' ' {
                Key::Named(NamedKey::Space)
            } else {
                ch(c)
            };
            assert!(
                library_search_key(&mut search, &key, true),
                "typing {c:?} must be consumed by the search"
            );
        }
        assert_eq!(search, text, "search must keep exactly what was typed");
    }

    // 4) Лимит длины: лишние символы не пишутся, но клавиша всё равно наша
    //    (иначе на пределе начинали срабатывать игровые хоткеи).
    let mut search = String::new();
    let long: String = "a".repeat(SEARCH_MAX_CHARS + 5);
    for c in long.chars() {
        assert!(push_search_text(&mut search, &c.to_string()), "at cap the key is still consumed");
    }
    assert_eq!(search.chars().count(), SEARCH_MAX_CHARS, "query length is capped");

    // 5) Управляющие символы в поиск не попадают.
    let mut search = String::new();
    assert!(!push_search_text(&mut search, "\u{1b}"), "control char is not text input");
    assert!(search.is_empty(), "control char must not be appended");

    println!("search self-test: OK (space, punctuation, cyrillic, cap, control chars)");
    Ok(())
}

/// Живой тест «скачать все паки» (нужна сеть): качаем несколько паков с
/// настоящего сервера во ВРЕМЕННУЮ папку, проверяем распаковку и манифесты,
/// затем второй прогон — он должен пропустить уже установленные папки.
/// Каталог `data/imported` игры не трогается.
fn packs_selftest() -> Result<()> {
    use pack_download::{install_all, Progress};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Instant;

    /// Один прогон install_all: отменяем после `stop` установок (`by_skips` —
    /// считать пропуски вместо скачанных). Возвращает итоговый прогресс и время.
    fn run(server: &str, dir: &std::path::Path, stop: usize, by_skips: bool) -> (Progress, f64) {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let c = cancel.clone();
        let srv = server.to_string();
        let d = dir.to_path_buf();
        let t0 = Instant::now();
        let handle = std::thread::spawn(move || install_all(&srv, &d, c, Some(tx)));
        let mut last = Progress::default();
        while !last.finished {
            let Ok(p) = rx.recv() else { break };
            last = p;
            let n = if by_skips { last.skipped } else { last.unpacked };
            if n >= stop {
                cancel.store(true, Ordering::Relaxed);
            }
        }
        let _ = handle.join();
        (last, t0.elapsed().as_secs_f64())
    }

    let server = std::env::var("DEEMO_SERVER")
        .unwrap_or_else(|_| online::DEFAULT_SERVER.to_string());
    // Пак-по-паку путь: быстрый и детерминированный (бандл на 1.3 ГБ в
    // самотесте гонять бессмысленно — он целиком покрыт cargo-тестами).
    // Хочешь проверить именно бандл: DEEMO_PACK_BUNDLE=1 перед запуском.
    if std::env::var("DEEMO_PACK_BUNDLE").is_err() {
        // SAFETY: однопоточная инициализация до старта фоновых потоков.
        unsafe { std::env::set_var("DEEMO_PACK_BUNDLE", "0") };
    }
    let root = std::env::temp_dir().join(format!("deemo-packs-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    println!("packs self-test: server {server}");
    println!("packs self-test: temp import dir {}", root.display());

    // --- прогон 1: три пака, затем отмена ---
    let (p1, dt1) = run(&server, &root, 3, false);
    println!(
        "packs self-test: run 1 - total {} done {} skipped {} failed {} bytes {} in {dt1:.1}s (cancelled: {})",
        p1.total,
        p1.done,
        p1.skipped,
        p1.failed.len(),
        p1.bytes,
        p1.cancelled
    );
    for (id, why) in &p1.failed {
        println!("packs self-test:   failed {id}: {why}");
    }
    assert!(p1.cancelled, "run 1 must end cancelled by the flag");
    assert!(p1.unpacked >= 3, "expected at least 3 installed packs, got {}", p1.unpacked);
    assert!(p1.bytes > 0, "no bytes reported");

    // --- проверяем, что пак реально лёг как data/imported/<id>/pack.json + чарты + аудио ---
    let mut dirs = 0usize;
    let mut charts = 0usize;
    let mut audio = 0usize;
    let mut titles: Vec<String> = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&root)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !n.starts_with('.'))
                .unwrap_or(false)
        })
        .collect();
    entries.sort();
    for dir in &entries {
        dirs += 1;
        assert!(dir.join("pack.json").is_file(), "no pack.json in {}", dir.display());
        let m = crate::pack::manifest_of(dir)
            .with_context(|| format!("manifest_of({}) failed", dir.display()))?;
        assert!(!m.charts.is_empty(), "pack {} has no charts", m.id);
        for c in &m.charts {
            assert!(
                dir.join(&c.path).is_file(),
                "chart {} missing in {}",
                c.path,
                dir.display()
            );
            charts += 1;
        }
        if let Some(a) = &m.audio {
            assert!(dir.join(a).is_file(), "audio {a} missing in {}", dir.display());
            audio += 1;
        }
        titles.push(format!("{} ({})", m.title, m.id));
    }
    println!(
        "packs self-test: run 1 verified {dirs} packs, {charts} charts, {audio} audio files: {:?}",
        titles
    );
    assert_eq!(dirs, p1.unpacked + p1.skipped, "every unpacked pack must land as a folder");
    assert!(charts >= 3, "expected charts for every pack");
    assert!(audio >= 1, "expected audio files");

    // временных папок не осталось
    let hidden = std::fs::read_dir(&root)?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .count();
    assert_eq!(hidden, 0, "temp install dirs left behind");

    // --- прогон 2: те же паки должны пропуститься ---
    let (p2, dt2) = run(&server, &root, 3, true);
    println!(
        "packs self-test: run 2 - total {} done {} skipped {} failed {} in {dt2:.1}s",
        p2.total,
        p2.done,
        p2.skipped,
        p2.failed.len()
    );
    assert!(p2.skipped >= 3, "second run must skip installed packs, got {}", p2.skipped);
    assert!(p2.done <= 2, "second run must not re-download installed packs ({})", p2.done);
    for dir in &entries {
        assert!(dir.join("pack.json").is_file(), "pack vanished: {}", dir.display());
        assert!(crate::pack::manifest_of(dir).is_some(), "manifest broke: {}", dir.display());
    }

    let (part, part_etag) = pack_download::bundle_paths(&root);
    let _ = std::fs::remove_file(&part);
    let _ = std::fs::remove_file(&part_etag);
    let _ = std::fs::remove_dir_all(&root);
    println!(
        "packs self-test: OK ({dirs} packs, {} bytes, {:.1}s + {dt2:.1}s, skipped on rerun {})",
        p1.bytes, dt1, p2.skipped
    );
    Ok(())
}

/// Живой тест инбокса `data/inbox`: настоящий `.deemopack` копируется во
/// временный инбокс, один проход наблюдателя ставит его в temp-import,
/// манифест и чарты парсятся, источник уезжает в `_installed`. Битый файл —
/// в `_failed`, посторонние файлы не трогаются. `data/` игры не меняется.
fn inbox_test(pack: Option<String>) -> Result<()> {
    use crate::pack_import::{
        ensure_inbox, file_pack_id, Inbox, InboxEvent, INBOX_FAILED, INBOX_INSTALLED,
    };
    use std::path::PathBuf;

    fn first_pack_in(dir: &std::path::Path) -> Option<PathBuf> {
        let rd = std::fs::read_dir(dir).ok()?;
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("deemopack"))
                    .unwrap_or(false)
            })
            .collect();
        v.sort();
        v.into_iter().next()
    }

    fn real_pack() -> Option<PathBuf> {
        for dir in [".work/packs", "../.work/packs"] {
            if let Some(p) = first_pack_in(std::path::Path::new(dir)) {
                return Some(p);
            }
        }
        None
    }

    let source = match pack {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_dir() {
                first_pack_in(&p).ok_or_else(|| {
                    anyhow::anyhow!("no .deemopack in {}", p.display())
                })?
            } else {
                p
            }
        }
        None => real_pack().ok_or_else(|| {
            anyhow::anyhow!("no .deemopack in .work/packs - pass a path: --inbox-test <pack>")
        })?,
    };
    if !source.is_file() {
        anyhow::bail!("pack not found: {}", source.display());
    }
    println!("inbox self-test: source {}", source.display());

    let root = std::env::temp_dir().join(format!("deemo-inbox-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let inbox = root.join("inbox");
    let import = root.join("imported");
    ensure_inbox(&inbox)?;
    std::fs::create_dir_all(&import)?;
    println!("inbox self-test: temp inbox {}", inbox.display());
    println!("inbox self-test: temp import {}", import.display());

    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let placed = inbox.join(&name);
    std::fs::copy(&source, &placed)?;

    // один проход наблюдателя (immediate: без ожидания «докачки»)
    let mut ib = Inbox::immediate(&inbox, &import);
    let events = ib.scan();
    if events.len() != 1 {
        anyhow::bail!("expected one inbox event, got {events:?}");
    }
    println!("inbox self-test: {}", events[0].toast());
    let InboxEvent::Installed { name: moved, title } = &events[0] else {
        anyhow::bail!("expected Installed, got {:?}", events[0]);
    };
    assert_eq!(moved, &name, "the archive keeps its name under _installed");
    assert!(!placed.exists(), "installed source must leave the inbox");
    assert!(
        inbox.join(INBOX_INSTALLED).join(&name).is_file(),
        "_installed must keep the archive"
    );

    // то же id, что при drag & drop файла
    let id = file_pack_id(&source);
    let dir = import.join(&id);
    assert!(dir.join("pack.json").is_file(), "no pack.json in {}", dir.display());
    let manifest = crate::pack::manifest_of(&dir)
        .ok_or_else(|| anyhow::anyhow!("manifest_of({}) failed", dir.display()))?;
    assert!(!manifest.charts.is_empty(), "pack {} has no charts", manifest.id);
    for c in &manifest.charts {
        assert!(dir.join(&c.path).is_file(), "chart {} missing", c.path);
    }
    assert!(
        manifest.title.starts_with(title.trim_end_matches('\u{2026}')) || title == &manifest.id,
        "toast title {title:?} should come from manifest {:?}",
        manifest.title
    );
    println!(
        "inbox self-test: id {} title {:?} charts {}",
        manifest.id,
        manifest.title,
        manifest.charts.len()
    );

    let installed_before = std::fs::read_dir(&import)?.count();

    // боевой путь: фоновый поток (два опроса на «устоявшийся» файл)
    let live_inbox = root.join("live-inbox");
    let live_import = root.join("live-import");
    ensure_inbox(&live_inbox)?;
    std::fs::create_dir_all(&live_import)?;
    std::fs::copy(&source, live_inbox.join(&name))?;
    let started = std::time::Instant::now();
    let rx = crate::pack_import::spawn_inbox_watcher_in(&live_inbox, &live_import);
    let ev = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|e| anyhow::anyhow!("inbox watcher did not report: {e}"))?;
    let elapsed = started.elapsed();
    println!("inbox self-test: watcher {} after {:.1}s", ev.toast(), elapsed.as_secs_f64());
    assert!(
        matches!(ev, InboxEvent::Installed { .. }),
        "watcher must install a real pack, got {ev:?}"
    );
    assert!(
        elapsed >= crate::pack_import::INBOX_POLL.mul_f32(0.9),
        "watcher must settle for at least one poll ({elapsed:?})"
    );
    assert!(
        live_import.join(&id).join("pack.json").is_file(),
        "watcher must install into the given import dir"
    );
    assert!(
        live_inbox.join(INBOX_INSTALLED).join(&name).is_file(),
        "watcher must move the source to _installed"
    );
    drop(rx);
    let _ = std::fs::remove_dir_all(&live_inbox);
    let _ = std::fs::remove_dir_all(&live_import);

    // битый файл: ошибка, источник уезжает в _failed, в импорте ничего нового
    let bogus = inbox.join("broken pack.deemopack");
    std::fs::write(&bogus, b"definitely not a zip")?;
    let events = ib.scan();
    if events.len() != 1 {
        anyhow::bail!("expected one failure event, got {events:?}");
    }
    println!("inbox self-test: {}", events[0].toast());
    let InboxEvent::Failed { name, reason } = &events[0] else {
        anyhow::bail!("expected Failed, got {:?}", events[0]);
    };
    assert_eq!(name, "broken pack.deemopack");
    assert!(reason.starts_with("Install failed:"), "toast: {reason}");
    assert!(!bogus.exists(), "failed source must leave the inbox");
    assert!(
        inbox.join(INBOX_FAILED).join("broken pack.deemopack").is_file(),
        "_failed must keep the broken file"
    );
    assert_eq!(
        std::fs::read_dir(&import)?.count(),
        installed_before,
        "a failed install must not add anything to the import dir"
    );
    // и повторно он не ставится (файл уже не в инбоксе)
    assert!(ib.scan().is_empty(), "no retry loop after a failure");

    // посторонние файлы игнорируются: ни установок, ни тостов
    std::fs::write(inbox.join("notes.txt"), b"hello")?;
    std::fs::write(inbox.join(".hidden.deemopack"), b"x")?;
    assert!(ib.scan().is_empty(), "non-pack files must be ignored");
    assert!(inbox.join("notes.txt").is_file(), "notes must stay where they are");
    assert!(
        inbox.join(".hidden.deemopack").is_file(),
        "hidden files must stay where they are"
    );

    // временных папок установки не осталось
    let hidden = std::fs::read_dir(&import)?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .count();
    assert_eq!(hidden, 0, "temp install dirs left behind");

    let _ = std::fs::remove_dir_all(&root);
    println!("inbox self-test: OK");
    Ok(())
}

/// Живой тест drag & drop: установить настоящий `.deemopack` во временную
/// папку импорта, проверить манифест и чарты, затем убедиться, что битый файл
/// и неподдерживаемый тип отвергаются без следов. `data/imported` не трогается.
fn drop_selftest(path: &str) -> Result<()> {
    use std::path::Path;

    let root = std::env::temp_dir().join(format!("deemo-drop-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let import = root.join("imported");
    println!("drop self-test: source {}", path);
    println!("drop self-test: temp import dir {}", import.display());

    let title = crate::pack_import::install_dropped(Path::new(path), &import)
        .map_err(|e| anyhow::anyhow!("install failed: {e}"))?;
    println!("drop self-test: installed title {title:?}");

    let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&import)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert_eq!(dirs.len(), 1, "expected exactly one installed pack dir, got {dirs:?}");
    let dir = &dirs[0];
    assert!(dir.join("pack.json").is_file(), "pack.json missing in {}", dir.display());
    let manifest = crate::pack::manifest_of(dir)
        .ok_or_else(|| anyhow::anyhow!("manifest_of({}) failed", dir.display()))?;
    assert!(!manifest.charts.is_empty(), "pack {} has no charts", manifest.id);
    for c in &manifest.charts {
        assert!(dir.join(&c.path).is_file(), "chart {} missing", c.path);
    }
    println!(
        "drop self-test: id {} title {:?} charts {}",
        manifest.id,
        manifest.title,
        manifest.charts.len()
    );

    // битый .deemopack: ошибка и ни одного нового каталога/временных следов
    let before: Vec<_> = std::fs::read_dir(&import)?.flatten().map(|e| e.file_name()).collect();
    let bogus = root.join("broken.deemopack");
    std::fs::write(&bogus, b"definitely not a zip")?;
    let err = crate::pack_import::install_dropped(&bogus, &import).unwrap_err();
    assert!(err.starts_with("Install failed:"), "unexpected toast: {err}");
    let after: Vec<_> = std::fs::read_dir(&import)?.flatten().map(|e| e.file_name()).collect();
    assert_eq!(before, after, "bogus drop must not leave anything behind");
    println!("drop self-test: bogus rejected: {err}");

    // неподдерживаемый тип
    let txt = root.join("song.txt");
    std::fs::write(&txt, b"hello")?;
    let err = crate::pack_import::install_dropped(&txt, &import).unwrap_err();
    assert_eq!(err, "Unsupported file: expected .deemopack");
    println!("drop self-test: unsupported rejected: {err}");

    let _ = std::fs::remove_dir_all(&root);
    println!("drop self-test: OK");
    Ok(())
}

/// Живой тест очереди рекордов: запись на диск → недоступный сервер (остаётся)
/// → тот же адрес поднялся (2xx) → удалена и пришло событие. Использует
/// временный файл очереди, `data/online_queue.json` игры не трогается.
fn queue_selftest() -> Result<()> {
    use crate::online_queue::{load_entries, OnlineQueue, PendingScore, QueueEvent};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    fn wait(q: &OnlineQueue, timeout: Duration) -> Option<QueueEvent> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(ev) = q.poll() {
                return Some(ev);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    let root = std::env::temp_dir().join(format!("deemo-queue-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let path = root.join("online_queue.json");
    println!("queue self-test: temp queue {}", path.display());

    // свободный порт: подержали listener и отпустили — сервера по адресу нет
    let port = {
        let l = TcpListener::bind("127.0.0.1:0")?;
        l.local_addr()?.port()
    };
    let server = format!("http://127.0.0.1:{port}");
    let body = serde_json::json!({
        "song": "zzqueueprobe", "diff": "hard", "score": 424242,
        "accuracy": 50.0, "max_combo": 1, "perfect": 1, "good": 0, "miss": 0,
        "rank": "D", "full_combo": false, "speed": 4.0,
        "timestamp": 1_794_940_377u64,
    });
    let rec = PendingScore::new(
        &server,
        "selftest-token",
        body.as_object().cloned().unwrap_or_default(),
    );
    let mut q = OnlineQueue::open(&path);
    let seq = q.enqueue(rec.clone(), &server);
    let on_disk = load_entries(&path);
    assert_eq!(on_disk.len(), 1, "queue must persist the record before any network");
    println!("queue self-test: queued before upload (file has {} record)", on_disk.len());

    let ev = wait(&q, Duration::from_secs(10)).expect("worker event");
    assert_eq!(ev.seq, seq);
    assert_eq!((ev.uploaded, ev.failed), (0, 1), "unreachable server must not upload");
    assert!(q.contains(&rec), "failed record must stay queued");
    assert_eq!(load_entries(&path), vec![rec.clone()], "failed record stays in the file");
    let toast = crate::app::queue_toast(true, ev.uploaded, q.contains(&rec)).unwrap_or("");
    println!("queue self-test: server down -> {toast:?}, still queued");

    // тот же адрес теперь отвечает 2xx
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let seen = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut sock, _) = listener.accept().expect("accept");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match sock.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .to_ascii_lowercase()
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 29\r\nConnection: close\r\n\r\n{\"record\":true,\"best\":424242}";
        let _ = sock.write_all(resp.as_bytes());
        String::from_utf8_lossy(&buf).to_string()
    });

    let seq2 = q.flush(&server);
    let ev2 = wait(&q, Duration::from_secs(10)).expect("worker event");
    assert_eq!(ev2.seq, seq2);
    assert_eq!((ev2.uploaded, ev2.failed), (1, 0), "2xx must remove the record");
    assert!(!q.contains(&rec));
    assert!(load_entries(&path).is_empty(), "queue file must be empty");
    let req = seen.join().unwrap();
    assert!(req.starts_with("POST /api/scores HTTP/1.1"), "request: {req}");
    assert!(req.to_ascii_lowercase().contains("authorization: bearer selftest-token"));
    assert!(req.contains("\"song\":\"zzqueueprobe\""));
    let toast = crate::app::queue_toast(false, ev2.uploaded, false).unwrap_or("");
    println!("queue self-test: upload ok -> {toast:?}, entry removed");

    let _ = std::fs::remove_dir_all(&root);
    println!("queue self-test: OK");
    Ok(())
}

/// Windows release: GUI-подсистема не даёт консоли, а `println!`/паника на
/// невалидном stdout могут уронить игру. Уводим оба стандартных потока в
/// `deemo.log` рядом с exe. Для `--tablet-probe` дополнительно пробуем
/// прицепиться к консоли cmd: сам probe пишет в `CONOUT$` отдельно, поэтому
/// редирект stdout ему не мешает.
#[cfg(all(windows, not(debug_assertions)))]
fn init_release_logging(probe: bool) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        AttachConsole, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };

    if probe {
        // cmd/PowerShell: даём probe шанс увидеть консоль родителя.
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
    let exe_log = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("deemo.log")));
    let candidates: Vec<std::path::PathBuf> = exe_log
        .into_iter()
        .chain(std::iter::once(std::env::temp_dir().join("deemo.log")))
        .collect();
    let Some((path, file)) = candidates.into_iter().find_map(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .ok()
            .map(|f| (path, f))
    }) else {
        return;
    };
    let handle = file.as_raw_handle() as HANDLE;
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, handle);
        SetStdHandle(STD_ERROR_HANDLE, handle);
    }
    // Хэндл должен жить весь процесс.
    std::mem::forget(file);
    println!(
        "deemo: windows release, log {}",
        path.display()
    );
    println!(
        "deemo: cwd {}",
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<{e}>"))
    );
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(all(windows, not(debug_assertions)))]
    init_release_logging(args.iter().any(|a| a == "--tablet-probe"));
    let get = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    // Диагностика планшетов: окно/GPU не нужны, лог пишется рядом с exe.
    if let Some(i) = args.iter().position(|a| a == "--tablet-probe") {
        let seconds = args
            .get(i + 1)
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(10.0)
            .clamp(0.5, 120.0);
        return tablet::probe(seconds);
    }

    if args.iter().any(|a| a == "--settings-selftest") {
        return settings_selftest();
    }

    if args.iter().any(|a| a == "--search-selftest") {
        return search_selftest();
    }

    if args.iter().any(|a| a == "--packs-selftest") {
        return packs_selftest();
    }

    if let Some(path) = get("--drop-test") {
        return drop_selftest(&path);
    }

    if args.iter().any(|a| a == "--inbox-test") {
        let pack = get("--inbox-test").filter(|p| !p.starts_with('-'));
        return inbox_test(pack);
    }

    if let Some(dir) = get("--pack-info") {
        let path = std::path::Path::new(&dir);
        match crate::pack::manifest_of(path) {
            Some(m) => {
                println!("id: {}", m.id);
                println!("title: {:?} (chars: {})", m.title, m.title.chars().count());
                println!("composer: {:?}", m.composer);
                println!("album: {:?}", m.album);
                println!("levels: {:?}", m.levels);
                println!("charts: {:?}", m.charts.iter().map(|c| (&c.diff, &c.path)).collect::<Vec<_>>());
                println!("audio: {:?}", m.audio);
                println!("preview: {:?}", m.preview);
                println!("cover: {:?}", m.cover.map(|c| path.join(c).exists()));
                println!("duration: {}", m.duration);
            }
            None => println!("manifest_of -> None (pack.json не разобран)"),
        }
        return Ok(());
    }

    if args.iter().any(|a| a == "--queue-selftest") {
        return queue_selftest();
    }

    if let Some(out) = get("--shot") {
        let width: u32 = get("--width").and_then(|v| v.parse().ok()).unwrap_or(1280);
        let height: u32 = get("--height").and_then(|v| v.parse().ok()).unwrap_or(720);
        if let Some(screen) = get("--screen") {
            let demo = args.iter().any(|a| a == "--demo");
            let packs = get("--packs");
            return app::shot_screen(&out, &screen, demo, width, height, packs.as_deref());
        }
        let chart_path = get("--chart").unwrap_or_else(|| "data/charts/12thchance/hard.json".into());
        let time: f32 = get("--time").and_then(|v| v.parse().ok()).unwrap_or(3.0);
        let speed: f32 = get("--speed").and_then(|v| v.parse().ok()).unwrap_or(4.0);
        let autohit = args.iter().any(|a| a == "--autohit");
        let replay_path = get("--replay");
        let replay_nick = get("--replay-nick").unwrap_or_else(|| "player".to_string());
        return shot(
            &out,
            &chart_path,
            time,
            speed,
            width,
            height,
            autohit,
            replay_path.as_deref(),
            &replay_nick,
        );
    }

    if args.iter().any(|a| a == "--x11") {
        // SAFETY: до старта любых потоков/OK: однопоточная инициализация.
        unsafe { std::env::set_var("DEEMO_X11", "1") };
    }
    if args.iter().any(|a| a == "--backend") {
        unsafe { std::env::set_var("DEEMO_BACKEND_ONLY", "1") };
    }

    app::run()
}
