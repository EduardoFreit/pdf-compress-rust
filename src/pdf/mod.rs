//! Otimização de PDFs.
//!
//! Etapas (cada uma só é aplicada se for segura para o arquivo em questão):
//! 1. Recompressão das imagens embutidas.
//! 2. Remoção da árvore de estrutura (tags) e das marcações de conteúdo.
//! 3. Remoção de metadados (XMP, PieceInfo, miniaturas).
//! 4. Subconjunto das fontes TrueType: só os glifos usados ficam no arquivo.
//! 5. Deduplicação de streams idênticos e remoção de objetos órfãos.
//! 6. Recompressão de todos os streams (zlib nível máximo ou, opcionalmente, Zopfli).
//! 7. Gravação com object streams e xref comprimida (PDF 1.5).

mod content;
mod fonts;
mod images;
mod truetype;
mod writer;

use lopdf::{Document, Object, ObjectId};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

#[derive(Clone, Copy, Debug)]
pub struct PdfOptions {
    /// Qualidade JPEG (1–100) das imagens recomprimidas.
    pub quality: u8,
    /// Fator de escala das imagens, em (0, 1].
    pub scale: f32,
    /// Remove tags de acessibilidade (árvore de estrutura).
    pub strip_structure: bool,
    /// Remove metadados XMP, dados privados de aplicativos e miniaturas.
    pub strip_metadata: bool,
    /// Mantém apenas os glifos usados nas fontes embutidas.
    pub subset_fonts: bool,
    /// Usa o Zopfli para comprimir os streams: ~10% menor, mas dezenas de vezes mais lento.
    pub max_compression: bool,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self { quality: 70, scale: 1.0, strip_structure: true, strip_metadata: true, subset_fonts: true, max_compression: false }
    }
}

pub struct PdfOutput {
    pub data: Vec<u8>,
    pub images_found: u32,
    pub images_optimized: u32,
    pub fonts_subset: u32,
    pub objects_before: u32,
    pub objects_after: u32,
    /// `true` quando o PDF gerado não ficou menor e o original foi mantido.
    pub kept_original: bool,
}

pub fn compress_pdf(data: &[u8], opts: PdfOptions) -> Result<PdfOutput, String> {
    let mut doc = Document::load_mem(data).map_err(|e| format!("Não foi possível ler o PDF: {e}"))?;
    if doc.is_encrypted() {
        return Err("PDFs criptografados não são suportados".into());
    }
    let objects_before = count_objects(&doc);

    let (images_found, images_optimized) = images::optimize_images(&mut doc, opts.quality, opts.scale);
    if opts.strip_structure {
        content::strip_structure(&mut doc);
    }
    if opts.strip_metadata {
        content::strip_metadata(&mut doc);
    }
    let fonts_subset = if opts.subset_fonts { fonts::subset_fonts(&mut doc) } else { 0 };

    doc.prune_objects();
    dedup_streams(&mut doc);
    doc.prune_objects();
    doc.renumber_objects();
    recompress_streams(&mut doc, opts.max_compression);

    let out = writer::save(&doc, opts.max_compression);
    let objects_after = count_objects(&doc);

    // Garante que o resultado pode ser lido de volta antes de entregá-lo.
    if Document::load_mem(&out).is_err() {
        return Err("O PDF otimizado ficou inválido; o arquivo original foi preservado.".into());
    }

    let kept_original = out.len() >= data.len();
    Ok(PdfOutput {
        data: if kept_original { data.to_vec() } else { out },
        images_found,
        images_optimized: if kept_original { 0 } else { images_optimized },
        fonts_subset: if kept_original { 0 } else { fonts_subset },
        objects_before,
        objects_after: if kept_original { objects_before } else { objects_after },
        kept_original,
    })
}

/// Objetos "reais" (ignora os contêineres ObjStm/XRef que o lopdf mantém após a leitura).
fn count_objects(doc: &Document) -> u32 {
    doc.objects
        .values()
        .filter(|o| {
            !o.as_stream()
                .is_ok_and(|s| matches!(s.dict.get(b"Type").and_then(Object::as_name), Ok(b"ObjStm" | b"XRef")))
        })
        .count() as u32
}

/// Streams idênticos (mesmo dicionário e mesmo conteúdo) passam a ser um só.
fn dedup_streams(doc: &mut Document) {
    let mut by_hash: HashMap<u64, Vec<ObjectId>> = HashMap::new();
    let mut replace: HashMap<ObjectId, ObjectId> = HashMap::new();
    for (&id, obj) in &doc.objects {
        let Object::Stream(s) = obj else { continue };
        let mut h = DefaultHasher::new();
        let mut dict = s.dict.clone();
        dict.remove(b"Length");
        let mut key = Vec::new();
        writer::write_object(&mut key, &Object::Dictionary(dict));
        key.hash(&mut h);
        s.content.hash(&mut h);
        let candidates = by_hash.entry(h.finish()).or_default();
        let same = candidates.iter().find(|&&other| {
            let Ok(Object::Stream(o)) = doc.get_object(other) else { return false };
            o.content == s.content && {
                let mut od = o.dict.clone();
                od.remove(b"Length");
                let mut okey = Vec::new();
                writer::write_object(&mut okey, &Object::Dictionary(od));
                okey == key
            }
        });
        match same {
            Some(&canonical) => {
                replace.insert(id, canonical);
            }
            None => candidates.push(id),
        }
    }
    if replace.is_empty() {
        return;
    }
    fn fix(obj: &mut Object, map: &HashMap<ObjectId, ObjectId>) {
        match obj {
            Object::Reference(id) => {
                if let Some(new) = map.get(id) {
                    *id = *new;
                }
            }
            Object::Array(items) => items.iter_mut().for_each(|i| fix(i, map)),
            Object::Dictionary(d) => d.iter_mut().for_each(|(_, v)| fix(v, map)),
            Object::Stream(s) => s.dict.iter_mut().for_each(|(_, v)| fix(v, map)),
            _ => {}
        }
    }
    for obj in doc.objects.values_mut() {
        fix(obj, &replace);
    }
    doc.trailer.iter_mut().for_each(|(_, v)| fix(v, &replace));
}

/// Comprime streams sem filtro e recomprime streams Flate no nível máximo.
fn recompress_streams(doc: &mut Document, max: bool) {
    let mut jobs: Vec<(ObjectId, Vec<u8>, usize)> = Vec::new();
    for (&id, obj) in &doc.objects {
        let Object::Stream(s) = obj else { continue };
        if matches!(s.dict.get(b"Type").and_then(Object::as_name), Ok(b"ObjStm" | b"XRef")) {
            continue;
        }
        let filters: Vec<&[u8]> = if s.dict.has(b"Filter") {
            match s.filters() {
                Ok(f) => f,
                Err(_) => continue,
            }
        } else {
            Vec::new()
        };
        let raw = match filters.as_slice() {
            [] => s.content.clone(),
            [b"FlateDecode"] if !s.dict.has(b"DecodeParms") => match s.decompressed_content() {
                Ok(d) => d,
                Err(_) => continue,
            },
            _ => continue,
        };
        jobs.push((id, raw, s.content.len()));
    }

    let pack = |(id, raw, current): (ObjectId, Vec<u8>, usize)| {
        let packed = writer::deflate(&raw, max);
        (packed.len() + 20 < current).then_some((id, packed))
    };
    #[cfg(not(target_arch = "wasm32"))]
    let results: Vec<_> = {
        use rayon::prelude::*;
        jobs.into_par_iter().filter_map(pack).collect()
    };
    #[cfg(target_arch = "wasm32")]
    let results: Vec<_> = jobs.into_iter().filter_map(pack).collect();

    for (id, packed) in results {
        if let Ok(Object::Stream(s)) = doc.get_object_mut(id) {
            s.dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
            s.set_content(packed);
        }
    }
}
