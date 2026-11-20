use std::fs::{self, File};
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::imageops::FilterType;
use image::{
    AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader, Rgba32FImage,
    RgbaImage,
};
use jxl_oxide::color::{ColourEncoding, Primaries as JxlPrimaries, TransferFunction};
use jxl_oxide::{EnumColourEncoding, JxlImage, Render, RenderingIntent};
use libheif_rs::{
    ColorPrimaries, ColorSpace, HeifContext, LibHeif, RgbChroma, TransferCharacteristics,
};
use resvg::{tiny_skia, usvg};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Srgb,
    Linear,
    Pq,
    Hlg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primaries {
    Bt709,
    DisplayP3,
    Bt2020,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signal {
    pub transfer: Transfer,
    pub primaries: Primaries,
}

impl Signal {
    pub const SRGB: Self = Self {
        transfer: Transfer::Srgb,
        primaries: Primaries::Bt709,
    };

    pub fn describe(&self) -> String {
        let transfer = match self.transfer {
            Transfer::Srgb => "SDR",
            Transfer::Linear => "HDR linear",
            Transfer::Pq => "HDR PQ",
            Transfer::Hlg => "HDR HLG",
        };
        let primaries = match self.primaries {
            Primaries::Bt709 => "sRGB",
            Primaries::DisplayP3 => "Display P3",
            Primaries::Bt2020 => "BT.2020",
        };
        format!("{transfer} {primaries}")
    }
}

pub enum Pixels {
    Rgba8(RgbaImage),
    RgbaF32(Rgba32FImage),
}

impl Pixels {
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Pixels::Rgba8(p) => p.dimensions(),
            Pixels::RgbaF32(p) => p.dimensions(),
        }
    }

    fn fit_within(self, max: u32) -> Self {
        let (w, h) = self.dimensions();
        if w <= max && h <= max {
            return self;
        }
        let scale = max as f64 / w.max(h) as f64;
        let nw = ((w as f64 * scale).round() as u32).clamp(1, max);
        let nh = ((h as f64 * scale).round() as u32).clamp(1, max);
        match self {
            Pixels::Rgba8(p) => {
                Pixels::Rgba8(image::imageops::resize(&p, nw, nh, FilterType::Triangle))
            }
            Pixels::RgbaF32(p) => {
                Pixels::RgbaF32(image::imageops::resize(&p, nw, nh, FilterType::Triangle))
            }
        }
    }
}

pub struct Frame {
    pub pixels: Pixels,
    pub delay: Duration,
}

pub struct Decoded {
    pub frames: Vec<Frame>,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub color: String,
    pub signal: Signal,
}

#[derive(Error, Debug)]
pub enum DecodeError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Image(#[from] image::ImageError),
    #[error("svg: {0}")]
    Svg(#[from] usvg::Error),
    #[error("heif: {0}")]
    Heif(#[from] libheif_rs::HeifError),
    #[error("jxl: {0}")]
    Jxl(String),
    #[error("{0}")]
    Other(String),
}

const SVG_RENDER_SIZE: u32 = 4096;
const DEFAULT_FRAME_DELAY: Duration = Duration::from_millis(100);
const MIN_FRAME_DELAY: Duration = Duration::from_millis(11);

pub fn decode(path: &Path, max_dimension: u32) -> Result<Decoded, DecodeError> {
    let mut decoded = match extension(path).as_str() {
        "svg" | "svgz" => decode_svg(path, SVG_RENDER_SIZE.min(max_dimension))?,
        "heic" | "heif" | "hif" | "avif" => decode_heif(path)?,
        "jxl" => decode_jxl(path)?,
        _ => decode_raster(path)?,
    };
    decoded.frames = decoded
        .frames
        .into_iter()
        .map(|frame| Frame {
            pixels: frame.pixels.fit_within(max_dimension),
            delay: frame.delay,
        })
        .collect();
    Ok(decoded)
}

pub fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn still(pixels: Pixels) -> Vec<Frame> {
    vec![Frame {
        pixels,
        delay: Duration::ZERO,
    }]
}

fn frame_delay(delay: Duration) -> Duration {
    if delay < MIN_FRAME_DELAY {
        DEFAULT_FRAME_DELAY
    } else {
        delay
    }
}

fn decode_raster(path: &Path) -> Result<Decoded, DecodeError> {
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    reader.no_limits();
    let detected = reader.format();
    let format = detected
        .map(|f| format!("{f:?}").to_uppercase())
        .unwrap_or_else(|| extension(path).to_uppercase());

    if let Some(detected) = detected
        && let Some(frames) = decode_animation(path, detected)?
    {
        let (width, height) = frames[0].pixels.dimensions();
        let format = if detected == ImageFormat::Png {
            "APNG".into()
        } else {
            format
        };
        return Ok(Decoded {
            frames,
            width,
            height,
            format,
            color: "Rgba8".into(),
            signal: Signal::SRGB,
        });
    }

    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let (width, height) = (image.width(), image.height());
    let color = format!("{:?}", image.color());
    let (pixels, signal) = match image {
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_) => (
            Pixels::RgbaF32(image.into_rgba32f()),
            Signal {
                transfer: Transfer::Linear,
                primaries: Primaries::Bt709,
            },
        ),
        DynamicImage::ImageLuma16(_)
        | DynamicImage::ImageLumaA16(_)
        | DynamicImage::ImageRgb16(_)
        | DynamicImage::ImageRgba16(_) => (Pixels::RgbaF32(image.into_rgba32f()), Signal::SRGB),
        _ => (Pixels::Rgba8(image.into_rgba8()), Signal::SRGB),
    };
    Ok(Decoded {
        frames: still(pixels),
        width,
        height,
        format,
        color,
        signal,
    })
}

fn decode_animation(path: &Path, format: ImageFormat) -> Result<Option<Vec<Frame>>, DecodeError> {
    let file = BufReader::new(File::open(path)?);
    let frames = match format {
        ImageFormat::Gif => GifDecoder::new(file)?.into_frames(),
        ImageFormat::WebP => {
            let decoder = WebPDecoder::new(file)?;
            if !decoder.has_animation() {
                return Ok(None);
            }
            decoder.into_frames()
        }
        ImageFormat::Png => {
            let decoder = PngDecoder::new(file)?;
            if !decoder.is_apng()? {
                return Ok(None);
            }
            decoder.apng()?.into_frames()
        }
        _ => return Ok(None),
    };
    let frames: Vec<Frame> = frames
        .collect_frames()?
        .into_iter()
        .map(|frame| {
            let delay = Duration::from(frame.delay());
            Frame {
                pixels: Pixels::Rgba8(frame.into_buffer()),
                delay: frame_delay(delay),
            }
        })
        .collect();
    Ok((frames.len() > 1).then_some(frames))
}

fn font_database() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

fn decode_svg(path: &Path, target: u32) -> Result<Decoded, DecodeError> {
    let data = fs::read(path)?;
    let options = usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        fontdb: font_database(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(&data, &options)?;
    let size = tree.size();
    let scale = (target as f32 / size.width().max(size.height())).max(1.0);
    let pw = (size.width() * scale).ceil().max(1.0) as u32;
    let ph = (size.height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph)
        .ok_or_else(|| DecodeError::Other(format!("invalid svg size {pw}x{ph}")))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut rgba = Vec::with_capacity((pw * ph * 4) as usize);
    for pixel in pixmap.pixels() {
        let c = pixel.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    let pixels = RgbaImage::from_raw(pw, ph, rgba)
        .ok_or_else(|| DecodeError::Other("svg buffer size mismatch".into()))?;
    Ok(Decoded {
        frames: still(Pixels::Rgba8(pixels)),
        width: size.width().round().max(1.0) as u32,
        height: size.height().round().max(1.0) as u32,
        format: "SVG".into(),
        color: "Vector".into(),
        signal: Signal::SRGB,
    })
}

fn heif_signal(handle: &libheif_rs::ImageHandle) -> Signal {
    let Some(nclx) = handle.color_profile_nclx() else {
        return Signal::SRGB;
    };
    let transfer = match nclx.transfer_characteristics() {
        TransferCharacteristics::ITU_R_BT_2100_0_PQ => Transfer::Pq,
        TransferCharacteristics::ITU_R_BT_2100_0_HLG => Transfer::Hlg,
        TransferCharacteristics::Linear => Transfer::Linear,
        _ => Transfer::Srgb,
    };
    let primaries = match nclx.color_primaries() {
        ColorPrimaries::ITU_R_BT_2020_2_and_2100_0 => Primaries::Bt2020,
        ColorPrimaries::SMPTE_EG_432_1 => Primaries::DisplayP3,
        _ => Primaries::Bt709,
    };
    Signal {
        transfer,
        primaries,
    }
}

fn decode_heif(path: &Path) -> Result<Decoded, DecodeError> {
    let path_str = path
        .to_str()
        .ok_or_else(|| DecodeError::Other("path is not valid UTF-8".into()))?;
    let lib = LibHeif::new();
    let ctx = HeifContext::read_from_file(path_str)?;
    let handle = ctx.primary_image_handle()?;
    let signal = heif_signal(&handle);
    let bits = handle.luma_bits_per_pixel();
    let high_precision = bits > 8;
    let chroma = if high_precision {
        RgbChroma::HdrRgbaLe
    } else {
        RgbChroma::Rgba
    };
    let image = lib.decode(&handle, ColorSpace::Rgb(chroma), None)?;
    let planes = image.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| DecodeError::Other("no interleaved plane".into()))?;
    let (w, h) = (plane.width, plane.height);
    let pixels = if high_precision {
        let max = ((1u32 << plane.bits_per_pixel.clamp(1, 16)) - 1) as f32;
        let row_bytes = w as usize * 8;
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for row in 0..h as usize {
            let start = row * plane.stride;
            for sample in plane.data[start..start + row_bytes].chunks_exact(2) {
                rgba.push(u16::from_le_bytes([sample[0], sample[1]]) as f32 / max);
            }
        }
        Pixels::RgbaF32(
            Rgba32FImage::from_raw(w, h, rgba)
                .ok_or_else(|| DecodeError::Other("heif buffer size mismatch".into()))?,
        )
    } else {
        let row_bytes = w as usize * 4;
        let mut rgba = Vec::with_capacity(row_bytes * h as usize);
        for row in 0..h as usize {
            let start = row * plane.stride;
            rgba.extend_from_slice(&plane.data[start..start + row_bytes]);
        }
        Pixels::Rgba8(
            RgbaImage::from_raw(w, h, rgba)
                .ok_or_else(|| DecodeError::Other("heif buffer size mismatch".into()))?,
        )
    };
    let channels = if handle.has_alpha_channel() {
        "RGBA"
    } else {
        "RGB"
    };
    Ok(Decoded {
        frames: still(pixels),
        width: w,
        height: h,
        format: extension(path).to_uppercase(),
        color: format!("{bits}-bit {channels}"),
        signal,
    })
}

fn jxl_error(e: Box<dyn std::error::Error + Send + Sync>) -> DecodeError {
    DecodeError::Jxl(e.to_string())
}

fn expand_channels<T: Copy>(buf: &[T], channels: usize, opaque: T) -> Vec<T> {
    let mut rgba = Vec::with_capacity(buf.len() / channels * 4);
    for px in buf.chunks_exact(channels) {
        match channels {
            1 => rgba.extend_from_slice(&[px[0], px[0], px[0], opaque]),
            2 => rgba.extend_from_slice(&[px[0], px[0], px[0], px[1]]),
            3 => rgba.extend_from_slice(&[px[0], px[1], px[2], opaque]),
            _ => rgba.extend_from_slice(&px[..4]),
        }
    }
    rgba
}

fn jxl_render_to_pixels(render: &Render, float: bool) -> Result<Pixels, DecodeError> {
    let mut stream = render.stream();
    let (w, h, channels) = (
        stream.width(),
        stream.height(),
        stream.channels() as usize,
    );
    let len = w as usize * h as usize * channels;
    let mismatch = || DecodeError::Other("jxl buffer size mismatch".into());
    if float {
        let mut buf = vec![0f32; len];
        stream.write_to_buffer(&mut buf);
        let rgba = expand_channels(&buf, channels, 1.0);
        Ok(Pixels::RgbaF32(
            Rgba32FImage::from_raw(w, h, rgba).ok_or_else(mismatch)?,
        ))
    } else {
        let mut buf = vec![0u8; len];
        stream.write_to_buffer(&mut buf);
        let rgba = expand_channels(&buf, channels, 255);
        Ok(Pixels::Rgba8(
            RgbaImage::from_raw(w, h, rgba).ok_or_else(mismatch)?,
        ))
    }
}

fn jxl_native_signal(encoding: &ColourEncoding) -> Option<Signal> {
    let ColourEncoding::Enum(e) = encoding else {
        return None;
    };
    let transfer = match e.tf {
        TransferFunction::Pq => Transfer::Pq,
        TransferFunction::Hlg => Transfer::Hlg,
        TransferFunction::Linear => Transfer::Linear,
        _ => return None,
    };
    let primaries = match e.primaries {
        JxlPrimaries::Srgb => Primaries::Bt709,
        JxlPrimaries::Bt2100 => Primaries::Bt2020,
        JxlPrimaries::P3 => Primaries::DisplayP3,
        _ => return None,
    };
    Some(Signal {
        transfer,
        primaries,
    })
}

fn decode_jxl(path: &Path) -> Result<Decoded, DecodeError> {
    let mut image = JxlImage::builder().open(path).map_err(jxl_error)?;
    let metadata = &image.image_header().metadata;
    let bits = metadata.bit_depth.bits_per_sample();
    let native = jxl_native_signal(&metadata.colour_encoding);
    let ticks_per_second = metadata
        .animation
        .as_ref()
        .filter(|a| a.tps_numerator > 0 && a.tps_denominator > 0)
        .map(|a| a.tps_numerator as f64 / a.tps_denominator as f64);
    let signal = match native {
        Some(signal) => signal,
        None => {
            image.request_color_encoding(EnumColourEncoding::srgb(RenderingIntent::Relative));
            Signal::SRGB
        }
    };
    let float = native.is_some() || bits > 8;

    let keyframes = match ticks_per_second {
        Some(_) => image.num_loaded_keyframes().max(1),
        None => 1,
    };
    let mut frames = Vec::with_capacity(keyframes);
    for index in 0..keyframes {
        let render = image.render_frame(index).map_err(jxl_error)?;
        let delay = match ticks_per_second {
            Some(tps) if keyframes > 1 => {
                frame_delay(Duration::from_secs_f64(render.duration() as f64 / tps))
            }
            _ => Duration::ZERO,
        };
        frames.push(Frame {
            pixels: jxl_render_to_pixels(&render, float)?,
            delay,
        });
    }
    let (width, height) = frames[0].pixels.dimensions();
    Ok(Decoded {
        frames,
        width,
        height,
        format: "JXL".into(),
        color: format!("{bits}-bit {:?}", image.pixel_format()),
        signal,
    })
}
