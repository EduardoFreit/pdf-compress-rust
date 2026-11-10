//! Limpeza estrutural: remove a árvore de tags (acessibilidade), metadados e
//! as marcações de conteúdo correspondentes nos streams de página.

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashSet;

pub fn resolve_dict<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Dictionary> {
    match doc.dereference(obj).ok()?.1 {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

/// Conteúdo descomprimido de um stream.
pub fn decode_stream_content(doc: &Document, id: ObjectId) -> Option<Vec<u8>> {
    let stream = doc.get_object(id).ok()?.as_stream().ok()?;
    if stream.filters().map(|f| f.is_empty()).unwrap_or(true) {
        Some(stream.content.clone())
    } else {
        stream.decompressed_content().ok()
    }
}

/// Dicionário /Resources de uma página, respeitando a herança via /Parent.
pub fn resources_of_page(doc: &Document, page_id: ObjectId) -> Option<Dictionary> {
    let mut node = doc.get_dictionary(page_id).ok()?;
    for _ in 0..64 {
        if let Some(r) = node.get(b"Resources").ok().and_then(|r| resolve_dict(doc, r)) {
            return Some(r.clone());
        }
        node = resolve_dict(doc, node.get(b"Parent").ok()?)?;
    }
    None
}

/// Remove a árvore de estrutura (tags de acessibilidade). Em PDFs gerados pelo
/// Office ela pode ter dezenas de milhares de objetos.
pub fn strip_structure(doc: &mut Document) {
    if let Ok(catalog) = doc.catalog_mut() {
        catalog.remove(b"StructTreeRoot");
        catalog.remove(b"MarkInfo");
    }
    for obj in doc.objects.values_mut() {
        let dict = match obj {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => continue,
        };
        dict.remove(b"StructParents");
        dict.remove(b"StructParent");
    }
    strip_marked_content(doc);
}

/// Remove metadados XMP, dados privados de aplicativos e miniaturas.
pub fn strip_metadata(doc: &mut Document) {
    for obj in doc.objects.values_mut() {
        let dict = match obj {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => continue,
        };
        dict.remove(b"Metadata");
        dict.remove(b"PieceInfo");
        if dict.get(b"Type").and_then(Object::as_name).ok() == Some(b"Page") {
            dict.remove(b"Thumb");
        }
    }
}

/// Retira os operadores BDC/BMC/EMC (exceto conteúdo opcional, /OC) de todas
/// as páginas e Form XObjects. Sem a árvore de estrutura eles não servem para nada.
fn strip_marked_content(doc: &mut Document) {
    let mut done = HashSet::new();

    // Páginas: os streams de uma página são tratados em sequência, pois uma
    // marcação pode abrir num stream e fechar no seguinte.
    for (_, page_id) in doc.get_pages() {
        let ids: Vec<ObjectId> = match doc.get_dictionary(page_id).ok().and_then(|p| p.get(b"Contents").ok()) {
            Some(Object::Reference(id)) => vec![*id],
            Some(Object::Array(arr)) => arr.iter().filter_map(|o| o.as_reference().ok()).collect(),
            _ => continue,
        };
        if ids.iter().any(|id| done.contains(id)) {
            continue;
        }
        rewrite_streams(doc, &ids);
        done.extend(ids);
    }

    // Form XObjects (inclui aparências de anotações).
    let forms: Vec<ObjectId> = doc
        .objects
        .iter()
        .filter(|(id, o)| {
            !done.contains(*id)
                && o.as_stream().is_ok_and(|s| s.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form"))
        })
        .map(|(id, _)| *id)
        .collect();
    for id in forms {
        rewrite_streams(doc, &[id]);
    }
}

fn rewrite_streams(doc: &mut Document, ids: &[ObjectId]) {
    // Decodifica todos antes de alterar qualquer um: se algum falhar, a página fica como está.
    let mut decoded = Vec::with_capacity(ids.len());
    for &id in ids {
        let Some(data) = decode_stream_content(doc, id) else { return };
        let Ok(content) = Content::decode(&data) else { return };
        // Imagens inline não sobrevivem ao re-encoding do lopdf.
        if content.operations.iter().any(|op| op.operator == "BI") {
            return;
        }
        decoded.push((id, content.operations, data.len()));
    }

    let mut stack: Vec<bool> = Vec::new(); // true = marcação mantida
    let mut results = Vec::with_capacity(decoded.len());
    for (id, ops, _) in decoded {
        let mut kept: Vec<Operation> = Vec::with_capacity(ops.len());
        let mut changed = false;
        for op in ops {
            let keep = match op.operator.as_str() {
                "BDC" | "BMC" => {
                    let is_oc = op.operands.first().and_then(|o| o.as_name().ok()) == Some(b"OC");
                    stack.push(is_oc);
                    is_oc
                }
                "EMC" => stack.pop().unwrap_or(true),
                "MP" | "DP" => false,
                _ => true,
            };
            if keep {
                kept.push(op);
            } else {
                changed = true;
            }
        }
        if !changed {
            continue;
        }
        results.push((id, super::writer::encode_content(&kept)));
    }
    // Se as marcações não fecham corretamente, algo está estranho: não mexe.
    if !stack.is_empty() {
        return;
    }
    for (id, bytes) in results {
        if let Ok(Object::Stream(stream)) = doc.get_object_mut(id) {
            stream.dict.remove(b"Filter");
            stream.dict.remove(b"DecodeParms");
            stream.set_content(bytes);
            stream.allows_compression = true;
        }
    }
}
