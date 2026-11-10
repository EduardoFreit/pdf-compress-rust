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
    assert!(compress_pdf_impl(b"%PDF-1.4 lixo", Default::default()).is_err());
}

fn check_pdf(bytes: &[u8]) {
    let r = compress_pdf_impl(bytes, projeto_02::PdfOptions { quality: 50, scale: 0.5, ..Default::default() }).unwrap();
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

/// PDF "marcado" como os gerados pelo Office: árvore de estrutura com muitos
/// elementos, marcações BDC/EMC e um bloco de conteúdo opcional (/OC).
fn tagged_pdf() -> Vec<u8> {
    use lopdf::{Object, Stream, dictionary};
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
    let mut content = Vec::new();
    for i in 0..300 {
        content.extend_from_slice(
            format!("/P <</MCID {i}>> BDC BT /F1 10 Tf 20 {} Td (Linha {i}) Tj ET EMC\n", 20 + i * 2).as_bytes(),
        );
    }
    content.extend_from_slice(b"/OC /Camada1 BDC 0 0 10 10 re f EMC\n");
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "StructParents" => 0,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1 }),
    );
    let root_elem = doc.new_object_id();
    let kids: Vec<Object> = (0..300)
        .map(|i| doc.add_object(dictionary! { "Type" => "StructElem", "S" => "P", "P" => root_elem, "Pg" => page_id, "K" => i }).into())
        .collect();
    doc.objects.insert(root_elem, Object::Dictionary(dictionary! { "Type" => "StructElem", "S" => "Document", "K" => kids }));
    let tree = doc.add_object(dictionary! { "Type" => "StructTreeRoot", "K" => root_elem });
    let xmp = doc.add_object(Stream::new(dictionary! { "Type" => "Metadata", "Subtype" => "XML" }, vec![b' '; 4000]));
    let catalog = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "StructTreeRoot" => tree,
        "MarkInfo" => dictionary! { "Marked" => true },
        "Metadata" => xmp,
    });
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

#[test]
fn pdf_remove_estrutura_e_metadados() {
    let original = tagged_pdf();
    let r = compress_pdf_impl(&original, Default::default()).unwrap();
    assert!(!r.kept_original());
    assert!(r.compressed_size() * 3 < original.len(), "{} -> {}", original.len(), r.compressed_size());
    assert!(r.objects_after() < 10);

    let doc = Document::load_mem(&r.data()).unwrap();
    let catalog = doc.catalog().unwrap();
    assert!(!catalog.has(b"StructTreeRoot") && !catalog.has(b"MarkInfo") && !catalog.has(b"Metadata"));
    let page = *doc.get_pages().get(&1).unwrap();
    assert!(!doc.get_dictionary(page).unwrap().has(b"StructParents"));
    let content = String::from_utf8(doc.get_page_content(page).unwrap()).unwrap();
    assert!(!content.contains("MCID"), "marcações de estrutura deveriam sair");
    assert!(content.contains("(Linha 299)Tj"));
    assert!(content.contains("/OC/Camada1 BDC"), "conteúdo opcional deve ser mantido: {}", &content[content.len() - 80..]);
    assert_eq!(content.matches("EMC").count(), 1);
}

#[test]
fn pdf_opcoes_desligadas_mantem_estrutura() {
    let original = tagged_pdf();
    let opts = projeto_02::PdfOptions {
        strip_structure: false,
        strip_metadata: false,
        subset_fonts: false,
        ..Default::default()
    };
    let r = compress_pdf_impl(&original, opts).unwrap();
    let doc = Document::load_mem(&r.data()).unwrap();
    assert!(doc.catalog().unwrap().has(b"StructTreeRoot"));
    assert!(doc.catalog().unwrap().has(b"Metadata"));
    // Mesmo sem remover nada, a gravação com object streams já reduz o arquivo.
    assert!(r.compressed_size() < original.len());
}

#[test]
fn pdf_input() {
    check_pdf(include_bytes!("input.pdf"));
}

#[test]
fn pdf_input2() {
    check_pdf(include_bytes!("input2.pdf"));
}
