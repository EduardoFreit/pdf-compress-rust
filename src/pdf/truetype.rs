//! Subconjunto (subsetting) de fontes TrueType embutidas em PDFs.
//!
//! Mantém a numeração original dos glifos (assim o PDF continua apontando
//! para os mesmos IDs), mas esvazia o contorno de todo glifo não usado e
//! descarta tabelas que um leitor de PDF não consulta (GSUB, GPOS, kern,
//! name, DSIG...). Para fontes "quase completas", o ganho costuma passar de 90%.

use std::collections::BTreeSet;

/// Tabelas mantidas. As demais servem a editores de texto, não à renderização em PDF.
const KEEP: [&[u8; 4]; 15] = [
    b"cmap", b"cvt ", b"fpgm", b"gasp", b"glyf", b"head", b"hhea", b"hmtx", b"loca", b"maxp", b"OS/2", b"post",
    b"prep", b"vhea", b"vmtx",
];

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Localiza as tabelas de uma fonte TrueType simples (não coleção, não CFF).
pub fn tables(data: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    let version = u32_at(data, 0)?;
    if version != 0x0001_0000 && &data[..4] != b"true" {
        return None;
    }
    let num_tables = u16_at(data, 4)? as usize;
    let mut out = Vec::with_capacity(num_tables);
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        let tag: [u8; 4] = data.get(rec..rec + 4)?.try_into().ok()?;
        let offset = u32_at(data, rec + 8)? as usize;
        let length = u32_at(data, rec + 12)? as usize;
        out.push((tag, data.get(offset..offset.checked_add(length)?)?));
    }
    Some(out)
}

pub fn table<'a>(tables: &[([u8; 4], &'a [u8])], tag: &[u8; 4]) -> Option<&'a [u8]> {
    tables.iter().find(|(t, _)| t == tag).map(|(_, d)| *d)
}

/// Gera a fonte reduzida contendo apenas `used` (mais os componentes de glifos compostos).
pub fn subset(data: &[u8], used: &BTreeSet<u16>) -> Option<Vec<u8>> {
    let tables = tables(data)?;
    let head = table(&tables, b"head")?;
    let maxp = table(&tables, b"maxp")?;
    let loca = table(&tables, b"loca")?;
    let glyf = table(&tables, b"glyf")?;
    let num_glyphs = u16_at(maxp, 4)? as usize;
    let long_loca = u16_at(head, 50)? == 1;

    let glyph_range = |gid: usize| -> Option<(usize, usize)> {
        let (a, b) = if long_loca {
            (u32_at(loca, gid * 4)? as usize, u32_at(loca, gid * 4 + 4)? as usize)
        } else {
            (u16_at(loca, gid * 2)? as usize * 2, u16_at(loca, gid * 2 + 2)? as usize * 2)
        };
        (a <= b && b <= glyf.len()).then_some((a, b))
    };

    // Fecha o conjunto: glifos compostos referenciam outros glifos.
    let mut keep = vec![false; num_glyphs];
    let mut stack: Vec<usize> = used.iter().map(|&g| g as usize).chain([0]).filter(|&g| g < num_glyphs).collect();
    while let Some(gid) = stack.pop() {
        if std::mem::replace(&mut keep[gid], true) {
            continue;
        }
        let (a, b) = glyph_range(gid)?;
        let g = &glyf[a..b];
        if g.len() < 10 || (u16_at(g, 0)? as i16) >= 0 {
            continue;
        }
        let mut p = 10;
        loop {
            let flags = u16_at(g, p)?;
            let component = u16_at(g, p + 2)? as usize;
            if component < num_glyphs && !keep[component] {
                stack.push(component);
            }
            p += 4 + if flags & 0x0001 != 0 { 4 } else { 2 };
            p += if flags & 0x0008 != 0 {
                2
            } else if flags & 0x0040 != 0 {
                4
            } else if flags & 0x0080 != 0 {
                8
            } else {
                0
            };
            if flags & 0x0020 == 0 {
                break;
            }
        }
    }

    // Novas tabelas glyf e loca (sempre no formato longo).
    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity((num_glyphs + 1) * 4);
    for (gid, &k) in keep.iter().enumerate() {
        new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
        if k {
            let (a, b) = glyph_range(gid)?;
            new_glyf.extend_from_slice(&glyf[a..b]);
            while new_glyf.len() % 4 != 0 {
                new_glyf.push(0);
            }
        }
    }
    new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());

    let mut new_head = head.to_vec();
    new_head.get_mut(8..12)?.copy_from_slice(&[0; 4]); // checkSumAdjustment
    new_head.get_mut(50..52)?.copy_from_slice(&1u16.to_be_bytes());

    // post versão 3: sem nomes de glifos.
    let new_post = table(&tables, b"post").filter(|p| p.len() >= 32).map(|p| {
        let mut p = p[..32].to_vec();
        p[..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
        p
    });

    let mut out_tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for (tag, d) in &tables {
        if !KEEP.contains(&tag) {
            continue;
        }
        let content = match tag {
            b"glyf" => std::mem::take(&mut new_glyf),
            b"loca" => std::mem::take(&mut new_loca),
            b"head" => std::mem::take(&mut new_head),
            b"post" => match &new_post {
                Some(p) => p.clone(),
                None => continue,
            },
            _ => d.to_vec(),
        };
        out_tables.push((*tag, content));
    }
    out_tables.sort_by(|a, b| a.0.cmp(&b.0));

    let font = assemble(&out_tables);
    (font.len() < data.len()).then_some(font)
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, c| {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        sum.wrapping_add(u32::from_be_bytes(w))
    })
}

fn assemble(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let n = tables.len() as u16;
    let entry_selector = 15 - n.leading_zeros() as u16; // floor(log2(n))
    let search_range = (1u16 << entry_selector) * 16;

    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&(n * 16 - search_range).to_be_bytes());

    let mut offset = 12 + 16 * tables.len();
    let mut head_offset = None;
    for (tag, d) in tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(d).to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        if tag == b"head" {
            head_offset = Some(offset);
        }
        offset += d.len().div_ceil(4) * 4;
    }
    for (_, d) in tables {
        out.extend_from_slice(d);
        out.resize(out.len().div_ceil(4) * 4, 0);
    }
    if let Some(h) = head_offset {
        let adj = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[h + 8..h + 12].copy_from_slice(&adj.to_be_bytes());
    }
    out
}
