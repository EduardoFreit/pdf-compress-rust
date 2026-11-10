//! Compressão de imagens comuns (JPEG, PNG, GIF, BMP, WebP, ...).

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, GenericImageView, ImageEncoder, ImageFormat, Rgb, RgbImage};

/// Formato de saída pedido pelo usuário.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// PNG quando a imagem usa transparência, JPEG caso contrário.
    Auto,
    Jpeg,
    Png,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "jpeg" | "jpg" => Ok(Self::Jpeg),
            "png" => Ok(Self::Png),
            other => Err(format!("Formato de saída desconhecido: {other}")),
        }
    }
}

pub struct ImageOutput {
    pub data: Vec<u8>,
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
    pub original_width: u32,
    pub original_height: u32,
    /// `true` quando nenhuma versão gerada ficou menor e o original foi mantido.
    pub kept_original: bool,
}

/// Limita a qualidade JPEG ao intervalo válido (1–100).
pub fn clamp_quality(quality: u8) -> u8 {
    quality.clamp(1, 100)
}

/// Calcula as novas dimensões para um fator de escala em (0, 1].
pub fn scaled_dims(width: u32, height: u32, scale: f32) -> (u32, u32) {
    let s = if scale.is_finite() { scale.clamp(0.01, 1.0) } else { 1.0 };
    let w = ((width as f32) * s).round().max(1.0) as u32;
    let h = ((height as f32) * s).round().max(1.0) as u32;
    (w, h)
}

/// Redimensiona a imagem se o fator de escala for menor que 1.
pub fn resize(img: DynamicImage, scale: f32) -> DynamicImage {
    let (w, h) = img.dimensions();
    let (nw, nh) = scaled_dims(w, h, scale);
    if (nw, nh) == (w, h) {
        img
    } else {
        img.resize_exact(nw, nh, FilterType::Triangle)
    }
}

/// Codifica pixels crus (L8 ou RGB8) como JPEG com a qualidade dada.
pub fn encode_jpeg(pixels: &[u8], width: u32, height: u32, color: ExtendedColorType, quality: u8) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, clamp_quality(quality))
        .write_image(pixels, width, height, color)
        .map_err(|e| format!("Falha ao codificar JPEG: {e}"))?;
    Ok(buf)
}

fn encode_png(pixels: &[u8], width: u32, height: u32, color: ExtendedColorType) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilter::Adaptive)
        .write_image(pixels, width, height, color)
        .map_err(|e| format!("Falha ao codificar PNG: {e}"))?;
    Ok(buf)
}

fn is_grayscale(img: &DynamicImage) -> bool {
    !img.color().has_color()
}

fn uses_transparency(img: &DynamicImage) -> bool {
    img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p[3] < 255)
}

/// Remove o canal alfa compondo a imagem sobre fundo branco (JPEG não tem transparência).
fn flatten_on_white(img: &DynamicImage) -> RgbImage {
    let rgba = img.to_rgba8();
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (dst, src) in out.pixels_mut().zip(rgba.pixels()) {
        let a = src[3] as u32;
        let blend = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        *dst = Rgb([blend(src[0]), blend(src[1]), blend(src[2])]);
    }
    out
}

pub fn compress_image(data: &[u8], quality: u8, scale: f32, format: OutputFormat) -> Result<ImageOutput, String> {
    let input_format = image::guess_format(data).map_err(|_| "Formato de imagem não reconhecido".to_string())?;
    let img = image::load_from_memory_with_format(data, input_format)
        .map_err(|e| format!("Falha ao decodificar a imagem: {e}"))?;
    let (original_width, original_height) = img.dimensions();

    let transparent = uses_transparency(&img);
    let target = match format {
        OutputFormat::Auto if transparent => OutputFormat::Png,
        OutputFormat::Auto => OutputFormat::Jpeg,
        f => f,
    };

    let img = resize(img, scale);
    let (width, height) = img.dimensions();
    let gray = is_grayscale(&img);

    let (encoded, mime, out_format) = match target {
        OutputFormat::Png => {
            let bytes = match (gray, transparent) {
                (true, false) => encode_png(img.to_luma8().as_raw(), width, height, ExtendedColorType::L8)?,
                (true, true) => encode_png(img.to_luma_alpha8().as_raw(), width, height, ExtendedColorType::La8)?,
                (false, false) => encode_png(img.to_rgb8().as_raw(), width, height, ExtendedColorType::Rgb8)?,
                (false, true) => encode_png(img.to_rgba8().as_raw(), width, height, ExtendedColorType::Rgba8)?,
            };
            (bytes, "image/png", ImageFormat::Png)
        }
        _ => {
            let bytes = if gray && !transparent {
                encode_jpeg(img.to_luma8().as_raw(), width, height, ExtendedColorType::L8, quality)?
            } else {
                encode_jpeg(flatten_on_white(&img).as_raw(), width, height, ExtendedColorType::Rgb8, quality)?
            };
            (bytes, "image/jpeg", ImageFormat::Jpeg)
        }
    };

    // Se nada mudou além do re-encoding e o resultado ficou maior, mantém o original.
    let same_shape = (width, height) == (original_width, original_height) && out_format == input_format;
    if same_shape && encoded.len() >= data.len() {
        return Ok(ImageOutput {
            data: data.to_vec(),
            mime: input_format.to_mime_type(),
            width,
            height,
            original_width,
            original_height,
            kept_original: true,
        });
    }

    Ok(ImageOutput {
        data: encoded,
        mime,
        width,
        height,
        original_width,
        original_height,
        kept_original: false,
    })
}
