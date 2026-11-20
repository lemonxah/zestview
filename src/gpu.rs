use std::sync::Arc;

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::decode::{Pixels, Primaries, Signal, Transfer};

const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const REFERENCE_WHITE_NITS: f32 = 203.0;
const MIN_WHITE_NITS: f32 = 80.0;
const MAX_WHITE_NITS: f32 = 1000.0;

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
const P3_TO_BT709: [[f32; 3]; 3] = [
    [1.2249401, -0.2249404, 0.0],
    [-0.0420569, 1.0420571, 0.0],
    [-0.0196376, -0.0786361, 1.0982735],
];
const BT2020_TO_BT709: [[f32; 3]; 3] = [
    [1.6604910, -0.5876411, -0.0728499],
    [-0.1245505, 1.1328999, -0.0083494],
    [-0.0181508, -0.1005789, 1.1187297],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    Sdr,
    ScRgb,
    Pq,
}

impl OutputMode {
    pub fn label(self) -> &'static str {
        match self {
            OutputMode::Sdr => "SDR",
            OutputMode::ScRgb => "HDR scRGB",
            OutputMode::Pq => "HDR10 PQ",
        }
    }

    fn code(self) -> u32 {
        match self {
            OutputMode::Sdr => 0,
            OutputMode::ScRgb => 1,
            OutputMode::Pq => 2,
        }
    }
}

#[derive(Clone, Copy)]
struct OutputTarget {
    mode: OutputMode,
    format: wgpu::TextureFormat,
    color_space: wgpu::SurfaceColorSpace,
    encode_srgb: bool,
}

pub struct ImageTransform {
    pub scale: [f32; 2],
    pub offset: [f32; 2],
    pub nearest: bool,
}

struct ImageTexture {
    texture: wgpu::Texture,
    linear: wgpu::BindGroup,
    nearest: wgpu::BindGroup,
    width: u32,
    height: u32,
    mip_count: u32,
}

struct Staging {
    texture: wgpu::Texture,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
}

struct Scene {
    view: wgpu::TextureView,
    output_bind_group: wgpu::BindGroup,
}

struct TextLayer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    renderer: TextRenderer,
    buffer: Buffer,
    visible: bool,
}

pub struct Gpu {
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    backend: String,
    outputs: Vec<OutputTarget>,
    output_index: usize,
    white_nits: f32,
    panel_pipeline: wgpu::RenderPipeline,
    panel_buffer: wgpu::Buffer,
    panel_bind_group: wgpu::BindGroup,
    load_layout: wgpu::BindGroupLayout,
    load_pipeline_layout: wgpu::PipelineLayout,
    output_pipeline: wgpu::RenderPipeline,
    output_buffer: wgpu::Buffer,
    scene: Scene,
    image_pipeline: wgpu::RenderPipeline,
    mip_pipeline: wgpu::RenderPipeline,
    linearize_pipeline: wgpu::RenderPipeline,
    linearize_buffer: wgpu::Buffer,
    linear_sampler: wgpu::Sampler,
    nearest_sampler: wgpu::Sampler,
    view_buffer: wgpu::Buffer,
    image: Option<ImageTexture>,
    staging: Option<Staging>,
    text: TextLayer,
    pub window: Arc<dyn Window>,
}

fn available_outputs(caps: &wgpu::SurfaceCapabilities) -> Vec<OutputTarget> {
    let supports = |format: wgpu::TextureFormat, space: wgpu::SurfaceColorSpaces| {
        caps.format_capabilities
            .iter()
            .any(|f| f.format == format && f.color_spaces.contains(space))
    };
    let mut outputs = Vec::new();
    if supports(
        wgpu::TextureFormat::Rgba16Float,
        wgpu::SurfaceColorSpaces::EXTENDED_SRGB_LINEAR,
    ) {
        outputs.push(OutputTarget {
            mode: OutputMode::ScRgb,
            format: wgpu::TextureFormat::Rgba16Float,
            color_space: wgpu::SurfaceColorSpace::ExtendedSrgbLinear,
            encode_srgb: false,
        });
    }
    if supports(
        wgpu::TextureFormat::Rgb10a2Unorm,
        wgpu::SurfaceColorSpaces::BT2100_PQ,
    ) {
        outputs.push(OutputTarget {
            mode: OutputMode::Pq,
            format: wgpu::TextureFormat::Rgb10a2Unorm,
            color_space: wgpu::SurfaceColorSpace::Bt2100Pq,
            encode_srgb: false,
        });
    }
    let sdr_format = caps
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb() && supports(*f, wgpu::SurfaceColorSpaces::SRGB));
    let (format, encode_srgb) = match sdr_format {
        Some(format) => (format, false),
        None => (caps.formats[0], true),
    };
    outputs.push(OutputTarget {
        mode: OutputMode::Sdr,
        format,
        color_space: wgpu::SurfaceColorSpace::Srgb,
        encode_srgb,
    });
    outputs
}

fn preferred_white_nits() -> f32 {
    std::env::var("ZESTVIEW_WHITE_NITS")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(REFERENCE_WHITE_NITS)
        .clamp(MIN_WHITE_NITS, MAX_WHITE_NITS)
}

fn preferred_output() -> Option<OutputMode> {
    let value = std::env::var("ZESTVIEW_OUTPUT").ok()?;
    match value.to_lowercase().as_str() {
        "sdr" => Some(OutputMode::Sdr),
        "scrgb" => Some(OutputMode::ScRgb),
        "pq" | "hdr10" => Some(OutputMode::Pq),
        _ => None,
    }
}

fn gamut_to_bt709(primaries: Primaries) -> [[f32; 3]; 3] {
    match primaries {
        Primaries::Bt709 => IDENTITY,
        Primaries::DisplayP3 => P3_TO_BT709,
        Primaries::Bt2020 => BT2020_TO_BT709,
    }
}

fn linearize_params(signal: Signal) -> [f32; 16] {
    let m = gamut_to_bt709(signal.primaries);
    let mut data = [0f32; 16];
    for col in 0..3 {
        for row in 0..3 {
            data[col * 4 + row] = m[row][col];
        }
    }
    let transfer: u32 = match signal.transfer {
        Transfer::Srgb => 0,
        Transfer::Linear => 1,
        Transfer::Pq => 2,
        Transfer::Hlg => 3,
    };
    data[12] = f32::from_bits(transfer);
    data[13] = REFERENCE_WHITE_NITS;
    data
}

impl Gpu {
    pub fn new(event_loop: &dyn ActiveEventLoop, window: Arc<dyn Window>) -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(event_loop.owned_display_handle()),
        ));
        let surface = instance.create_surface(window.clone()).unwrap();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .unwrap();
        let info = adapter.get_info();
        let backend = format!("{:?} · {}", info.backend, info.name);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .unwrap();

        let outputs = available_outputs(&surface.get_capabilities(&adapter));
        let output_index = preferred_output()
            .and_then(|mode| outputs.iter().position(|o| o.mode == mode))
            .unwrap_or(0);
        let size = window.surface_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .unwrap();
        config.format = outputs[output_index].format;
        config.color_space = outputs[output_index].color_space;
        surface.configure(&device, &config);

        let load_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("load"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let load_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("load"),
            bind_group_layouts: &[Some(&load_layout)],
            immediate_size: 0,
        });

        let image_pipeline = create_pipeline(
            &device,
            include_str!("shaders/image.wgsl"),
            None,
            SCENE_FORMAT,
            wgpu::PrimitiveTopology::TriangleStrip,
            None,
        );
        let mip_pipeline = create_pipeline(
            &device,
            include_str!("shaders/mip.wgsl"),
            None,
            SCENE_FORMAT,
            wgpu::PrimitiveTopology::TriangleList,
            None,
        );
        let linearize_pipeline = create_pipeline(
            &device,
            include_str!("shaders/linearize.wgsl"),
            Some(&load_pipeline_layout),
            SCENE_FORMAT,
            wgpu::PrimitiveTopology::TriangleList,
            None,
        );
        let output_pipeline = create_pipeline(
            &device,
            include_str!("shaders/output.wgsl"),
            Some(&load_pipeline_layout),
            config.format,
            wgpu::PrimitiveTopology::TriangleList,
            None,
        );
        let panel_pipeline = create_pipeline(
            &device,
            include_str!("shaders/panel.wgsl"),
            None,
            SCENE_FORMAT,
            wgpu::PrimitiveTopology::TriangleStrip,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );

        let uniform = |label: &str, size: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let view_buffer = uniform("view", 16);
        let linearize_buffer = uniform("linearize", 64);
        let output_buffer = uniform("output", 16);
        let panel_buffer = uniform("panel", 48);
        let panel_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("panel"),
            layout: &panel_pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: panel_buffer.as_entire_binding(),
            }],
        });

        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let nearest_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let scene = create_scene(&device, &load_layout, &output_buffer, config.width, config.height);
        let text = TextLayer::new(&device, &queue, SCENE_FORMAT, window.scale_factor() as f32);

        let gpu = Self {
            instance,
            surface,
            device,
            queue,
            config,
            backend,
            outputs,
            output_index,
            white_nits: preferred_white_nits(),
            panel_pipeline,
            panel_buffer,
            panel_bind_group,
            load_layout,
            load_pipeline_layout,
            output_pipeline,
            output_buffer,
            scene,
            image_pipeline,
            mip_pipeline,
            linearize_pipeline,
            linearize_buffer,
            linear_sampler,
            nearest_sampler,
            view_buffer,
            image: None,
            staging: None,
            text,
            window,
        };
        gpu.write_output_params();
        gpu
    }

    pub fn size(&self) -> (f32, f32) {
        (self.config.width as f32, self.config.height as f32)
    }

    pub fn max_texture_size(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    pub fn texture_width(&self) -> Option<u32> {
        self.image.as_ref().map(|i| i.width)
    }

    pub fn backend_label(&self) -> &str {
        &self.backend
    }

    pub fn output_mode(&self) -> OutputMode {
        self.outputs[self.output_index].mode
    }

    pub fn cycle_output(&mut self) {
        self.output_index = (self.output_index + 1) % self.outputs.len();
        let target = self.outputs[self.output_index];
        self.config.format = target.format;
        self.config.color_space = target.color_space;
        self.surface.configure(&self.device, &self.config);
        self.output_pipeline = create_pipeline(
            &self.device,
            include_str!("shaders/output.wgsl"),
            Some(&self.load_pipeline_layout),
            target.format,
            wgpu::PrimitiveTopology::TriangleList,
            None,
        );
        self.write_output_params();
    }

    pub fn white_nits(&self) -> f32 {
        self.white_nits
    }

    pub fn adjust_white(&mut self, factor: f32) {
        self.white_nits = (self.white_nits * factor).clamp(MIN_WHITE_NITS, MAX_WHITE_NITS);
        self.write_output_params();
    }

    fn write_output_params(&self) {
        let target = self.outputs[self.output_index];
        let data: [u32; 4] = [
            target.mode.code(),
            target.encode_srgb as u32,
            self.white_nits.to_bits(),
            0,
        ];
        self.queue
            .write_buffer(&self.output_buffer, 0, bytemuck::cast_slice(&data));
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.scene = create_scene(
            &self.device,
            &self.load_layout,
            &self.output_buffer,
            width,
            height,
        );
    }

    pub fn set_scale_factor(&mut self, scale: f32) {
        self.text.set_scale(scale);
    }

    pub fn set_text(&mut self, text: &str) {
        self.text.set_text(text);
    }

    pub fn toggle_text(&mut self) {
        self.text.visible = !self.text.visible;
    }

    pub fn clear_image(&mut self) {
        self.image = None;
        self.staging = None;
    }

    pub fn upload_frame(&mut self, pixels: &Pixels, signal: Signal) {
        let (width, height) = pixels.dimensions();
        let (format, bytes, bytes_per_row): (_, &[u8], _) = match pixels {
            Pixels::Rgba8(p) => (wgpu::TextureFormat::Rgba8Unorm, p.as_raw(), 4 * width),
            Pixels::RgbaF32(p) => (
                wgpu::TextureFormat::Rgba32Float,
                bytemuck::cast_slice(p.as_raw()),
                16 * width,
            ),
        };
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let reuse_staging = self
            .staging
            .as_ref()
            .is_some_and(|s| s.format == format && s.width == width && s.height == height);
        if !reuse_staging {
            self.staging = Some(Staging {
                texture: self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("staging"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                }),
                format,
                width,
                height,
            });
        }
        let staging = self.staging.as_ref().unwrap();
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &staging.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
            size,
        );

        let reuse_image = self
            .image
            .as_ref()
            .is_some_and(|i| i.width == width && i.height == height);
        if !reuse_image {
            self.image = Some(self.create_image_texture(width, height));
        }

        self.queue.write_buffer(
            &self.linearize_buffer,
            0,
            bytemuck::cast_slice(&linearize_params(signal)),
        );

        let staging = self.staging.as_ref().unwrap();
        let image = self.image.as_ref().unwrap();
        let source_view = staging
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("linearize"),
            layout: &self.load_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.linearize_buffer.as_entire_binding(),
                },
            ],
        });
        let target = mip_view(&image.texture, 0);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = begin_pass(&mut encoder, &target, wgpu::Color::TRANSPARENT);
            pass.set_pipeline(&self.linearize_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.encode_mips(&mut encoder, &image.texture, image.mip_count);
        self.queue.submit([encoder.finish()]);
    }

    fn create_image_texture(&self, width: u32, height: u32) -> ImageTexture {
        let mip_count = 32 - width.max(height).leading_zeros();
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCENE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let layout = self.image_pipeline.get_bind_group_layout(0);
        let bind = |sampler: &wgpu::Sampler| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("image"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.view_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        };
        ImageTexture {
            linear: bind(&self.linear_sampler),
            nearest: bind(&self.nearest_sampler),
            texture,
            width,
            height,
            mip_count,
        }
    }

    fn encode_mips(&self, encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture, mip_count: u32) {
        let layout = self.mip_pipeline.get_bind_group_layout(0);
        for level in 1..mip_count {
            let src = mip_view(texture, level - 1);
            let dst = mip_view(texture, level);
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mip"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                    },
                ],
            });
            let mut pass = begin_pass(encoder, &dst, wgpu::Color::TRANSPARENT);
            pass.set_pipeline(&self.mip_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    pub fn render(&mut self, transform: Option<ImageTransform>) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.surface.configure(&self.device, &self.config);
                frame
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self.instance.create_surface(self.window.clone()).unwrap();
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return;
            }
            _ => {
                self.window.request_redraw();
                return;
            }
        };

        if let Some(t) = &transform {
            let data = [t.scale[0], t.scale[1], t.offset[0], t.offset[1]];
            self.queue
                .write_buffer(&self.view_buffer, 0, bytemuck::cast_slice(&data));
        }

        let text_ready = self.text.visible
            && self
                .text
                .prepare(&self.device, &self.queue, self.config.width, self.config.height);
        if text_ready {
            let (x, y, w, h, radius) = self.text.panel_rect();
            let data = [
                x,
                y,
                w,
                h,
                0.0,
                0.0,
                0.0,
                0.6,
                self.config.width as f32,
                self.config.height as f32,
                radius,
                0.0,
            ];
            self.queue
                .write_buffer(&self.panel_buffer, 0, bytemuck::cast_slice(&data));
        }

        let surface_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let background = wgpu::Color {
                r: 0.012,
                g: 0.012,
                b: 0.015,
                a: 1.0,
            };
            let mut pass = begin_pass(&mut encoder, &self.scene.view, background);
            if let (Some(image), Some(t)) = (&self.image, &transform) {
                pass.set_pipeline(&self.image_pipeline);
                let group = if t.nearest {
                    &image.nearest
                } else {
                    &image.linear
                };
                pass.set_bind_group(0, group, &[]);
                pass.draw(0..4, 0..1);
            }
            if text_ready {
                pass.set_pipeline(&self.panel_pipeline);
                pass.set_bind_group(0, &self.panel_bind_group, &[]);
                pass.draw(0..4, 0..1);
                self.text
                    .renderer
                    .render(&self.text.atlas, &self.text.viewport, &mut pass)
                    .unwrap();
            }
        }
        {
            let mut pass = begin_pass(&mut encoder, &surface_view, wgpu::Color::BLACK);
            pass.set_pipeline(&self.output_pipeline);
            pass.set_bind_group(0, &self.scene.output_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);
        self.text.atlas.trim();
    }
}

fn mip_view(texture: &wgpu::Texture, level: u32) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        base_mip_level: level,
        mip_level_count: Some(1),
        ..Default::default()
    })
}

fn begin_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    clear: wgpu::Color,
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(clear),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    })
}

fn create_scene(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    output_buffer: &wgpu::Buffer,
    width: u32,
    height: u32,
) -> Scene {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: SCENE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let output_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("output"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer.as_entire_binding(),
            },
        ],
    });
    Scene {
        view,
        output_bind_group,
    }
}

impl TextLayer {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        scale: f32,
    ) -> Self {
        let mut font_system = FontSystem::new();
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let buffer = Buffer::new(&mut font_system, metrics(scale));
        Self {
            font_system,
            swash_cache: SwashCache::new(),
            viewport,
            atlas,
            renderer,
            buffer,
            visible: true,
        }
    }

    fn set_scale(&mut self, scale: f32) {
        self.buffer.set_metrics(metrics(scale));
    }

    fn scale(&self) -> f32 {
        self.buffer.metrics().font_size / 14.0
    }

    fn panel_rect(&self) -> (f32, f32, f32, f32, f32) {
        let scale = self.scale();
        let (mut width, mut height) = (0f32, 0f32);
        for run in self.buffer.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
        }
        let margin = 20.0 * scale;
        let pad = 10.0 * scale;
        (
            margin - pad,
            margin - pad,
            width + 2.0 * pad,
            height + 2.0 * pad,
            8.0 * scale,
        )
    }

    fn set_text(&mut self, text: &str) {
        self.buffer.set_text(
            text,
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
            None,
        );
        self.buffer.shape_until_scroll(&mut self.font_system, false);
    }

    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
    ) -> bool {
        self.viewport.update(queue, Resolution { width, height });
        let scale = self.scale();
        let margin = 20.0 * scale;
        let bounds = TextBounds {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let area = TextArea {
            buffer: &self.buffer,
            left: margin,
            top: margin,
            scale: 1.0,
            bounds,
            default_color: Color::rgb(235, 235, 235),
            custom_glyphs: &[],
        };
        self.renderer
            .prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                [area],
                &mut self.swash_cache,
            )
            .is_ok()
    }
}

fn metrics(scale: f32) -> Metrics {
    Metrics::new(14.0 * scale, 20.0 * scale)
}

fn create_pipeline(
    device: &wgpu::Device,
    source: &str,
    layout: Option<&wgpu::PipelineLayout>,
    format: wgpu::TextureFormat,
    topology: wgpu::PrimitiveTopology,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
