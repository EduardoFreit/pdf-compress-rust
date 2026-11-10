use image::{GenericImageView, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use lopdf::Document;
use projeto_02::{compress_image_impl, compress_pdf_impl};
use std::io::Cursor;

/// Imagem com gradiente e ruído, para que a compressão tenha trabalho de verdade.
fn sample_rgb(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        let n = (x.wrapping_mul(7919) ^ y.wrapping_mul(104729)) % 32;
        Rgb([(x * 255 / w) as u8 ^ n as u8, (y * 255 / h) as u8, ((x + y) % 256) as u8])
    })
}

fn encode(img: &image::DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, format).unwrap();
    buf.into_inner()
}

#[test]
fn png_opaco_vira_jpeg_menor() {
    let png = encode(&sample_rgb(400, 300).into(), ImageFormat::Png);
    let r = compress_image_impl(&png, 70, 1.0, "auto").unwrap();
    assert_eq!(r.mime(), "image/jpeg");
    assert!(r.compressed_size() < png.len());
    let decoded = image::load_from_memory(&r.data()).unwrap();
    assert_eq!(decoded.dimensions(), (400, 300));
}

#[test]
fn escala_reduz_dimensoes() {
    let jpg = encode(&sample_rgb(400, 300).into(), ImageFormat::Jpeg);
    let r = compress_image_impl(&jpg, 80, 0.5, "jpeg").unwrap();
    assert_eq!((r.width(), r.height()), (200, 150));
    assert_eq!((r.original_width(), r.original_height()), (400, 300));
    let decoded = image::load_from_memory(&r.data()).unwrap();
    assert_eq!(decoded.dimensions(), (200, 150));
}

#[test]
fn qualidade_menor_gera_arquivo_menor() {
    let png = encode(&sample_rgb(300, 300).into(), ImageFormat::Png);
    let alta = compress_image_impl(&png, 95, 1.0, "jpeg").unwrap();
    let baixa = compress_image_impl(&png, 20, 1.0, "jpeg").unwrap();
    assert!(baixa.compressed_size() < alta.compressed_size());
}

#[test]
fn transparencia_mantem_png() {
    let img = RgbaImage::from_fn(64, 64, |x, _| Rgba([255, 0, 0, if x < 32 { 0 } else { 255 }]));
    let png = encode(&img.into(), ImageFormat::Png);
    let r = compress_image_impl(&png, 70, 1.0, "auto").unwrap();
    assert!(r.mime() == "image/png");
    let decoded = image::load_from_memory(&r.data()).unwrap();
    assert!(decoded.color().has_alpha());
}

#[test]
fn entrada_invalida_retorna_erro() {
    assert!(compress_image_impl(b"nao sou uma imagem", 70, 1.0, "auto").is_err());
    assert!(compress_image_impl(&[0x89, b'P', b'N', b'G', 0, 0], 70, 1.0, "auto").is_err());
    assert!(compress_image_impl(&encode(&sample_rgb(8, 8).into(), ImageFormat::Png), 70, 1.0, "gif").is_err());
    assert!(compress_pdf_impl(b"%PDF-1.4 lixo", 70, 1.0).is_err());
}

fn check_pdf(bytes: &[u8]) {
    let r = compress_pdf_impl(bytes, 50, 0.5).unwrap();
    assert_eq!(r.mime(), "application/pdf");
    assert!(r.images_found() > 0, "o PDF de teste deveria conter imagens");
    assert!(r.compressed_size() <= bytes.len());
    let original = Document::load_mem(bytes).unwrap();
    let otimizado = Document::load_mem(&r.data()).expect("PDF gerado deve ser válido");
    assert_eq!(original.get_pages().len(), otimizado.get_pages().len());
    println!(
        "{} -> {} bytes, {}/{} imagens otimizadas",
        bytes.len(),
        r.compressed_size(),
        r.images_optimized(),
        r.images_found()
    );
}

#[test]
fn pdf_input() {
    check_pdf(include_bytes!("input.pdf"));
}

#[test]
fn pdf_input2() {
    check_pdf(include_bytes!("input2.pdf"));
}
