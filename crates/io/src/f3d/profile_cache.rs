//! Resolve a stale profile through its explicitly named current snapshot.
//! Snapshot curve tags select editable source pieces; coordinates are never
//! substituted for source geometry. Unsupported snapshot topology stays explicit.
use super::history::{empty, finish, payload, references, require_class, zeros};
use super::*;
use cadmpeg_asm::{
    asm_header,
    sab::{self, Record as NativeRecord, Token},
};
use cadmpeg_core::decode::{DecodeArena, DecodeContext, DecodePolicy};

const HANDLE: &str = "C46D3EEB-40E1-43FA-83DC-9CB204335417";
const MAP: &str = "40DCC3F41D-E94F-B3B1-3E95-9B6ABE572C";
const LIST: &str = "E55D0457-898F-4A8E-B1A5-C85BDBA8794F";
const CONTAINER: &str = "CD57BC48-50EC-47DC-975A-FB6DEA72F4DA";

pub(super) fn resolve(design: &Design, profile: u64, geometry: u64, curves: &BTreeMap<(u64, (u64, u64)), u64>) -> Result<Vec<Vec<ProfileLoop>>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| resolve_inner(design, profile, geometry, curves)))
        .map_err(|_| invalid("profile snapshot decoder stopped unexpectedly"))?
}

fn resolve_inner(design: &Design, profile: u64, geometry: u64, curves: &BTreeMap<(u64, (u64, u64)), u64>) -> Result<Vec<Vec<ProfileLoop>>> {
    let records = Records::new(design);
    let classes: BTreeMap<_, _> = design.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
    let mut r = payload(records.primary(profile)?, profile)?;
    empty(&mut r)?;
    zeros(&mut r, 2)?;
    r.reference()?; // tracking has been fully framed by the caller
    if r.u32()? != 1 {
        return Err(invalid("unsupported snapshot source count"));
    }
    let asset = r.wide_string()?;
    if r.wide_string()? != geometry.to_string() || r.u32()? != 1 {
        return Err(invalid("inconsistent snapshot sketch source"));
    }
    zeros(&mut r, 4)?;
    let handles = references(&mut r)?;
    let [handle] = handles.as_slice() else {
        return Err(invalid("profile requires one current snapshot"));
    };
    require_class(&classes, *handle, HANDLE, 1)?;
    let mut r = payload(records.primary(*handle)?, *handle)?;
    empty(&mut r)?;
    zeros(&mut r, 2)?;
    let local = r.u64()?;
    if local == 0 || r.wide_string()? != asset {
        return Err(invalid("snapshot belongs to another asset"));
    }
    if r.wide_string()?.is_empty() || r.u32()? != 2 {
        return Err(invalid("unsupported snapshot context"));
    }
    zeros(&mut r, 5)?;
    finish(&r)?;

    let (blob, ordinal, count) = snapshot_map(&records, &classes, local)?;
    let (asset_path, _) = design.path.rsplit_once('/').ok_or_else(|| invalid("missing snapshot asset path"))?;
    let path = format!("{asset_path}/Breps.BlobParts/{blob}");
    let bytes = design.archive.as_deref().ok_or_else(|| invalid("missing source archive"))?;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| invalid(&e.to_string()))?;
    let bytes = archive::read(&mut zip, &path)?;
    let arena = DecodeArena::new();
    let mut policy = DecodePolicy::service();
    policy.limits.max_input_bytes = MAX_STREAM as u64;
    policy.limits.max_retained_bytes = 256 << 20;
    let (ctx, _) = DecodeContext::from_root_bytes(&bytes, &arena, &policy).map_err(|e| invalid(&e.to_string()))?;
    let header = asm_header::parse(&bytes).ok_or_else(|| invalid("missing snapshot ASM header"))?;
    if header.metadata.has_history_partition() || header.metadata.entity_count != Some(count as u64 + 1) {
        return Err(invalid("snapshot root count disagrees with its identity table"));
    }
    let start = asm_header::record_stream_start_with_header(&bytes, &header).ok_or_else(|| invalid("missing snapshot record stream"))?;
    let framed =
        sab::frame_history(&ctx, &bytes, start, bytes.len(), header.width, header.metadata.entity_count).map_err(|e| invalid(&e.to_string()))?;
    if framed.last().is_none_or(|r| r.name != "End-of-ASM-data") {
        return Err(invalid("incomplete snapshot record stream"));
    }
    selected_loop(&framed, ordinal + 1, geometry, curves).map(|boundary| vec![vec![boundary]])
}

fn snapshot_map(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, local: u64) -> Result<(String, usize, usize)> {
    let mut selected = None;
    for class in records.design.classes.iter().filter(|c| c.id == MAP) {
        if class.version != 1 {
            return Err(invalid("unsupported snapshot map version"));
        }
        for id in &class.objects {
            let mut r = payload(records.primary(*id)?, *id)?;
            empty(&mut r)?;
            zeros(&mut r, 2)?;
            let list = r.reference()?;
            require_class(classes, list, LIST, 0)?;
            let pairs = r.count(16)?;
            r.take(pairs * 16)?; // optional model-body bindings, not root order
            let container = r.reference()?;
            require_class(classes, container, CONTAINER, 4)?;
            zeros(&mut r, 1)?;
            let name = r.wide_string()?;
            finish(&r)?;
            let mut r = payload(records.primary(list)?, list)?;
            empty(&mut r)?;
            zeros(&mut r, 2)?;
            let count = r.count(8)?;
            if count > 100_000 {
                return Err(invalid("excessive snapshot roots"));
            }
            r.take(count * 8)?; // process-local handles, never used as identities
            let next = r.u64()?;
            zeros(&mut r, 2)?;
            if r.count(8)? != count {
                return Err(invalid("inconsistent snapshot identity count"));
            }
            let mut seen = BTreeSet::new();
            for ordinal in 0..count {
                let identity = r.u64()?;
                if identity == 0 || identity >= next || !seen.insert(identity) {
                    return Err(invalid("invalid snapshot identity table"));
                }
                if identity == local {
                    if !name.starts_with("BREP.") || !name.ends_with(".smb") || name.contains(['/', '\\', ':']) {
                        return Err(invalid("invalid snapshot blob name"));
                    }
                    if selected.replace((name.clone(), ordinal, count)).is_some() {
                        return Err(invalid("ambiguous snapshot identity"));
                    }
                }
            }
            finish(&r)?;
        }
    }
    selected.ok_or_else(|| invalid("missing current snapshot identity"))
}

fn record<'a>(records: &'a [NativeRecord], id: usize, name: &str) -> Result<&'a NativeRecord> {
    records.get(id).filter(|r| r.index == id && r.name == name).ok_or_else(|| invalid("invalid snapshot topology reference"))
}

fn link(record: &NativeRecord, slot: usize) -> Result<usize> {
    record.ref_at(slot).and_then(|id| usize::try_from(id).ok()).ok_or_else(|| invalid("missing snapshot topology link"))
}

fn null(record: &NativeRecord, slot: usize) -> Result<()> {
    if matches!(record.chunk(slot), Some(Token::Ref(-1))) { Ok(()) } else { Err(invalid("unsupported compound snapshot selection")) }
}

fn selected_loop(records: &[NativeRecord], root: usize, geometry: u64, curves: &BTreeMap<(u64, (u64, u64)), u64>) -> Result<ProfileLoop> {
    let body = record(records, root, "body")?;
    null(body, 4)?;
    let lump_id = link(body, 3)?;
    let lump = record(records, lump_id, "lump")?;
    null(lump, 3)?;
    if link(lump, 5)? != root {
        return Err(invalid("snapshot lump belongs to another body"));
    }
    let shell_id = link(lump, 4)?;
    let shell = record(records, shell_id, "shell")?;
    null(shell, 3)?;
    if link(shell, 7)? != lump_id {
        return Err(invalid("snapshot shell belongs to another lump"));
    }
    let face_id = link(shell, 5)?;
    let face = record(records, face_id, "face")?;
    null(face, 3)?;
    if link(face, 5)? != shell_id {
        return Err(invalid("snapshot face belongs to another shell"));
    }
    let loop_id = link(face, 4)?;
    let boundary = record(records, loop_id, "loop")?;
    null(boundary, 3)?;
    if link(boundary, 5)? != face_id {
        return Err(invalid("snapshot loop belongs to another face"));
    }
    let first = link(boundary, 4)?;
    let mut id = first;
    let mut seen = BTreeSet::new();
    let mut pieces_seen = BTreeSet::new();
    let mut result = ProfileLoop { curves: Vec::new(), reversed: Vec::new(), pieces: Vec::new(), outer: true };
    loop {
        if seen.len() >= 512 || !seen.insert(id) {
            return Err(invalid("cyclic or excessive snapshot loop"));
        }
        let coedge = record(records, id, "coedge")?;
        if link(coedge, 8)? != loop_id {
            return Err(invalid("snapshot coedge belongs to another loop"));
        }
        let tag = record(records, link(coedge, 0)?, "ATTRIB_CUSTOM-attrib")?;
        null(tag, 2)?;
        null(tag, 3)?;
        if link(tag, 4)? != id
            || tag.chunk_len() != 10
            || !matches!(tag.chunk(5), Some(Token::Str(name)) if name == "sketch_attrib_def")
            || !matches!((tag.chunk(6), tag.chunk(7), tag.chunk(8)), (Some(Token::Long(1)), Some(Token::Long(1)), Some(Token::Long(3))))
        {
            return Err(invalid("unsupported snapshot curve tag"));
        }
        let Some(Token::Str(value)) = tag.chunk(9) else {
            return Err(invalid("missing snapshot curve identity"));
        };
        let fields = value
            .split_ascii_whitespace()
            .take(7)
            .map(str::parse::<u64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| invalid("invalid snapshot curve identity"))?;
        let [primary, secondary, reversed, 0, piece, count] = fields.as_slice() else {
            return Err(invalid("unsupported snapshot piece schema"));
        };
        if *reversed > 1 || *piece == 0 || piece > count || *count > 50_000 {
            return Err(invalid("invalid snapshot piece range"));
        }
        let curve = *curves.get(&(geometry, (*primary, *secondary))).ok_or_else(|| invalid("snapshot curve is absent from its source sketch"))?;
        if !pieces_seen.insert((curve, *piece)) {
            return Err(invalid("repeated snapshot curve piece"));
        }
        result.curves.push(curve);
        result.reversed.push(*reversed == 1);
        result.pieces.push((*piece as u32, *count as u32));
        let next = link(coedge, 3)?;
        if link(record(records, next, "coedge")?, 4)? != id {
            return Err(invalid("inconsistent snapshot loop links"));
        }
        if next == first {
            break;
        }
        id = next;
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
