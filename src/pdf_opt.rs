//! Otimização de PDFs: extrai as imagens embutidas, recomprime-as como JPEG
//! (com os mesmos métodos usados para imagens comuns) e recria o documento
//! com os objetos compactados.

use crate::image_opt::{encode_jpeg, resize};
use image::{DynamicImage, ExtendedColorType, GenericImageView, GrayImage, ImageFormat, RgbImage};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use std::collections::HashSet;

pub struct PdfOutput {
    pub data: Vec<u8>,
    pub images_found: u32,
    pub images_optimized: u32,
    /// `true` quando o PDF gerado não ficou menor e o original foi mantido.
    pub kept_original: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Color {
    Gray,
    Rgb,
}

impl Color {
    fn channels(self) -> usize {
        match self {
            Color::Gray => 1,
            Color::Rgb => 3,
        }
    }
}

enum ColorSpace {
    Direct(Color),
    Indexed { base: Color, palette: Vec<u8> },
}

enum Source {
    /// Stream já em JPEG (filtro DCTDecode).
    Jpeg(Vec<u8>),
    /// Amostras de 8 bits descomprimidas.
    Raw { pixels: Vec<u8>, color: Color },
}

struct Job {
    id: ObjectId,
    width: u32,
    height: u32,
    source: Source,
    original_len: usize,
}

struct Replacement {
    id: ObjectId,
    data: Vec<u8>,
    width: u32,
    height: u32,
    color: Color,
}

pub fn compress_pdf(data: &[u8], quality: u8, scale: f32) -> Result<PdfOutput, String> {
    let mut doc = Document::load_mem(data).map_err(|e| format!("Não foi possível ler o PDF: {e}"))?;
    if doc.is_encrypted() {
        return Err("PDFs criptografados não são suportados".into());
    }

    // Máscaras (SMask / Mask) são mantidas sem perdas.
    let masks = collect_mask_ids(&doc);

    let mut images_found = 0u32;
    let mut jobs = Vec::new();
    for (&id, object) in doc.objects.iter() {
        let Object::Stream(stream) = object else { continue };
        if !is_image(&stream.dict) {
            continue;
        }
        images_found += 1;
        if masks.contains(&id) {
            continue;
        }
        if let Some(job) = prepare_job(&doc, id, stream) {
            jobs.push(job);
        }
    }

    let replacements = run_jobs(jobs, quality, scale);
    let images_optimized = replacements.len() as u32;

    for r in replacements {
        if let Ok(Object::Stream(stream)) = doc.get_object_mut(r.id) {
            apply_replacement(stream, r);
        }
    }

    doc.prune_objects();
    doc.delete_zero_length_streams();
    doc.renumber_objects();
    doc.compress();

    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(|e| format!("Falha ao gravar o PDF: {e}"))?;

    if out.len() >= data.len() {
        return Ok(PdfOutput { data: data.to_vec(), images_found, images_optimized: 0, kept_original: true });
    }
    Ok(PdfOutput { data: out, images_found, images_optimized, kept_original: false })
}

#[cfg(not(target_arch = "wasm32"))]
fn run_jobs(jobs: Vec<Job>, quality: u8, scale: f32) -> Vec<Replacement> {
    use rayon::prelude::*;
    jobs.into_par_iter().filter_map(|job| process_job(job, quality, scale)).collect()
}

#[cfg(target_arch = "wasm32")]
fn run_jobs(jobs: Vec<Job>, quality: u8, scale: f32) -> Vec<Replacement> {
    jobs.into_iter().filter_map(|job| process_job(job, quality, scale)).collect()
}

fn is_image(dict: &Dictionary) -> bool {
    matches!(dict.get(b"Subtype").and_then(Object::as_name), Ok(b"Image"))
}

fn collect_mask_ids(doc: &Document) -> HashSet<ObjectId> {
    let mut ids = HashSet::new();
    for object in doc.objects.values() {
        if let Object::Stream(stream) = object {
            for key in [&b"SMask"[..], b"Mask"] {
                if let Ok(id) = stream.dict.get(key).and_then(Object::as_reference) {
                    ids.insert(id);
                }
            }
        }
    }
    ids
}

fn deref<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Object> {
    doc.dereference(obj).ok().map(|(_, o)| o)
}

fn get_int(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<i64> {
    deref(doc, dict.get(key).ok()?)?.as_i64().ok()
}

fn parse_color(doc: &Document, obj: &Object) -> Option<Color> {
    match parse_color_space(doc, obj)? {
        ColorSpace::Direct(c) => Some(c),
        ColorSpace::Indexed { .. } => None,
    }
}

fn parse_color_space(doc: &Document, obj: &Object) -> Option<ColorSpace> {
    match deref(doc, obj)? {
        Object::Name(name) => match name.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" => Some(ColorSpace::Direct(Color::Gray)),
            b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(ColorSpace::Direct(Color::Rgb)),
            _ => None,
        },
        Object::Array(arr) => {
            let family = arr.first()?.as_name().ok()?;
            match family {
                b"CalGray" => Some(ColorSpace::Direct(Color::Gray)),
                b"CalRGB" => Some(ColorSpace::Direct(Color::Rgb)),
                b"ICCBased" => {
                    let stream = deref(doc, arr.get(1)?)?.as_stream().ok()?;
                    match get_int(doc, &stream.dict, b"N")? {
                        1 => Some(ColorSpace::Direct(Color::Gray)),
                        3 => Some(ColorSpace::Direct(Color::Rgb)),
                        _ => None,
                    }
                }
                b"Indexed" | b"I" => {
                    let base = parse_color(doc, arr.get(1)?)?;
                    let palette = match deref(doc, arr.get(3)?)? {
                        Object::String(bytes, _) => bytes.clone(),
                        Object::Stream(s) => s.decompressed_content().unwrap_or_else(|_| s.content.clone()),
                        _ => return None,
                    };
                    Some(ColorSpace::Indexed { base, palette })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Reúne tudo o que é preciso para recomprimir uma imagem, ou `None` se ela
/// usa um formato que não sabemos reprocessar sem risco (CMYK, JBIG2, 1 bit, ...).
fn prepare_job(doc: &Document, id: ObjectId, stream: &Stream) -> Option<Job> {
    let dict = &stream.dict;
    if matches!(dict.get(b"ImageMask").and_then(Object::as_bool), Ok(true)) {
        return None;
    }
    // Arrays /Decode alteram a interpretação das amostras; não arriscamos.
    if dict.has(b"Decode") {
        return None;
    }
    // Máscara por faixa de cor (/Mask [..]) não sobrevive à compressão com perdas.
    if matches!(dict.get(b"Mask"), Ok(Object::Array(_))) {
        return None;
    }
    // SMask com /Matte exige as mesmas dimensões da imagem.
    if let Ok(smask) = dict.get(b"SMask") {
        let smask = deref(doc, smask)?.as_stream().ok()?;
        if smask.dict.has(b"Matte") {
            return None;
        }
    }

    let width = u32::try_from(get_int(doc, dict, b"Width")?).ok()?;
    let height = u32::try_from(get_int(doc, dict, b"Height")?).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let pixels_count = width as usize * height as usize;
    let color_space = parse_color_space(doc, dict.get(b"ColorSpace").ok()?)?;
    let filters = stream.filters().unwrap_or_default();

    let source = if filters == [b"DCTDecode".as_slice()] {
        // JPEG embutido: só aceitamos cinza/RGB (CMYK sai com cores erradas).
        match color_space {
            ColorSpace::Direct(_) => Source::Jpeg(stream.content.clone()),
            ColorSpace::Indexed { .. } => return None,
        }
    } else {
        if filters.iter().any(|f| !matches!(*f, b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode")) {
            return None; // JPX, JBIG2, CCITT, RunLength...
        }
        if get_int(doc, dict, b"BitsPerComponent")? != 8 {
            return None;
        }
        let raw = if filters.is_empty() { stream.content.clone() } else { stream.decompressed_content().ok()? };
        match color_space {
            ColorSpace::Direct(color) => {
                let needed = pixels_count * color.channels();
                if raw.len() < needed {
                    return None;
                }
                let mut pixels = raw;
                pixels.truncate(needed);
                Source::Raw { pixels, color }
            }
            ColorSpace::Indexed { base, palette } => {
                if raw.len() < pixels_count {
                    return None;
                }
                let n = base.channels();
                let mut pixels = Vec::with_capacity(pixels_count * n);
                for &index in &raw[..pixels_count] {
                    let start = index as usize * n;
                    match palette.get(start..start + n) {
                        Some(entry) => pixels.extend_from_slice(entry),
                        None => pixels.extend(std::iter::repeat_n(0, n)),
                    }
                }
                Source::Raw { pixels, color: base }
            }
        }
    };

    Some(Job { id, width, height, source, original_len: stream.content.len() })
}

fn process_job(job: Job, quality: u8, scale: f32) -> Option<Replacement> {
    let img = match job.source {
        Source::Jpeg(bytes) => image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg).ok()?,
        Source::Raw { pixels, color: Color::Gray } => {
            DynamicImage::ImageLuma8(GrayImage::from_raw(job.width, job.height, pixels)?)
        }
        Source::Raw { pixels, color: Color::Rgb } => {
            DynamicImage::ImageRgb8(RgbImage::from_raw(job.width, job.height, pixels)?)
        }
    };

    let img = resize(img, scale);
    let (width, height) = img.dimensions();
    let (data, color) = if img.color().has_color() {
        (encode_jpeg(img.to_rgb8().as_raw(), width, height, ExtendedColorType::Rgb8, quality).ok()?, Color::Rgb)
    } else {
        (encode_jpeg(img.to_luma8().as_raw(), width, height, ExtendedColorType::L8, quality).ok()?, Color::Gray)
    };

    // Só substitui se realmente economizar espaço.
    (data.len() < job.original_len).then_some(Replacement { id: job.id, data, width, height, color })
}

fn apply_replacement(stream: &mut Stream, r: Replacement) {
    let dict = &mut stream.dict;
    dict.set("Width", r.width as i64);
    dict.set("Height", r.height as i64);
    dict.set("BitsPerComponent", 8);
    dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
    dict.set(
        "ColorSpace",
        Object::Name(match r.color {
            Color::Gray => b"DeviceGray".to_vec(),
            Color::Rgb => b"DeviceRGB".to_vec(),
        }),
    );
    dict.remove(b"DecodeParms");
    dict.remove(b"Decode");
    stream.set_content(r.data);
}
