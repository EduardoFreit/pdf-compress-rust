//! Gravação compacta de PDF (1.5+): objetos que não são streams vão dentro de
//! /ObjStm comprimidos e a tabela xref vira um stream binário comprimido.
//! O lopdf 0.35 só sabe gravar cada objeto solto, o que para PDFs com muitos
//! objetos pequenos (ex.: gerados pelo Office) deixa o arquivo várias vezes maior.

use lopdf::{Dictionary, Document, Object, StringFormat};
use std::io::Write;

const OBJECTS_PER_STREAM: usize = 200;

#[derive(Clone, Copy)]
enum Entry {
    Free,
    Offset(usize, u16),
    InStream(u32, u16),
}

/// Comprime no formato zlib. `max` usa o Zopfli: ~10% menor, porém muito mais lento.
pub fn deflate(data: &[u8], max: bool) -> Vec<u8> {
    if max {
        let mut out = Vec::with_capacity(data.len() / 2);
        let opts = zopfli::Options { iteration_count: std::num::NonZeroU64::new(2).unwrap(), ..Default::default() };
        if zopfli::compress(opts, zopfli::Format::Zlib, data, &mut out).is_ok() {
            return out;
        }
    }
    miniz_oxide::deflate::compress_to_vec_zlib(data, 10)
}

pub fn save(doc: &Document, max_compression: bool) -> Vec<u8> {
    let max_id = doc.objects.keys().map(|id| id.0).max().unwrap_or(0) as usize;
    let mut entries = vec![Entry::Free; max_id + 1];
    let mut out = Vec::new();
    let version = if doc.version.as_str() > "1.5" { doc.version.as_str() } else { "1.5" };
    // Comentário binário logo após o cabeçalho, como recomenda a especificação.
    out.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
    out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

    let mut packable = Vec::new();
    for (&(num, generation), obj) in &doc.objects {
        match obj {
            Object::Stream(s) => {
                let skip = matches!(s.dict.get(b"Type").and_then(Object::as_name), Ok(b"ObjStm" | b"XRef"));
                if skip {
                    continue;
                }
                entries[num as usize] = Entry::Offset(out.len(), generation);
                writeln!(out, "{num} {generation} obj").unwrap();
                write_object(&mut out, obj);
                out.extend_from_slice(b"\nendobj\n");
            }
            _ if generation == 0 => packable.push((num, obj)),
            _ => {
                entries[num as usize] = Entry::Offset(out.len(), generation);
                writeln!(out, "{num} {generation} obj").unwrap();
                write_object(&mut out, obj);
                out.extend_from_slice(b"\nendobj\n");
            }
        }
    }

    let mut next_id = max_id as u32 + 1;
    for chunk in packable.chunks(OBJECTS_PER_STREAM) {
        let mut header = Vec::new();
        let mut body = Vec::new();
        for (i, (num, obj)) in chunk.iter().enumerate() {
            write!(header, "{} {} ", num, body.len()).unwrap();
            write_object(&mut body, obj);
            body.push(b'\n');
            entries[*num as usize] = Entry::InStream(next_id, i as u16);
        }
        let first = header.len();
        header.extend_from_slice(&body);
        let content = deflate(&header, max_compression);

        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"ObjStm".to_vec()));
        dict.set("N", chunk.len() as i64);
        dict.set("First", first as i64);
        dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        entries.push(Entry::Offset(out.len(), 0));
        writeln!(out, "{next_id} 0 obj").unwrap();
        write_stream(&mut out, &dict, &content);
        out.extend_from_slice(b"\nendobj\n");
        next_id += 1;
    }

    // Stream de referência cruzada (W [1 4 2]).
    let xref_id = next_id;
    let xref_offset = out.len();
    entries.push(Entry::Offset(xref_offset, 0));
    let mut rows = Vec::with_capacity(entries.len() * 7);
    for (i, e) in entries.iter().enumerate() {
        let (t, a, b): (u8, u32, u16) = match *e {
            Entry::Free => (0, 0, if i == 0 { 0xFFFF } else { 0 }),
            Entry::Offset(off, generation) => (1, off as u32, generation),
            Entry::InStream(s, idx) => (2, s, idx),
        };
        rows.push(t);
        rows.extend_from_slice(&a.to_be_bytes());
        rows.extend_from_slice(&b.to_be_bytes());
    }
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"XRef".to_vec()));
    dict.set("Size", entries.len() as i64);
    dict.set("W", vec![1.into(), 4.into(), 2.into()]);
    dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
    for key in [&b"Root"[..], b"Info", b"ID"] {
        if let Ok(v) = doc.trailer.get(key) {
            dict.set(key.to_vec(), v.clone());
        }
    }
    writeln!(out, "{xref_id} 0 obj").unwrap();
    write_stream(&mut out, &dict, &deflate(&rows, max_compression));
    write!(out, "\nendobj\nstartxref\n{xref_offset}\n%%EOF\n").unwrap();
    out
}

fn write_stream(out: &mut Vec<u8>, dict: &Dictionary, content: &[u8]) {
    let mut dict = dict.clone();
    dict.set("Length", content.len() as i64);
    write_dict(out, &dict);
    out.extend_from_slice(b"stream\n");
    out.extend_from_slice(content);
    out.extend_from_slice(b"\nendstream");
}

/// Objetos que começam com um delimitador não precisam de espaço antes.
pub fn starts_with_delimiter(obj: &Object) -> bool {
    matches!(
        obj,
        Object::Name(_) | Object::String(..) | Object::Array(_) | Object::Dictionary(_) | Object::Stream(_)
    )
}

/// Objetos cujo último caractere é "regular" (exige espaço antes do próximo token regular).
pub fn ends_regular(obj: &Object) -> bool {
    matches!(
        obj,
        Object::Null | Object::Boolean(_) | Object::Integer(_) | Object::Real(_) | Object::Name(_) | Object::Reference(_)
    )
}

/// Codifica operações de conteúdo sem espaços supérfluos, ex.: `[(AT)-7(E)]TJ`.
pub fn encode_content(ops: &[lopdf::content::Operation]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        let mut prev_regular = false;
        for operand in &op.operands {
            if prev_regular && !starts_with_delimiter(operand) {
                out.push(b' ');
            }
            write_object(&mut out, operand);
            prev_regular = ends_regular(operand);
        }
        if prev_regular {
            out.push(b' ');
        }
        out.extend_from_slice(op.operator.as_bytes());
    }
    out
}

fn write_dict(out: &mut Vec<u8>, dict: &Dictionary) {
    out.extend_from_slice(b"<<");
    for (k, v) in dict.iter() {
        write_name(out, k);
        if !starts_with_delimiter(v) {
            out.push(b' ');
        }
        write_object(out, v);
    }
    out.extend_from_slice(b">>");
}

pub fn write_object(out: &mut Vec<u8>, obj: &Object) {
    match obj {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(i) => write!(out, "{i}").unwrap(),
        Object::Real(r) => write_real(out, *r),
        Object::Name(n) => write_name(out, n),
        Object::String(s, format) => write_string(out, s, format),
        Object::Array(items) => {
            out.push(b'[');
            let mut prev_regular = false;
            for item in items {
                if prev_regular && !starts_with_delimiter(item) {
                    out.push(b' ');
                }
                write_object(out, item);
                prev_regular = ends_regular(item);
            }
            out.push(b']');
        }
        Object::Dictionary(d) => write_dict(out, d),
        Object::Stream(s) => write_stream(out, &s.dict, &s.content),
        Object::Reference((n, g)) => write!(out, "{n} {g} R").unwrap(),
    }
}

fn write_real(out: &mut Vec<u8>, r: f32) {
    if !r.is_finite() {
        out.push(b'0');
    } else if r.fract() == 0.0 && r.abs() < 1e9 {
        write!(out, "{}", r as i64).unwrap();
    } else {
        // Display de float em Rust nunca usa notação científica (inválida em PDF).
        let s = format!("{r}");
        let s = s.strip_prefix("0.").map(|rest| format!(".{rest}")).unwrap_or(s);
        let s = s.strip_prefix("-0.").map(|rest| format!("-.{rest}")).unwrap_or(s);
        out.extend_from_slice(s.as_bytes());
    }
}

fn write_name(out: &mut Vec<u8>, name: &[u8]) {
    out.push(b'/');
    for &b in name {
        if b" \t\n\r\x0C()<>[]{}/%#".contains(&b) || !(33..=126).contains(&b) {
            write!(out, "#{b:02X}").unwrap();
        } else {
            out.push(b);
        }
    }
}

fn write_string(out: &mut Vec<u8>, s: &[u8], format: &StringFormat) {
    match format {
        StringFormat::Hexadecimal => {
            out.push(b'<');
            for b in s {
                write!(out, "{b:02X}").unwrap();
            }
            out.push(b'>');
        }
        StringFormat::Literal => {
            out.push(b'(');
            for &b in s {
                match b {
                    b'(' | b')' | b'\\' => {
                        out.push(b'\\');
                        out.push(b);
                    }
                    b'\r' => out.extend_from_slice(b"\\r"),
                    _ => out.push(b),
                }
            }
            out.push(b')');
        }
    }
}
