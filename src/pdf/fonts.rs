//! Descobre quais glifos de cada fonte TrueType são realmente usados no PDF
//! e substitui a fonte embutida por um subconjunto.
//!
//! A análise percorre o conteúdo de todas as páginas, Form XObjects,
//! aparências de anotações e padrões. Ao menor sinal de algo que não sabemos
//! analisar com segurança, a fonte (ou todo o processo) fica intocada.

use super::content::{decode_stream_content, resolve_dict, resources_of_page};
use super::truetype;
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone)]
enum Kind {
    /// Fonte simples (1 byte por caractere) com codificação WinAnsi ou simbólica.
    Simple,
    /// Fonte CID com Identity-H/V; `None` = CIDToGIDMap Identity.
    Cid(Option<std::rc::Rc<Vec<u8>>>),
}

#[derive(Clone)]
struct FontRef {
    file: ObjectId,
    kind: Kind,
}

#[derive(Default)]
struct Usage {
    codes: BTreeSet<u8>,
    gids: BTreeSet<u16>,
}

struct Walker<'a> {
    doc: &'a Document,
    usage: HashMap<ObjectId, Usage>,
    unsafe_files: HashSet<ObjectId>,
    seen_fonts: HashSet<ObjectId>,
    visiting: HashSet<ObjectId>,
}

/// Retorna quantas fontes foram reduzidas.
pub fn subset_fonts(doc: &mut Document) -> u32 {
    let Some((usage, unsafe_files)) = collect(doc) else { return 0 };
    let mut count = 0;
    for (file_id, usage) in usage {
        if unsafe_files.contains(&file_id) {
            continue;
        }
        let Ok(stream) = doc.get_object(file_id).and_then(Object::as_stream) else { continue };
        let data = if stream.filters().map(|f| f.is_empty()).unwrap_or(true) {
            stream.content.clone()
        } else {
            match stream.decompressed_content() {
                Ok(d) => d,
                Err(_) => continue,
            }
        };
        let Some(gids) = glyphs_for(&data, &usage) else { continue };
        let Some(font) = truetype::subset(&data, &gids) else { continue };
        if let Ok(Object::Stream(stream)) = doc.get_object_mut(file_id) {
            stream.dict.remove(b"Filter");
            stream.dict.remove(b"DecodeParms");
            stream.dict.set("Length1", font.len() as i64);
            stream.set_content(font);
            stream.allows_compression = true;
            count += 1;
        }
    }
    count
}

fn collect(doc: &Document) -> Option<(HashMap<ObjectId, Usage>, HashSet<ObjectId>)> {
    let mut w = Walker {
        doc,
        usage: HashMap::new(),
        unsafe_files: HashSet::new(),
        seen_fonts: HashSet::new(),
        visiting: HashSet::new(),
    };

    for (_, page_id) in doc.get_pages() {
        let page = doc.get_dictionary(page_id).ok()?;
        let resources = resources_of_page(doc, page_id);
        let mut state = vec![None];
        for content_id in page_contents(page) {
            let data = decode_stream_content(doc, content_id)?;
            w.walk(&data, resources.as_ref(), &mut state)?;
        }
        // Aparências de anotações.
        if let Ok(annots) = page.get(b"Annots").and_then(|a| doc.dereference(a)).and_then(|(_, a)| a.as_array()) {
            for annot in annots {
                let Some(annot) = resolve_dict(doc, annot) else { continue };
                let Some(ap) = annot.get(b"AP").ok().and_then(|o| resolve_dict(doc, o)) else { continue };
                for (_, appearance) in ap.iter() {
                    w.walk_appearance(appearance, None)?;
                }
            }
        }
    }

    // Fontes do formulário (AcroForm) podem ser usadas para gerar texto novo: não mexer.
    if let Some(dr) = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok())
        .and_then(|a| resolve_dict(doc, a))
        .and_then(|a| a.get(b"DR").ok())
        .and_then(|d| resolve_dict(doc, d))
        .and_then(|d| d.get(b"Font").ok())
        .and_then(|f| resolve_dict(doc, f))
    {
        for (_, font) in dr.iter() {
            if let Some(file) = resolve_dict(doc, font).and_then(|f| font_file(doc, f)) {
                w.unsafe_files.insert(file);
            }
        }
    }

    // Fontes que não encontramos no percurso podem ser usadas em lugares que
    // não analisamos; seus arquivos ficam intactos.
    for (id, obj) in &doc.objects {
        let Object::Dictionary(d) = obj else { continue };
        let is_font = d.get(b"Type").and_then(Object::as_name).ok() == Some(b"Font");
        let subtype = d.get(b"Subtype").and_then(Object::as_name).ok();
        if is_font && matches!(subtype, Some(b"TrueType" | b"Type0")) && !w.seen_fonts.contains(id) {
            w.unsafe_files.extend(font_file(doc, d));
        }
    }

    Some((w.usage, w.unsafe_files))
}

fn page_contents(page: &Dictionary) -> Vec<ObjectId> {
    match page.get(b"Contents") {
        Ok(Object::Reference(id)) => vec![*id],
        Ok(Object::Array(arr)) => arr.iter().filter_map(|o| o.as_reference().ok()).collect(),
        _ => vec![],
    }
}

impl Walker<'_> {
    fn walk_appearance(&mut self, obj: &Object, font: Option<FontRef>) -> Option<()> {
        match obj {
            Object::Reference(id) => match self.doc.get_object(*id).ok()? {
                Object::Stream(_) => self.walk_form(*id, None, font),
                Object::Dictionary(d) => {
                    for (_, v) in d.iter() {
                        self.walk_appearance(v, font.clone())?;
                    }
                    Some(())
                }
                _ => Some(()),
            },
            Object::Dictionary(d) => {
                for (_, v) in d.iter() {
                    self.walk_appearance(v, font.clone())?;
                }
                Some(())
            }
            _ => Some(()),
        }
    }

    /// Percorre um Form XObject (ou padrão) com seus próprios recursos.
    fn walk_form(&mut self, id: ObjectId, parent_resources: Option<&Dictionary>, font: Option<FontRef>) -> Option<()> {
        if !self.visiting.insert(id) {
            return Some(()); // já analisado (ou recursão)
        }
        let doc = self.doc;
        let stream = doc.get_object(id).ok()?.as_stream().ok()?;
        let own = stream.dict.get(b"Resources").ok().and_then(|r| resolve_dict(doc, r)).cloned();
        let resources = own.as_ref().or(parent_resources).cloned();
        let data = decode_stream_content(doc, id)?;
        let mut state = vec![font];
        self.walk(&data, resources.as_ref(), &mut state)
    }

    fn walk(&mut self, data: &[u8], resources: Option<&Dictionary>, state: &mut Vec<Option<FontRef>>) -> Option<()> {
        let doc = self.doc;
        let content = lopdf::content::Content::decode(data).ok()?;
        let lookup = |category: &[u8], name: &[u8]| -> Option<&Object> {
            let cat = resolve_dict(doc, resources?.get(category).ok()?)?;
            cat.get(name).ok()
        };

        // Padrões de ladrilho (PatternType 1) têm conteúdo próprio.
        if let Some(patterns) = resources.and_then(|r| r.get(b"Pattern").ok()).and_then(|p| resolve_dict(doc, p)) {
            let is_tiling = |id: ObjectId| {
                matches!(doc.get_object(id), Ok(Object::Stream(s))
                    if s.dict.get(b"PatternType").and_then(Object::as_i64).ok() == Some(1))
            };
            let tiling: Vec<ObjectId> =
                patterns.iter().filter_map(|(_, p)| p.as_reference().ok()).filter(|&id| is_tiling(id)).collect();
            for id in tiling {
                self.walk_form(id, resources, None)?;
            }
        }

        for op in &content.operations {
            match op.operator.as_str() {
                "q" => {
                    let top = state.last().cloned().flatten();
                    state.push(top);
                }
                "Q" => {
                    if state.len() > 1 {
                        state.pop();
                    }
                }
                "Tf" => {
                    let name = op.operands.first()?.as_name().ok()?;
                    let font_obj = lookup(b"Font", name)?;
                    let font = self.font_ref(font_obj)?;
                    *state.last_mut()? = font;
                }
                "Tj" | "'" | "\"" | "TJ" => {
                    let Some(font) = state.last().cloned().flatten() else { continue };
                    for operand in &op.operands {
                        match operand {
                            Object::String(s, _) => self.record(&font, s),
                            Object::Array(items) => {
                                for item in items {
                                    if let Object::String(s, _) = item {
                                        self.record(&font, s);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "Do" => {
                    let name = op.operands.first()?.as_name().ok()?;
                    let Some(Object::Reference(id)) = lookup(b"XObject", name) else { continue };
                    let is_form = doc
                        .get_object(*id)
                        .ok()
                        .and_then(|o| o.as_stream().ok())
                        .is_some_and(|s| s.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form"));
                    if is_form {
                        let font = state.last().cloned().flatten();
                        self.walk_form(*id, resources, font)?;
                    }
                }
                _ => {}
            }
        }
        Some(())
    }

    fn record(&mut self, font: &FontRef, s: &[u8]) {
        let usage = self.usage.entry(font.file).or_default();
        match &font.kind {
            Kind::Simple => usage.codes.extend(s.iter().copied()),
            Kind::Cid(map) => {
                // Códigos de 2 bytes (big-endian); um byte final sem par é ignorado.
                for i in (0..s.len() / 2).map(|i| i * 2) {
                    let cid = u16::from_be_bytes([s[i], s[i + 1]]);
                    let gid = match map {
                        None => cid,
                        Some(m) => {
                            let i = cid as usize * 2;
                            m.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]])).unwrap_or(0)
                        }
                    };
                    usage.gids.insert(gid);
                }
            }
        }
    }

    /// Interpreta um dicionário de fonte. `Some(None)` = fonte que não reduzimos
    /// (Type1, Type3, CFF...), o que não impede o restante da análise.
    fn font_ref(&mut self, obj: &Object) -> Option<Option<FontRef>> {
        let doc = self.doc;
        if let Ok(id) = obj.as_reference() {
            self.seen_fonts.insert(id);
        }
        let font = resolve_dict(doc, obj)?;
        let subtype = font.get(b"Subtype").and_then(Object::as_name).ok();
        let file = font_file(doc, font);

        let kind = match subtype {
            Some(b"TrueType") => {
                let flags = font_descriptor(doc, font)
                    .and_then(|d| d.get(b"Flags").ok())
                    .and_then(|f| f.as_i64().ok())
                    .unwrap_or(0);
                let symbolic = flags & 4 != 0;
                let encoding_ok = match font.get(b"Encoding").ok().and_then(|e| doc.dereference(e).ok()).map(|(_, e)| e) {
                    Some(Object::Name(n)) => n == b"WinAnsiEncoding",
                    Some(Object::Dictionary(d)) => {
                        !d.has(b"Differences")
                            && d.get(b"BaseEncoding").and_then(Object::as_name).ok() == Some(b"WinAnsiEncoding")
                    }
                    None => symbolic,
                    _ => false,
                };
                if !encoding_ok {
                    if let Some(f) = file {
                        self.unsafe_files.insert(f);
                    }
                    return Some(None);
                }
                Kind::Simple
            }
            Some(b"Type0") => {
                let encoding = font.get(b"Encoding").and_then(Object::as_name).ok();
                let descendant = font
                    .get(b"DescendantFonts")
                    .ok()
                    .and_then(|d| doc.dereference(d).ok())
                    .and_then(|(_, d)| d.as_array().ok())
                    .and_then(|a| a.first())
                    .and_then(|d| resolve_dict(doc, d));
                let map = descendant.and_then(|d| d.get(b"CIDToGIDMap").ok());
                let map = match map.map(|m| doc.dereference(m).map(|(_, m)| m)) {
                    None | Some(Ok(Object::Name(_))) => Some(None),
                    Some(Ok(Object::Stream(s))) => s.decompressed_content().ok().map(|d| Some(std::rc::Rc::new(d))),
                    _ => None,
                };
                let ok = matches!(encoding, Some(b"Identity-H" | b"Identity-V"))
                    && descendant.and_then(|d| d.get(b"Subtype").and_then(Object::as_name).ok()) == Some(b"CIDFontType2");
                match (ok, map) {
                    (true, Some(map)) => Kind::Cid(map),
                    _ => {
                        if let Some(f) = file {
                            self.unsafe_files.insert(f);
                        }
                        return Some(None);
                    }
                }
            }
            Some(b"Type3") => {
                // Glifos Type3 podem desenhar texto com outras fontes; não arriscamos.
                if font.get(b"Resources").ok().and_then(|r| resolve_dict(doc, r)).is_some_and(|r| r.has(b"Font")) {
                    return None;
                }
                return Some(None);
            }
            _ => return Some(None),
        };
        Some(file.map(|file| FontRef { file, kind }))
    }
}

fn font_descriptor<'a>(doc: &'a Document, font: &'a Dictionary) -> Option<&'a Dictionary> {
    if let Some(d) = font.get(b"FontDescriptor").ok().and_then(|d| resolve_dict(doc, d)) {
        return Some(d);
    }
    let descendant = font
        .get(b"DescendantFonts")
        .ok()
        .and_then(|d| doc.dereference(d).ok())
        .and_then(|(_, d)| d.as_array().ok())
        .and_then(|a| a.first())
        .and_then(|d| resolve_dict(doc, d))?;
    descendant.get(b"FontDescriptor").ok().and_then(|d| resolve_dict(doc, d))
}

/// ID do stream /FontFile2 (TrueType) associado a um dicionário de fonte.
fn font_file(doc: &Document, font: &Dictionary) -> Option<ObjectId> {
    font_descriptor(doc, font)?.get(b"FontFile2").ok()?.as_reference().ok()
}

/// Tabela WinAnsi → Unicode para os códigos 0x80–0x9F (o resto coincide com Latin-1).
const WIN_ANSI_80: [u16; 32] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0, 0,
    0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
];

fn win_ansi(code: u8) -> u32 {
    match code {
        0x80..=0x9F => match WIN_ANSI_80[(code - 0x80) as usize] {
            0 => code as u32,
            u => u as u32,
        },
        _ => code as u32,
    }
}

/// Converte códigos de fontes simples em IDs de glifo usando o cmap da própria
/// fonte. Consulta todas as subtabelas para cobrir os diferentes caminhos que
/// leitores de PDF usam.
fn glyphs_for(data: &[u8], usage: &Usage) -> Option<BTreeSet<u16>> {
    let mut gids = usage.gids.clone();
    if usage.codes.is_empty() {
        return Some(gids);
    }
    let tables = truetype::tables(data)?;
    let cmap = ttf_parser::cmap::Table::parse(truetype::table(&tables, b"cmap")?)?;
    for sub in cmap.subtables {
        for &code in &usage.codes {
            let mut candidates = vec![code as u32];
            if sub.is_unicode() {
                candidates.push(win_ansi(code));
            } else if sub.platform_id == ttf_parser::PlatformId::Windows && sub.encoding_id == 0 {
                candidates.push(0xF000 + code as u32);
            }
            for c in candidates {
                if let Some(g) = sub.glyph_index(c) {
                    gids.insert(g.0);
                }
            }
        }
    }
    Some(gids)
}
