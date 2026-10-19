//! Native F3D archive inspection and reconstruction of verified history schemas.
//! Strict import rejects unsupported history. Explicit recovery preferences retain
//! usable geometry/history and report every known omission.
//! See docs/f3d.md for the evidence, supported layouts and remaining work.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};

use crate::{IoError, Result};

mod archive;
#[cfg(test)]
mod attribute_tests;
mod attributes;
mod blend_sides;
mod body_labels;
mod body_projections;
mod chamfer_circle_projections;
mod chamfers;
mod circles;
mod circular_patterns;
mod combines;
mod composite_blends;
mod composite_projections;
mod constraints;
mod construction_points;
mod cut_face_projections;
mod entity;
mod extrusion_targets;
mod face_cap_projections;
mod fillets;
#[cfg(test)]
mod geometric_tests;
mod history;
mod import;
#[cfg(test)]
mod import_tests;
#[cfg(test)]
mod legacy_geometry_tests;
mod lines;
mod loft_faces;
mod lofts;
mod mirrors;
#[cfg(not(target_arch = "wasm32"))]
mod native_bodies;
mod offset_import;
mod operations;
mod parameters;
#[cfg(test)]
mod plane_tests;
mod planes;
mod points;
#[cfg(test)]
mod polygon_tests;
#[cfg(not(target_arch = "wasm32"))]
mod profile_cache;
mod profile_groups;
mod profile_selection;
mod profiles;
#[cfg(test)]
mod projection_tests;
mod projections;
#[cfg(test)]
mod radial_tests;
mod recipe_incidence;
mod recovery;
mod retired_circle_projections;
#[cfg(test)]
mod round_profile_tests;
mod side_roles;
#[cfg(test)]
mod symmetric_extent_tests;
pub use chamfers::ChamferSet;
pub use circles::{SketchArc, SketchCircle};
pub use circular_patterns::CircularPattern;
pub use constraints::{CurveRelation, LinearOrientation, SketchConstraint, SketchConstraintGraph, SketchConstraints};
pub use entity::EntityMetadata;
pub use fillets::FilletSet;
pub use history::{HistoryFeature, HistoryKind, SketchPlacement, TimelineEntry};
pub use import::import_f3d;
pub use lines::SketchLine;
pub use parameters::Parameter;
pub use planes::OffsetPlaneDefinition;
pub use points::SketchPoint;
pub use profiles::{CircleProfile, ProfileLoop, SketchProfile};
pub use recovery::{ImportOutcome, ImportPreference, import_f3d_with_preference};

pub const MAX_F3D_BYTES: usize = 256 << 20;
const MAX_STREAM: usize = 128 << 20;
const MAX_EXPANDED: u64 = 512 << 20;
const MAX_ITEMS: usize = 1_000_000;

#[derive(Debug)]
pub struct Class {
    pub id: String,
    pub parent: String,
    pub version: u32,
    pub group: String,
    pub objects: Vec<u64>,
}

#[derive(Debug)]
pub struct Record {
    pub id: u64,
    pub offset: usize,
    pub secondary_offset: Option<usize>,
}

#[derive(Debug)]
pub struct Design {
    recovery: Option<std::cell::RefCell<Vec<String>>>,
    /// Shared compressed source for lazily resolving native selection snapshots.
    archive: Option<std::sync::Arc<[u8]>>,
    pub path: String,
    pub build: u32,
    pub layout: u32,
    pub classes: Vec<Class>,
    pub roots: Vec<u64>,
    pub records: Vec<Record>,
    /// Preserve both original streams, including unknown fields and trailers.
    pub metadata: Vec<u8>,
    pub bulk: Vec<u8>,
}

/// Both index tables delimit records: a secondary record may intervene between
/// successive primary records. Build once per decoding pass, not once per object.
struct Records<'a> {
    design: &'a Design,
    by_id: BTreeMap<u64, &'a Record>,
    offsets: BTreeSet<usize>,
}

impl<'a> Records<'a> {
    fn new(design: &'a Design) -> Self {
        Self {
            design,
            by_id: design.records.iter().map(|r| (r.id, r)).collect(),
            offsets: design
                .records
                .iter()
                .flat_map(|r| std::iter::once(r.offset).chain(r.secondary_offset))
                .chain(std::iter::once(design.bulk.len()))
                .collect(),
        }
    }

    fn primary(&self, id: u64) -> Result<&'a [u8]> {
        let rec = self.by_id.get(&id).ok_or_else(|| invalid("missing object record"))?;
        self.at(rec.offset)
    }

    fn secondary(&self, id: u64) -> Result<&'a [u8]> {
        let offset = self.by_id.get(&id).and_then(|r| r.secondary_offset).ok_or_else(|| invalid("missing secondary object record"))?;
        self.at(offset)
    }

    fn at(&self, offset: usize) -> Result<&'a [u8]> {
        let end = self
            .offsets
            .range((std::ops::Bound::Excluded(offset), std::ops::Bound::Unbounded))
            .next()
            .copied()
            .ok_or_else(|| invalid("missing record boundary"))?;
        self.design.bulk.get(offset..end).ok_or_else(|| invalid("invalid object range"))
    }
}

pub fn is_f3d_path(path: &str) -> bool {
    std::path::Path::new(path).extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("f3d"))
}

fn invalid(message: &str) -> IoError {
    IoError::Invalid(format!("F3D: {message}"))
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(size).ok_or_else(|| invalid("stream offset overflow"))?;
        let bytes = self.bytes.get(self.at..end).ok_or_else(|| invalid("truncated stream"))?;
        self.at = end;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?.try_into().map_err(|_| invalid("short integer"))?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn u64(&mut self) -> Result<u64> {
        let bytes = self.take(8)?.try_into().map_err(|_| invalid("short integer"))?;
        Ok(u64::from_le_bytes(bytes))
    }
    fn reference(&mut self) -> Result<u64> {
        let at = self.at;
        if self.take(1)? != [1] {
            return Err(invalid(&format!("missing object reference at record byte {at}")));
        }
        let id = self.u64()?;
        if self.take(2)? != [0, 0] {
            return Err(invalid(&format!("unsupported object reference at record byte {at}")));
        }
        Ok(id)
    }
    fn count(&mut self, width: usize) -> Result<usize> {
        let count = self.u32()? as usize;
        if count > MAX_ITEMS || count > self.bytes.len().saturating_sub(self.at) / width.max(1) {
            if std::env::var("F3D_DBG").is_ok() {
                eprintln!("DBGC at {} count {count} tag {:?}\n{}", self.at, self.bytes.get(..20), std::backtrace::Backtrace::force_capture());
            }
            return Err(invalid("invalid or excessive item count"));
        }
        Ok(count)
    }
    fn string(&mut self) -> Result<String> {
        let count = self.count(1)?;
        if count > 4096 {
            return Err(invalid("excessive string length"));
        }
        String::from_utf8(self.take(count)?.to_vec()).map_err(|_| invalid("invalid index string"))
    }
    fn wide_string(&mut self) -> Result<String> {
        let count = self.count(2)?;
        if count > 4096 {
            return Err(invalid("excessive wide string length"));
        }
        let chars: Vec<_> = self.take(count * 2)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16(&chars).map_err(|_| invalid("invalid wide string"))
    }
}

fn index(path: String, metadata: Vec<u8>, bulk: Vec<u8>) -> Result<Design> {
    let mut r = Reader { bytes: &metadata, at: 0 };
    let segment = r.string()?;
    // The u32 after the segment name is 0 in most observed streams and 1 in some
    // layout-5 streams; it is kept verbatim in metadata and not interpreted.
    if !matches!(segment.as_str(), "Design" | "FusionDesignSegmentType") || r.u32()? > 1 {
        return Err(invalid("unsupported design stream header"));
    }
    r.wide_string()?;
    let build = r.u32()?;
    let layout = r.u32()?;
    if layout == 8 {
        r.u32()?; // stream revision; retained in metadata
        r.u32()?; // stream flags; retained in metadata
    }
    let descriptor = r.string()?;
    let flavor = r.string()?;
    if !matches!(
        (layout, segment.as_str(), descriptor.as_str(), flavor.as_str()),
        (4 | 5, "Design", "Design", "") | (8, "Design" | "FusionDesignSegmentType", "FusionDesignSegmentType", "Fusion")
    ) || r.u32()? != 1
        || r.u32()? != 0
    {
        return Err(invalid("unsupported metadata layout (observed layouts: 4, 5 and 8)"));
    }
    let count = r.count(20)?;
    let mut classes = Vec::new();
    let mut identities = BTreeSet::new();
    let mut class_objects = BTreeSet::new();
    for _ in 0..count {
        let id = r.string()?;
        if id.is_empty() || !identities.insert(id.clone()) {
            return Err(invalid("empty or duplicate class identity"));
        }
        let parent = r.string()?;
        let version = r.u32()?;
        let group = r.string()?;
        let n = r.count(8)?;
        let mut objects = Vec::new();
        for _ in 0..n {
            let id = r.u64()?;
            if class_objects.len() >= MAX_ITEMS {
                return Err(invalid("too many indexed objects"));
            }
            if !class_objects.insert(id) {
                return Err(invalid("object belongs to multiple classes"));
            }
            objects.push(id);
        }
        classes.push(Class { id, parent, version, group, objects });
    }
    let count = r.count(8)?;
    let mut roots = Vec::new();
    for _ in 0..count {
        let id = r.u64()?;
        if !class_objects.contains(&id) {
            return Err(invalid("unknown root object"));
        }
        roots.push(id);
    }
    let count = r.count(16)?;
    if count != class_objects.len() {
        return Err(invalid("class and object index counts disagree"));
    }
    let mut records = Vec::new();
    let mut by_id = BTreeMap::new();
    let mut offsets = BTreeSet::new();
    let validate = |id: u64, offset: u64| -> Result<usize> {
        let offset = usize::try_from(offset).map_err(|_| invalid("object offset overflow"))?;
        let bytes = bulk.get(offset..).ok_or_else(|| invalid("object offset outside bulk stream"))?;
        let mut record = Reader { bytes, at: 0 };
        // A per-file numeric tag precedes the object identity. It is not a stable
        // feature type and must never be used as a cross-file class identifier.
        let tag = record.string()?;
        if tag.is_empty() || !tag.bytes().all(|c| c.is_ascii_digit()) || record.u64()? != id {
            return Err(invalid("bulk record identity does not match its index"));
        }
        Ok(offset)
    };
    for _ in 0..count {
        let id = r.u64()?;
        let offset = validate(id, r.u64()?)?;
        if !class_objects.contains(&id) || by_id.insert(id, records.len()).is_some() || !offsets.insert(offset) {
            return Err(invalid("unknown or duplicate object index entry"));
        }
        records.push(Record { id, offset, secondary_offset: None });
    }
    let count = r.count(16)?;
    for _ in 0..count {
        let id = r.u64()?;
        let offset = validate(id, r.u64()?)?;
        let record = by_id.get(&id).and_then(|i| records.get_mut(*i)).ok_or_else(|| invalid("unknown secondary object"))?;
        if record.secondary_offset.replace(offset).is_some() || !offsets.insert(offset) {
            return Err(invalid("duplicate secondary object index entry"));
        }
    }
    // These tails are not yet interpreted. Keep them verbatim; reject other
    // layouts instead of silently treating unknown metadata as imported history.
    if layout == 8 {
        r.u64()?; // next identity
        if r.u32()? != 0 {
            return Err(invalid("unsupported metadata trailer"));
        }
        for _ in 0..r.count(8)? {
            r.string()?;
            r.u32()?;
        }
    } else {
        r.take(if layout == 4 { 8 } else { 12 })?;
    }
    if r.at != metadata.len() {
        return Err(invalid("unsupported metadata trailer"));
    }
    Ok(Design { recovery: None, archive: None, path, build, layout, classes, roots, records, metadata, bulk })
}

/// Inspect native object indexes without extracting files or evaluating geometry.
/// Does not import editable features. Selected records can be inspected through
/// Design's decoding methods; unknown payload semantics remain opaque.
pub fn read_f3d(bytes: &[u8]) -> Result<Vec<Design>> {
    if bytes.len() > MAX_F3D_BYTES {
        return Err(invalid("archive exceeds 256 MiB"));
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| invalid(&format!("invalid ZIP archive: {e}")))?;
    if zip.len() > 10_000 {
        return Err(invalid("too many archive entries"));
    }
    let mut names = BTreeSet::new();
    let mut expanded = 0u64;
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(|e| invalid(&format!("invalid archive entry: {e}")))?;
        let name = entry.name();
        if name.starts_with('/') || name.contains(['\\', ':']) || name.split('/').any(|p| p == "..") || !names.insert(name.to_string()) {
            return Err(invalid("unsafe or duplicate archive path"));
        }
        expanded = expanded.checked_add(entry.size()).ok_or_else(|| invalid("expanded size overflow"))?;
        if expanded > MAX_EXPANDED {
            return Err(invalid("expanded archive exceeds 512 MiB"));
        }
    }
    if !names.contains("Manifest.dat") {
        return Err(invalid("missing archive manifest"));
    }
    let mut designs = Vec::new();
    for name in names.iter().filter(|n| n.ends_with("/Design1/MetaStream.dat") || n.ends_with("/FusionDesignSegmentType1/MetaStream.dat")) {
        let path = name.strip_suffix("MetaStream.dat").ok_or_else(|| invalid("invalid stream path"))?;
        let metadata = archive::read(&mut zip, name)?;
        let bulk = archive::read(&mut zip, &format!("{path}BulkStream.dat"))?;
        designs.push(index(path.trim_end_matches('/').to_string(), metadata, bulk)?);
    }
    if designs.is_empty() {
        return Err(invalid("no native design streams"));
    }
    let archive: std::sync::Arc<[u8]> = bytes.into();
    for design in &mut designs {
        design.archive = Some(archive.clone());
    }
    Ok(designs)
}

#[cfg(test)]
mod angular_dimension_tests;
#[cfg(test)]
mod midpoint_tests;
#[cfg(test)]
mod offset_dimension_tests;
#[cfg(test)]
mod offset_history_tests;
#[cfg(test)]
mod relationship_tests;
#[cfg(test)]
mod symmetry_tests;
#[cfg(test)]
mod tests;
