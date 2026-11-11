//! Mostra quanto espaço cada tipo de objeto ocupa num PDF.
//! Uso: cargo run --release --example inspect -- arquivo.pdf
use lopdf::{Document, Object};
use std::collections::BTreeMap;

fn main() {
    let path = std::env::args().nth(1).expect("informe o caminho do PDF");
    let data = std::fs::read(&path).unwrap();
    let doc = Document::load_mem(&data).unwrap();
    println!("{} bytes, versão {}, {} páginas, {} objetos", data.len(), doc.version, doc.get_pages().len(), doc.objects.len());
    let mut by_kind: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut images = vec![];
    for (id, obj) in &doc.objects {
        let (kind, size) = match obj {
            Object::Stream(s) => {
                let name = |k: &[u8]| s.dict.get(k).and_then(Object::as_name).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default();
                let filters = s.filters().map(|f| f.iter().map(|x| String::from_utf8_lossy(x).into_owned()).collect::<Vec<_>>().join("+")).unwrap_or_default();
                let mut kind = format!("stream {}/{} [{}]", name(b"Type"), name(b"Subtype"), filters);
                if s.dict.has(b"Length1") || s.dict.has(b"Length2") || s.dict.has(b"Length3") { kind = format!("fonte {} [{}]", if s.dict.has(b"Length2") {"Type1"} else {"TrueType"}, filters); }
                if name(b"Subtype") == "Image" {
                    let g = |k: &[u8]| s.dict.get(k).ok().map(|o| format!("{:?}", o)).unwrap_or_default();
                    images.push((s.content.len(), *id, g(b"Width"), g(b"Height"), g(b"ColorSpace"), g(b"BitsPerComponent"), filters.clone(), s.dict.has(b"SMask")));
                }
                (kind, s.content.len())
            }
            Object::Dictionary(d) => (format!("dict {}", d.get(b"Type").and_then(Object::as_name).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default()), 0),
            o => (o.enum_variant().to_string(), 0),
        };
        let e = by_kind.entry(kind).or_default();
        e.0 += 1;
        e.1 += size;
    }
    let mut v: Vec<_> = by_kind.into_iter().collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.1 .1));
    for (k, (n, s)) in v.iter().take(25) { println!("{s:>10} bytes  {n:>5}x  {k}"); }
    images.sort();
    images.reverse();
    println!("\nMaiores imagens:");
    for i in images.iter().take(15) { println!("{:?}", i); }
}
