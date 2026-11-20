mod decode;
mod gallery;
mod gpu;
mod loader;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use thiserror::Error;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::error::EventLoopError;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::decode::{Decoded, Frame, Signal};
use crate::gallery::Gallery;
use crate::gpu::{Gpu, ImageTransform, OutputMode};
use crate::loader::{Loader, Response};

const MIN_ZOOM: f32 = 0.01;
const MAX_ZOOM: f32 = 64.0;
const ZOOM_STEP: f32 = 1.15;

struct ImageInfo {
    name: String,
    width: u32,
    height: u32,
    format: String,
    color: String,
    signal: Signal,
    file_size: u64,
}

enum Status {
    Ready,
    Loading(String),
    Failed { name: String, error: String },
}

struct Animation {
    frames: Vec<Frame>,
    index: usize,
    next: Instant,
    paused: bool,
}

struct View {
    zoom: f32,
    offset: (f32, f32),
    fit: bool,
}

impl View {
    fn fit() -> Self {
        Self {
            zoom: 1.0,
            offset: (0.0, 0.0),
            fit: true,
        }
    }
}

struct App {
    gallery: Gallery,
    gpu: Option<Gpu>,
    loader: Option<Loader>,
    request_id: u64,
    shown: Option<ImageInfo>,
    status: Status,
    animation: Option<Animation>,
    view: View,
    cursor: PhysicalPosition<f64>,
    dragging: Option<PhysicalPosition<f64>>,
}

impl App {
    fn new(gallery: Gallery) -> Self {
        Self {
            gallery,
            gpu: None,
            loader: None,
            request_id: 0,
            shown: None,
            status: Status::Ready,
            animation: None,
            view: View::fit(),
            cursor: PhysicalPosition::new(0.0, 0.0),
            dragging: None,
        }
    }

    fn request_current(&mut self) {
        let (Some(gpu), Some(loader)) = (self.gpu.as_ref(), self.loader.as_ref()) else {
            return;
        };
        let Some(path) = self.gallery.current() else {
            self.status = Status::Ready;
            self.update_text();
            return;
        };
        let name = file_name(path);
        gpu.window.set_title(&format!("{name} — zestview"));
        self.request_id += 1;
        loader.request(self.request_id, path.to_path_buf());
        self.status = Status::Loading(name);
        self.update_text();
    }

    fn receive(&mut self, response: Response) {
        if response.id != self.request_id {
            return;
        }
        let name = self.gallery.current().map(file_name).unwrap_or_default();
        let Some(gpu) = self.gpu.as_mut() else { return };
        self.view = View::fit();
        match response.result {
            Ok(Decoded {
                frames,
                width,
                height,
                format,
                color,
                signal,
            }) => {
                gpu.upload_frame(&frames[0].pixels, signal);
                self.animation = (frames.len() > 1).then(|| Animation {
                    next: Instant::now() + frames[0].delay,
                    frames,
                    index: 0,
                    paused: false,
                });
                self.shown = Some(ImageInfo {
                    name,
                    width,
                    height,
                    format,
                    color,
                    signal,
                    file_size: response.file_size,
                });
                self.status = Status::Ready;
            }
            Err(err) => {
                gpu.clear_image();
                self.animation = None;
                self.shown = None;
                self.status = Status::Failed {
                    name,
                    error: err.to_string(),
                };
            }
        }
        self.update_text();
    }

    fn show_frame(&mut self, index: usize) {
        let (Some(gpu), Some(anim), Some(info)) =
            (self.gpu.as_mut(), self.animation.as_mut(), self.shown.as_ref())
        else {
            return;
        };
        anim.index = index % anim.frames.len();
        gpu.upload_frame(&anim.frames[anim.index].pixels, info.signal);
        self.update_text();
    }

    fn step_frame(&mut self, forward: bool) {
        let Some(anim) = self.animation.as_mut() else {
            return;
        };
        anim.paused = true;
        let len = anim.frames.len();
        let index = if forward {
            anim.index + 1
        } else {
            anim.index + len - 1
        };
        self.show_frame(index);
    }

    fn toggle_pause(&mut self) {
        if let Some(anim) = self.animation.as_mut() {
            anim.paused = !anim.paused;
            anim.next = Instant::now() + anim.frames[anim.index].delay;
            self.update_text();
        }
    }

    fn image_size(&self) -> Option<(f32, f32)> {
        self.shown
            .as_ref()
            .map(|info| (info.width as f32, info.height as f32))
    }

    fn effective_zoom(&self) -> f32 {
        let (Some(gpu), Some((iw, ih))) = (&self.gpu, self.image_size()) else {
            return 1.0;
        };
        if self.view.fit {
            let (ww, wh) = gpu.size();
            (ww / iw).min(wh / ih).min(1.0)
        } else {
            self.view.zoom
        }
    }

    fn unfit(&mut self) {
        if self.view.fit {
            self.view = View {
                zoom: self.effective_zoom(),
                offset: (0.0, 0.0),
                fit: false,
            };
        }
    }

    fn zoom_at(&mut self, factor: f32, anchor: PhysicalPosition<f64>) {
        let Some((ww, wh)) = self.gpu.as_ref().map(Gpu::size) else {
            return;
        };
        if self.image_size().is_none() {
            return;
        }
        self.unfit();
        let old = self.view.zoom;
        let new = (old * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let ax = anchor.x as f32 - ww / 2.0;
        let ay = anchor.y as f32 - wh / 2.0;
        let ratio = new / old;
        let (ox, oy) = self.view.offset;
        self.view.zoom = new;
        self.view.offset = (ax - (ax - ox) * ratio, ay - (ay - oy) * ratio);
        self.update_text();
    }

    fn set_actual_size(&mut self) {
        self.view = View {
            zoom: 1.0,
            offset: (0.0, 0.0),
            fit: false,
        };
        self.update_text();
    }

    fn transform(&self) -> Option<ImageTransform> {
        let gpu = self.gpu.as_ref()?;
        let (iw, ih) = self.image_size()?;
        let texture_width = gpu.texture_width()? as f32;
        let (ww, wh) = gpu.size();
        let zoom = self.effective_zoom();
        let (mut ox, mut oy) = if self.view.fit {
            (0.0, 0.0)
        } else {
            self.view.offset
        };
        let (sw, sh) = (iw * zoom, ih * zoom);
        let left = ww / 2.0 + ox - sw / 2.0;
        let top = wh / 2.0 + oy - sh / 2.0;
        ox += left.round() - left;
        oy += top.round() - top;
        Some(ImageTransform {
            scale: [sw / ww, sh / wh],
            offset: [2.0 * ox / ww, -2.0 * oy / wh],
            nearest: zoom * iw / texture_width >= 2.0,
        })
    }

    fn update_text(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else { return };
        let position = format!("{}/{}", self.gallery.position(), self.gallery.len());
        let help = "← → browse   scroll zoom   drag pan   F fit   1 actual size\nspace pause   , . step frames   H output mode   [ ] HDR brightness   I hide info";
        let mode = gpu.output_mode();
        let system = if mode == OutputMode::Sdr {
            format!("{}   output {}", gpu.backend_label(), mode.label())
        } else {
            format!(
                "{}   output {}   white {:.0} nits",
                gpu.backend_label(),
                mode.label(),
                gpu.white_nits()
            )
        };
        let headline = match &self.status {
            Status::Loading(name) => Some(format!("Loading {name}…   [{position}]")),
            Status::Failed { name, error } => {
                Some(format!("{name}   [{position}]\nCould not open: {error}"))
            }
            Status::Ready => None,
        };
        let details = self.shown.as_ref().map(|info| {
            let mut line = format!(
                "{} × {}   {}   {}   {}\n{}   {:.0}%",
                info.width,
                info.height,
                info.format,
                info.color,
                info.signal.describe(),
                format_bytes(info.file_size),
                self.effective_zoom() * 100.0,
            );
            if let Some(anim) = &self.animation {
                line.push_str(&format!("   frame {}/{}", anim.index + 1, anim.frames.len()));
                if anim.paused {
                    line.push_str(" (paused)");
                }
            }
            (info.name.clone(), line)
        });
        let body = match (headline, details) {
            (Some(headline), Some((_, details))) => format!("{headline}\n{details}"),
            (Some(headline), None) => headline,
            (None, Some((name, details))) => format!("{name}   [{position}]\n{details}"),
            (None, None) => "No images found".into(),
        };
        let text = format!("{body}\n{system}\n\n{help}");
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_text(&text);
            gpu.window.request_redraw();
        }
    }

    fn request_redraw(&self) {
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attributes = WindowAttributes::default()
            .with_title("zestview")
            .with_surface_size(LogicalSize::new(1280.0, 800.0));
        let window: Arc<dyn Window> = event_loop
            .create_window(with_app_id(attributes))
            .unwrap()
            .into();
        let gpu = Gpu::new(event_loop, window);
        self.loader = Some(Loader::new(
            event_loop.create_proxy(),
            gpu.max_texture_size(),
        ));
        self.gpu = Some(gpu);
        self.request_current();
    }

    fn proxy_wake_up(&mut self, _event_loop: &dyn ActiveEventLoop) {
        while let Some(response) = self.loader.as_ref().and_then(Loader::try_recv) {
            self.receive(response);
        }
    }

    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        let now = Instant::now();
        let advance = match self.animation.as_mut() {
            Some(anim) if !anim.paused => {
                if now >= anim.next {
                    let index = (anim.index + 1) % anim.frames.len();
                    anim.next += anim.frames[index].delay;
                    if anim.next < now {
                        anim.next = now + anim.frames[index].delay;
                    }
                    Some(index)
                } else {
                    None
                }
            }
            _ => {
                event_loop.set_control_flow(ControlFlow::Wait);
                return;
            }
        };
        if let Some(index) = advance {
            self.show_frame(index);
        }
        if let Some(anim) = &self.animation {
            event_loop.set_control_flow(ControlFlow::WaitUntil(anim.next));
        }
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::SurfaceResized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
                self.update_text();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.set_scale_factor(scale_factor as f32);
                }
                self.update_text();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::ArrowRight) => {
                        self.gallery.next();
                        self.request_current();
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.gallery.prev();
                        self.request_current();
                    }
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character(c) => match c.to_lowercase().as_str() {
                        "f" | "0" => {
                            self.view = View::fit();
                            self.update_text();
                        }
                        "1" => self.set_actual_size(),
                        "[" | "]" => {
                            if let Some(gpu) = self.gpu.as_mut() {
                                gpu.adjust_white(if c == "]" { 1.1 } else { 1.0 / 1.1 });
                            }
                            self.update_text();
                        }
                        "," => self.step_frame(false),
                        "." => self.step_frame(true),
                        " " => self.toggle_pause(),
                        "h" => {
                            if let Some(gpu) = self.gpu.as_mut() {
                                gpu.cycle_output();
                            }
                            self.update_text();
                        }
                        "i" => {
                            if let Some(gpu) = self.gpu.as_mut() {
                                gpu.toggle_text();
                            }
                            self.request_redraw();
                        }
                        "q" => event_loop.exit(),
                        _ => {}
                    },
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 50.0,
                    _ => 0.0,
                };
                self.zoom_at(ZOOM_STEP.powf(steps), self.cursor);
            }
            WindowEvent::PointerMoved { position, .. } => {
                self.cursor = position;
                if let Some(last) = self.dragging {
                    self.unfit();
                    self.view.offset.0 += (position.x - last.x) as f32;
                    self.view.offset.1 += (position.y - last.y) as f32;
                    self.dragging = Some(position);
                    self.request_redraw();
                }
            }
            WindowEvent::PointerButton {
                state,
                position,
                button,
                ..
            } => {
                if button.mouse_button() == Some(MouseButton::Left) {
                    self.dragging = (state == ElementState::Pressed).then_some(position);
                }
            }
            WindowEvent::RedrawRequested => {
                let transform = self.transform();
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.render(transform);
                }
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
fn with_app_id(attributes: WindowAttributes) -> WindowAttributes {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        attributes.with_platform_attributes(Box::new(
            winit::platform::wayland::WindowAttributesWayland::default()
                .with_name("zestview", "zestview"),
        ))
    } else {
        attributes.with_platform_attributes(Box::new(
            winit::platform::x11::WindowAttributesX11::default().with_name("zestview", "zestview"),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn with_app_id(attributes: WindowAttributes) -> WindowAttributes {
    attributes
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[derive(Error, Debug)]
pub enum MainError {
    #[error("EventLoop `{0}`")]
    EventLoop(#[from] EventLoopError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("usage: zestview <image or folder>")]
    Usage,
}

fn main() -> Result<(), MainError> {
    let path: PathBuf = std::env::args_os().nth(1).ok_or(MainError::Usage)?.into();
    let gallery = Gallery::open(&path)?;
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(App::new(gallery)).map_err(Into::into)
}
