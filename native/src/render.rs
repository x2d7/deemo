//! Спрайтовый рендерер на wgpu: один пайплайн, батчи по текстуре.

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

use crate::stage::{Sprite, Tex};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

const SHADER: &str = r#"
struct Uniforms { mvp: mat4x4<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsIn {
  @location(0) pos: vec3<f32>,
  @location(1) uv: vec2<f32>,
  @location(2) color: vec4<f32>,
};
struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(i: VsIn) -> VsOut {
  var o: VsOut;
  o.clip = u.mvp * vec4<f32>(i.pos, 1.0);
  o.uv = i.uv;
  o.color = i.color;
  return o;
}

@fragment
fn fs_main(i: VsOut) -> @location(0) vec4<f32> {
  let c = textureSample(tex, samp, i.uv);
  return vec4<f32>(c.rgb * i.color.rgb, c.a * i.color.a);
}

@fragment
fn fs_additive(i: VsOut) -> @location(0) vec4<f32> {
  let c = textureSample(tex, samp, i.uv);
  let a = c.a * i.color.a;
  return vec4<f32>(c.rgb * i.color.rgb * a, a);
}
"#;

/// Все файловые текстуры (включая 15 кадров взрыва). White генерируется отдельно.
fn tex_list() -> Vec<(Tex, String)> {
    let s = |t: Tex, p: &str| (t, p.to_string());
    let mut v = vec![
        s(Tex::Bg, "data/sprites/stage/bg.png"),
        s(Tex::BgMask, "data/sprites/stage/GP_bgrmask.png"),
        s(Tex::JudgeLine, "data/sprites/stage/GP_judgeline.png"),
        s(Tex::JudgeLineEf, "data/sprites/stage/GP_judgeline_ef.png"),
        s(Tex::Stave, "data/sprites/stage/GP_stave.png"),
        s(Tex::Line, "data/sprites/stage/GP_line.png"),
        s(Tex::BlackNote, "data/sprites/stage/black_note.png"),
        s(Tex::SlideNote, "data/sprites/stage/slide_note.png"),
        s(Tex::NoSoundNote, "data/sprites/stage/nosound_note.png"),
        s(Tex::HoldBody, "data/sprites/stage/holdbody.png"),
        s(Tex::Glow, "data/sprites/stage/effects/GPeffect_wave1.png"),
        s(Tex::Circle, "data/sprites/stage/effects/shockwave.png"),
        s(Tex::Wave, "data/sprites/stage/effects/shockwave3.png"),
    ];
    for i in 0..15u8 {
        v.push((
            Tex::Explosion(i),
            format!("data/sprites/stage/effects/note_explosion{:02}.png", i + 1),
        ));
    }
    v
}

/// Слой, который рисуется в экранных координатах (фон, маска).
pub struct ScreenQuad {
    pub tex: Tex,
    pub color: [f32; 4],
}

/// UI-квад в пикселях (текст, обложки). rect = [x, y, w, h], uv = [u0, v0, u1, v1].
pub struct UiQuad {
    pub tex: usize,
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    pub color: [f32; 4],
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    pipeline_additive: wgpu::RenderPipeline,
    ubuf_3d: wgpu::Buffer,
    ubuf_bg: wgpu::Buffer,
    ubuf_ui: wgpu::Buffer,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    groups: HashMap<Tex, wgpu::BindGroup>,
    screen_groups: HashMap<Tex, wgpu::BindGroup>,
    ui_groups: Vec<wgpu::BindGroup>,
    /// Текстуры UI в том же порядке, что и `ui_groups`: нужны, чтобы догружать
    /// глифы в атлас шрифта без пересоздания bind group.
    ui_textures: Vec<wgpu::Texture>,
    vbuf: Option<wgpu::Buffer>,
    vbuf_cap: usize,
}

impl Renderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat) -> Result<Self> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprite"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let make_pipeline = |blend: wgpu::BlendState, fs: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("sprite-pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x3,
                            1 => Float32x2,
                            2 => Float32x4
                        ],
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = make_pipeline(wgpu::BlendState::ALPHA_BLENDING, "fs_main");
        let pipeline_additive = make_pipeline(wgpu::BlendState::ADDITIVE, "fs_additive");

        let ubuf_3d = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mvp-3d"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ubuf_bg = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mvp-screen"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ident: crate::math::Mat4 = crate::math::mat4_identity();
        queue.write_buffer(&ubuf_bg, 0, bytemuck::cast_slice(&[ident]));
        let ubuf_ui = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mvp-ui"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ubuf_ui, 0, bytemuck::cast_slice(&[ident]));

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sprite-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let mut groups = HashMap::new();
        let mut screen_groups = HashMap::new();

        let upload = |label: &str, w: u32, h: u32, data: &[u8]| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * w),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            texture.create_view(&wgpu::TextureViewDescriptor::default())
        };

        let bind = |view: &wgpu::TextureView, ubuf: &wgpu::Buffer, label: &str| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        };

        for (t, path) in tex_list() {
            let img = image::open(&path).with_context(|| format!("no texture {path}"))?;
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let view = upload(&path, w, h, &rgba);
            groups.insert(t, bind(&view, &ubuf_3d, &path));
            if matches!(t, Tex::Bg | Tex::BgMask) {
                screen_groups.insert(t, bind(&view, &ubuf_bg, &path));
            }
        }

        // белый пиксель для линий
        let white = [255u8, 255, 255, 255];
        let view = upload("white", 1, 1, &white);
        groups.insert(Tex::White, bind(&view, &ubuf_3d, "white"));

        // градиентная направляющая засечки: яркая у линии (низ), затухает вдаль (верх)
        const MW: u32 = 8;
        const MH: u32 = 128;
        let mut marker = vec![0u8; (MW * MH * 4) as usize];
        for y in 0..MH {
            let vert = (y as f32 / (MH - 1) as f32).powf(1.6);
            for x in 0..MW {
                let hx = (1.0 - ((x as f32 - 3.5) / 4.0).abs()).clamp(0.0, 1.0);
                let a = (vert * hx * 255.0) as u8;
                let idx = ((y * MW + x) * 4) as usize;
                marker[idx] = 255;
                marker[idx + 1] = 255;
                marker[idx + 2] = 255;
                marker[idx + 3] = a;
            }
        }
        let view = upload("marker", MW, MH, &marker);
        groups.insert(Tex::Marker, bind(&view, &ubuf_3d, "marker"));

        Ok(Renderer {
            device,
            queue,
            format,
            pipeline,
            pipeline_additive,
            ubuf_3d,
            ubuf_bg,
            ubuf_ui,
            bgl,
            sampler,
            groups,
            screen_groups,
            ui_groups: Vec::new(),
            ui_textures: Vec::new(),
            vbuf: None,
            vbuf_cap: 0,
        })
    }

    /// Ортографическая проекция для UI: пиксели (0..w, 0..h, слева-сверху) → NDC.
    pub fn set_ui_size(&self, w: u32, h: u32) {
        let w = w.max(1) as f32;
        let h = h.max(1) as f32;
        let mut m = crate::math::mat4_identity();
        m[0][0] = 2.0 / w;
        m[1][1] = -2.0 / h;
        m[3][0] = -1.0;
        m[3][1] = 1.0;
        self.queue
            .write_buffer(&self.ubuf_ui, 0, bytemuck::cast_slice(&[m]));
    }

    /// Создать UI-текстуру (RGBA) и bind group для неё.
    fn make_ui_texture(
        &self,
        label: &str,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> (wgpu::Texture, wgpu::BindGroup) {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w.max(1)),
                rows_per_image: Some(h.max(1)),
            },
            wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.ubuf_ui.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        (texture, group)
    }

    /// Загрузить RGBA-текстуру для UI (шрифт, обложка). Возвращает handle.
    pub fn add_ui_texture(&mut self, label: &str, w: u32, h: u32, rgba: &[u8]) -> usize {
        let (texture, group) = self.make_ui_texture(label, w, h, rgba);
        self.ui_textures.push(texture);
        self.ui_groups.push(group);
        self.ui_groups.len() - 1
    }

    /// Зарезервировать handle под UI-текстуру, не занимая память: до первого
    /// [`Renderer::replace_ui_texture`] это пустой пиксель. Нужно атласу шрифта:
    /// страницы создаются по мере надобности, а индексы квадов известны заранее.
    pub fn reserve_ui_texture(&mut self, label: &str) -> usize {
        self.add_ui_texture(label, 1, 1, &[0, 0, 0, 0])
    }

    /// Заменить содержимое зарезервированного UI-handle текстурой нужного размера.
    pub fn replace_ui_texture(&mut self, slot: usize, label: &str, w: u32, h: u32, rgba: &[u8]) {
        if slot >= self.ui_groups.len() {
            return;
        }
        let (texture, group) = self.make_ui_texture(label, w, h, rgba);
        self.ui_textures[slot] = texture;
        self.ui_groups[slot] = group;
    }

    /// Догрузить прямоугольный кусок в существующую UI-текстуру (атлас глифов).
    pub fn update_ui_texture(&self, slot: usize, x: u32, y: u32, w: u32, h: u32, rgba: &[u8]) {
        let Some(texture) = self.ui_textures.get(slot) else {
            return;
        };
        if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
            return;
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Отрисовка в оффскрин-текстуру (для скриншотов).
    pub fn offscreen(
        &mut self,
        w: u32,
        h: u32,
        sprites: &[Sprite],
        screen: &[ScreenQuad],
        ui: &[UiQuad],
    ) -> wgpu::Texture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("offscreen") });
        self.draw(&mut enc, &view, sprites, screen, ui);
        self.queue.submit(Some(enc.finish()));
        texture
    }

    /// Сохранить текстуру в PNG (readback).
    pub fn save_png(&self, texture: &wgpu::Texture, w: u32, h: u32, out: &str) -> anyhow::Result<()> {
        let padded_row = (w * 4).div_ceil(256) * 256;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded_row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded_row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) })?;
        rx.recv()??;
        let data = slice.get_mapped_range()?;
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let start = (y * padded_row) as usize;
            pixels.extend_from_slice(&data[start..start + (w * 4) as usize]);
        }
        drop(data);
        readback.unmap();
        image::save_buffer(out, &pixels, w, h, image::ColorType::Rgba8)?;
        Ok(())
    }

    pub fn set_camera(&self, mvp: &crate::math::Mat4) {
        self.queue
            .write_buffer(&self.ubuf_3d, 0, bytemuck::cast_slice(&[*mvp]));
    }

    fn push_screen(&self, verts: &mut Vec<Vertex>, color: [f32; 4]) {
        // uv: (0,0) — левый верх текстуры; NDC y=+1 — верх экрана
        let corners = [
            ([-1.0f32, 1.0f32], [0.0f32, 0.0f32]),
            ([1.0, 1.0], [1.0, 0.0]),
            ([1.0, -1.0], [1.0, 1.0]),
            ([-1.0, 1.0], [0.0, 0.0]),
            ([1.0, -1.0], [1.0, 1.0]),
            ([-1.0, -1.0], [0.0, 1.0]),
        ];
        for (p, uv) in corners {
            verts.push(Vertex {
                pos: [p[0], p[1], 0.0],
                uv,
                color,
            });
        }
    }

    fn push_sprite(&self, verts: &mut Vec<Vertex>, s: &Sprite) {
        let px = s.pivot[0];
        let py = s.pivot[1];
        let corner = |sx: f32, sy: f32| -> [f32; 3] {
            [
                s.origin[0]
                    + s.right[0] * s.size[0] * (sx - px)
                    + s.up[0] * s.size[1] * (sy - py),
                s.origin[1]
                    + s.right[1] * s.size[0] * (sx - px)
                    + s.up[1] * s.size[1] * (sy - py),
                s.origin[2]
                    + s.right[2] * s.size[0] * (sx - px)
                    + s.up[2] * s.size[1] * (sy - py),
            ]
        };
        let p00 = corner(0.0, 0.0);
        let p10 = corner(1.0, 0.0);
        let p11 = corner(1.0, 1.0);
        let p01 = corner(0.0, 1.0);
        let c = s.color;
        let mut v = |p: [f32; 3], uv: [f32; 2]| {
            verts.push(Vertex { pos: p, uv, color: c });
        };
        v(p00, [0.0, 1.0]);
        v(p10, [1.0, 1.0]);
        v(p11, [1.0, 0.0]);
        v(p00, [0.0, 1.0]);
        v(p11, [1.0, 0.0]);
        v(p01, [0.0, 0.0]);
    }

    fn push_ui(&self, verts: &mut Vec<Vertex>, q: &UiQuad) {
        let [x, y, w, h] = q.rect;
        let [u0, v0, u1, v1] = q.uv;
        let c = q.color;
        let mut v = |px: f32, py: f32, u: f32, vv: f32| {
            verts.push(Vertex {
                pos: [px, py, 0.0],
                uv: [u, vv],
                color: c,
            });
        };
        v(x, y, u0, v0);
        v(x + w, y, u1, v0);
        v(x + w, y + h, u1, v1);
        v(x, y, u0, v0);
        v(x + w, y + h, u1, v1);
        v(x, y + h, u0, v1);
    }

    pub fn draw(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        sprites: &[Sprite],
        screen: &[ScreenQuad],
        ui: &[UiQuad],
    ) {
        let mut verts: Vec<Vertex> =
            Vec::with_capacity(screen.len() * 6 + sprites.len() * 6 + ui.len() * 6);
        enum Pass {
            Screen(Tex),
            World(Tex, bool),
            Ui(usize),
        }
        let mut ranges: Vec<(Pass, std::ops::Range<u32>)> = Vec::new();

        for q in screen {
            let start = verts.len() as u32;
            self.push_screen(&mut verts, q.color);
            ranges.push((Pass::Screen(q.tex), start..verts.len() as u32));
        }

        let mut i = 0;
        while i < sprites.len() {
            let tex = sprites[i].tex;
            let additive = sprites[i].additive;
            let start = verts.len() as u32;
            while i < sprites.len() && sprites[i].tex == tex && sprites[i].additive == additive {
                let s = &sprites[i];
                self.push_sprite(&mut verts, s);
                i += 1;
            }
            ranges.push((Pass::World(tex, additive), start..verts.len() as u32));
        }

        let mut i = 0;
        while i < ui.len() {
            let tex = ui[i].tex;
            let start = verts.len() as u32;
            while i < ui.len() && ui[i].tex == tex {
                self.push_ui(&mut verts, &ui[i]);
                i += 1;
            }
            ranges.push((Pass::Ui(tex), start..verts.len() as u32));
        }

        let bytes: &[u8] = bytemuck::cast_slice(&verts);
        if self.vbuf.is_none() || self.vbuf_cap < bytes.len() {
            let cap = bytes.len().next_power_of_two().max(4096);
            self.vbuf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sprites"),
                size: cap as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.vbuf_cap = cap;
        }
        if let Some(vb) = &self.vbuf {
            self.queue.write_buffer(vb, 0, bytes);
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("main"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        if let Some(vb) = &self.vbuf {
            pass.set_vertex_buffer(0, vb.slice(..));
        }
        for (p, range) in ranges {
            let (group, additive) = match p {
                Pass::Screen(t) => (self.screen_groups.get(&t), false),
                Pass::World(t, add) => (self.groups.get(&t), add),
                Pass::Ui(i) => (self.ui_groups.get(i), false),
            };
            if let Some(g) = group {
                pass.set_pipeline(if additive {
                    &self.pipeline_additive
                } else {
                    &self.pipeline
                });
                pass.set_bind_group(0, g, &[]);
                pass.draw(range, 0..1);
            }
        }
    }
}

/// Хелпер: заполнить буфер через DeviceExt (нужен для тестов/оффскрина).
#[allow(dead_code)]
pub fn init_buffer(device: &wgpu::Device, data: &[u8]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("init"),
        contents: data,
        usage: wgpu::BufferUsages::VERTEX,
    })
}
