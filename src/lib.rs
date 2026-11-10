//! Otimizador de arquivos (imagens e PDFs) exposto ao JavaScript via WebAssembly.

pub mod image_opt;
pub mod pdf_opt;

use image_opt::OutputFormat;
use wasm_bindgen::prelude::*;

/// Resultado de uma compressão, devolvido ao JavaScript.
#[wasm_bindgen]
pub struct CompressionResult {
    data: Vec<u8>,
    mime: String,
    original_size: usize,
    width: u32,
    height: u32,
    original_width: u32,
    original_height: u32,
    images_found: u32,
    images_optimized: u32,
    kept_original: bool,
}

#[wasm_bindgen]
impl CompressionResult {
    /// Bytes do arquivo otimizado. Cada chamada copia os dados para o JS.
    #[wasm_bindgen(getter)]
    pub fn data(&self) -> Vec<u8> {
        self.data.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn mime(&self) -> String {
        self.mime.clone()
    }
    #[wasm_bindgen(getter, js_name = originalSize)]
    pub fn original_size(&self) -> usize {
        self.original_size
    }
    #[wasm_bindgen(getter, js_name = compressedSize)]
    pub fn compressed_size(&self) -> usize {
        self.data.len()
    }
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.width
    }
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.height
    }
    #[wasm_bindgen(getter, js_name = originalWidth)]
    pub fn original_width(&self) -> u32 {
        self.original_width
    }
    #[wasm_bindgen(getter, js_name = originalHeight)]
    pub fn original_height(&self) -> u32 {
        self.original_height
    }
    #[wasm_bindgen(getter, js_name = imagesFound)]
    pub fn images_found(&self) -> u32 {
        self.images_found
    }
    #[wasm_bindgen(getter, js_name = imagesOptimized)]
    pub fn images_optimized(&self) -> u32 {
        self.images_optimized
    }
    #[wasm_bindgen(getter, js_name = keptOriginal)]
    pub fn kept_original(&self) -> bool {
        self.kept_original
    }
}

/// Comprime uma imagem.
/// - `quality`: qualidade JPEG de 1 a 100.
/// - `scale`: fator de redimensionamento em (0, 1]; 1 mantém o tamanho.
/// - `format`: "auto", "jpeg" ou "png".
#[wasm_bindgen]
pub fn compress_image(data: &[u8], quality: u8, scale: f32, format: &str) -> Result<CompressionResult, JsError> {
    compress_image_impl(data, quality, scale, format).map_err(|e| JsError::new(&e))
}

/// Otimiza um PDF recomprimindo as imagens embutidas e compactando os objetos.
#[wasm_bindgen]
pub fn compress_pdf(data: &[u8], quality: u8, scale: f32) -> Result<CompressionResult, JsError> {
    compress_pdf_impl(data, quality, scale).map_err(|e| JsError::new(&e))
}

pub fn compress_image_impl(data: &[u8], quality: u8, scale: f32, format: &str) -> Result<CompressionResult, String> {
    let format = OutputFormat::parse(format)?;
    let out = image_opt::compress_image(data, quality, scale, format)?;
    Ok(CompressionResult {
        mime: out.mime.to_string(),
        original_size: data.len(),
        width: out.width,
        height: out.height,
        original_width: out.original_width,
        original_height: out.original_height,
        images_found: 1,
        images_optimized: u32::from(!out.kept_original),
        kept_original: out.kept_original,
        data: out.data,
    })
}

pub fn compress_pdf_impl(data: &[u8], quality: u8, scale: f32) -> Result<CompressionResult, String> {
    let out = pdf_opt::compress_pdf(data, quality, scale)?;
    Ok(CompressionResult {
        mime: "application/pdf".into(),
        original_size: data.len(),
        width: 0,
        height: 0,
        original_width: 0,
        original_height: 0,
        images_found: out.images_found,
        images_optimized: out.images_optimized,
        kept_original: out.kept_original,
        data: out.data,
    })
}
