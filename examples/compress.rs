//! Otimiza um PDF ou imagem pela linha de comando.
//! Uso: cargo run --release --example compress -- entrada saida [qualidade] [escala] [max]
use projeto_02::{PdfOptions, compress_image_impl, compress_pdf_impl};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (input, output) = (&args[1], &args[2]);
    let quality = args.get(3).and_then(|q| q.parse().ok()).unwrap_or(70);
    let scale = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let data = std::fs::read(input).expect("não foi possível ler a entrada");
    let start = std::time::Instant::now();
    let r = if input.to_lowercase().ends_with(".pdf") {
        compress_pdf_impl(&data, PdfOptions { quality, scale, max_compression: args.get(5).is_some_and(|a| a == "max"), ..Default::default() })
    } else {
        compress_image_impl(&data, quality, scale, "auto")
    }
    .unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(output, r.data()).unwrap();
    println!(
        "{} -> {} bytes ({:.1}% menor) em {:.2?} | imagens {}/{} | fontes reduzidas {} | objetos {} -> {}",
        r.original_size(),
        r.compressed_size(),
        100.0 * (1.0 - r.compressed_size() as f64 / r.original_size() as f64),
        start.elapsed(),
        r.images_optimized(),
        r.images_found(),
        r.fonts_subset(),
        r.objects_before(),
        r.objects_after()
    );
}
