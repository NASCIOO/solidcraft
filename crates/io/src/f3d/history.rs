//! Schemas verified with local reorder, rename, suppression and operation controls.
//! These are inspected relationships, not a complete feature reconstruction.
use super::*;
use solidcraft_doc::Operation;

const TIMELINE: &str = "2F4C1849-1A5A-4F6C-A086-8DD445CBF94B";
pub(super) const SKETCH: &str = "8DA771B7-52ED-42FC-94E2-2353FE141373";
const EXTRUDE: &str = "DD405BC2-D673-44F0-8833-5CB2A1C186C7";
const GEOMETRY: &str = "44A64366-4BD3-4B24-881A-F94C206E8F2D";
const SKETCH_CONTEXT: &str = "5275CBA4-3D7D-40DC-A651-1DFF3FC8AFBF";
const TRANSFORM: &str = "F47A46FB-DA27-4AFA-8A72-1B38FA596E23";

#[derive(Debug, PartialEq)]
pub struct HistoryFeature {
    pub id: u64,
    pub name: String,
    pub suppressed: bool,
    pub dependencies: Vec<u64>,
    pub kind: HistoryKind,
}

#[derive(Debug, PartialEq)]
pub enum HistoryKind {
    CircularPattern(CircularPattern),
    TwoEdgePoint {
        definition: u64,
        edges: [u64; 2],
        position: solidcraft_geom::Vec3,
    },
    Chamfer {
        sets: Vec<ChamferSet>,
    },
    Fillet {
        sets: Vec<FilletSet>,
    },
    Sketch {
        geometry: u64,
        placement: SketchPlacement,
    },
    OffsetPlane {
        geometry: u64,
        definition: u64,
        extra_offset: Option<u64>,
    },
    Extrude {
        operation: Operation,
        profile_containers: Vec<u64>,
        target_containers: Vec<u64>,
    },
    Loft {
        operation: Operation,
        section_containers: Vec<u64>,
        target_containers: Vec<u64>,
    },
    Mirror {
        plane: u64,
        source_container: u64,
        output_count: usize,
    },
    MirrorFeatures {
        plane: u64,
        sources: Vec<u64>,
        targets: Vec<u64>,
        output_count: usize,
    },
    Combine {
        operation: Operation,
        keep_tools: bool,
        operation_tag: u64,
        tool_containers: Vec<u64>,
        target_container: u64,
        consumed_handles: Vec<u64>,
    },
}

#[derive(Debug, PartialEq)]
pub struct SketchPlacement {
    pub support: u64,
    /// Row-major local-to-world matrix; translations are native centimetres.
    pub matrix: [f64; 16],
    /// The placement's owning object. Assembly placement is not yet reconstructed.
    pub owner: u64,
    pub entities: Vec<u64>,
    pub projections: Vec<u64>,
    /// Version-17 secondary entity metadata, retained for investigation rather
    /// than discarded. Reconstruction rejects it until its semantics are known.
    pub legacy_entity_metadata: Vec<(u64, u32)>,
}

#[derive(Debug)]
pub struct TimelineEntry {
    pub id: u64,
    pub class: String,
    pub version: u32,
    /// A failure remains attached to its actual timeline position. Inspection
    /// can expose later supported records without treating the history as complete.
    pub feature: Result<HistoryFeature>,
}

impl Design {
    /// The actual timeline, including suppressed features. Neither registration
    /// order nor the evaluation dependency graph is a substitute for this list.
    pub fn history(&self) -> Result<Vec<HistoryFeature>> {
        self.recover_records("timeline feature", self.timeline()?.into_iter().map(|entry| entry.feature))
    }

    pub fn timeline(&self) -> Result<Vec<TimelineEntry>> {
        if !matches!(self.layout, 4 | 5 | 8) {
            return Err(invalid("unsupported history metadata layout"));
        }
        let class = self.classes.iter().find(|c| c.id == TIMELINE).ok_or_else(|| invalid("missing timeline"))?;
        if !matches!(class.version, 2 | 3) || class.objects.len() != 1 {
            return Err(invalid("unsupported timeline class version or count"));
        }
        let records = Records::new(self);
        let classes: BTreeMap<_, _> = self.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let timeline_id = class.objects.first().copied().ok_or_else(|| invalid("missing timeline identity"))?;
        let mut r = payload(records.primary(timeline_id)?, timeline_id)?;
        empty(&mut r)?;
        zeros(&mut r, 2)?;
        let owner = r.reference()?;
        require_class(&classes, owner, "C3423E04-7E85-4B44-8E31-A5FA9C100052", 2)?;
        let ids = references(&mut r)?;
        finish(&r)?;
        if ids.len() > solidcraft_doc::MAX_FEATURES {
            return Err(invalid("too many native timeline features"));
        }
        let inherited_version = self
            .classes
            .iter()
            .find(|c| c.id == "A0A15D26-1F3B-4120-A3F1-9CDDA189AB74")
            .map(|c| c.version)
            .unwrap_or(if self.layout == 8 { 2 } else { 0 });
        let body_feature_version = self
            .classes
            .iter()
            .find(|c| c.id == "2EDC2536-D95B-4469-8ACA-DC07124D9C43")
            .map(|c| c.version)
            .unwrap_or(if self.layout == 8 { 2 } else { 0 });
        let mut seen = BTreeSet::new();
        ids.into_iter()
            .map(|id| {
                if !seen.insert(id) {
                    return Err(invalid("duplicate timeline feature"));
                }
                let class = classes.get(&id).ok_or_else(|| invalid("missing timeline feature"))?;
                let feature = (|| {
                    let mut r = payload(records.primary(id)?, id)?;
                    let (kind, custom) = match (class.id.as_str(), class.version) {
                        (SKETCH, version @ 3..=5) => {
                            empty(&mut r)?;
                            zeros(&mut r, 1)?;
                            if !super::attributes::read(&mut r)?.is_empty() {
                                return Err(invalid("unsupported sketch feature attributes"));
                            }
                            zeros(&mut r, 1)?;
                            let bindings = references(&mut r)?;
                            validate_refs(&classes, &bindings)?;
                            r.u32()?; // evaluation state, not timeline position
                            zeros(&mut r, 1)?;
                            let geometry = r.reference()?;
                            // A sketch feature keeps its version when its geometry is upgraded:
                            // v4 features are observed over both v17 and v18 geometry.
                            let geometry_version = classes.get(&geometry).map_or(0, |c| c.version);
                            let geometry_version = if geometry_version > version + 13 { geometry_version.min(18) } else { version + 13 };
                            require_class(&classes, geometry, GEOMETRY, geometry_version)?;
                            let plane = r.reference()?;
                            validate_refs(&classes, &[plane])?;
                            if version >= 5 {
                                zeros(&mut r, 4)?;
                            }
                            let projections = references(&mut r)?;
                            for projection in &projections {
                                // Projection generations can survive a sketch upgrade.
                                // Their own versioned reader validates the relationship.
                                super::projections::version(&classes, *projection)?;
                            }
                            if version >= 4 {
                                zeros(&mut r, 4)?;
                            }
                            let (mut placement, name) = placement(&records, &classes, geometry, geometry_version)?;
                            placement.support = plane;
                            placement.projections = projections;
                            (HistoryKind::Sketch { geometry, placement }, name)
                        }
                        ("D869265F-F339-4751-A066-91D30F81FF08", version @ (2 | 4)) => {
                            empty(&mut r)?;
                            zeros(&mut r, 1)?;
                            if !super::attributes::read(&mut r)?.is_empty() {
                                return Err(invalid("unsupported plane feature attributes"));
                            }
                            if r.u32()? != 1 {
                                return Err(invalid("unsupported plane binding flags"));
                            }
                            if version >= 4 {
                                zeros(&mut r, 1)?;
                            }
                            let extra_offset = match r.bytes.get(r.at) {
                                Some(0) => {
                                    r.take(1)?;
                                    None
                                }
                                Some(1) => {
                                    let binding = r.reference()?;
                                    require_class(&classes, binding, "D91D429C-1BB5-4640-B49F-69AF14F463E9", 0)?;
                                    Some(binding)
                                }
                                _ => return Err(invalid("unsupported plane extra offset")),
                            };
                            if r.take(1)? != [1] {
                                return Err(invalid("unsupported plane flags"));
                            }
                            let geometry = r.reference()?;
                            require_class(&classes, geometry, super::planes::GEOMETRY, 1)?;
                            let definition = r.reference()?;
                            require_class(&classes, definition, super::planes::DEFINITION, if version == 2 { 8 } else { 15 })?;
                            if r.take(1)? != [u8::from(extra_offset.is_some())] {
                                return Err(invalid("unsupported plane definition flags"));
                            }
                            (HistoryKind::OffsetPlane { geometry, definition, extra_offset }, String::new())
                        }
                        (super::construction_points::CLASS, 1) => super::construction_points::inputs(&mut r, &records, &classes, id)?,
                        (super::chamfers::CLASS, version @ (2 | 3)) => {
                            (HistoryKind::Chamfer { sets: super::chamfers::inputs(&mut r, &records, &classes, id, version)? }, String::new())
                        }
                        (super::fillets::CLASS, version @ (2 | 3)) => {
                            (HistoryKind::Fillet { sets: super::fillets::inputs(&mut r, &records, &classes, version)? }, String::new())
                        }
                        (super::lofts::CLASS, 9) => {
                            let (operation, section_containers) = super::lofts::inputs(&mut r, &records, &classes, id)?;
                            let roles = body_roles(&mut r, &classes, body_feature_version)?;
                            let target_containers = roles.last().cloned().unwrap_or_default();
                            (HistoryKind::Loft { operation, section_containers, target_containers }, String::new())
                        }
                        (super::mirrors::CLASS, 0) => {
                            let kind = super::mirrors::inputs(&mut r, &records, &classes, id, body_feature_version)?;
                            (kind, String::new())
                        }
                        (super::circular_patterns::CLASS, 0) => {
                            let pattern = super::circular_patterns::inputs(&mut r, &records, &classes, id, body_feature_version)?;
                            (HistoryKind::CircularPattern(pattern), String::new())
                        }
                        (super::combines::CLASS, 1) => {
                            let kind = super::combines::inputs(&mut r, &records, &classes, id, body_feature_version)?;
                            (kind, String::new())
                        }
                        (EXTRUDE, 8) => {
                            let input = extrude_inputs(&mut r, &classes)?;
                            let operation = input.operation;
                            let profile_containers = references(&mut r)?;
                            for p in &profile_containers {
                                require_class(&classes, *p, "2CA5A1CD-C99B-4C9B-91AF-57989148E841", 1)?;
                            }
                            let roles = body_roles(&mut r, &classes, body_feature_version)?;
                            let target_containers = roles.last().cloned().unwrap_or_default();
                            for target in &target_containers {
                                require_class(&classes, *target, "2CA5A1CD-C99B-4C9B-91AF-57989148E841", 1)?;
                            }
                            if operation == Operation::NewBody && !target_containers.is_empty() {
                                return Err(invalid("new-body extrusion has boolean targets"));
                            }
                            (HistoryKind::Extrude { operation, profile_containers, target_containers }, String::new())
                        }
                        _ => return Err(invalid(&format!("unsupported timeline feature {id}: class {} version {}", class.id, class.version))),
                    };
                    r.wide_string()?; // inherited external identity
                    r.take(3)?;
                    let dependencies = references(&mut r)?;
                    validate_refs(&classes, &dependencies)?;
                    r.u32()?; // evaluation cache status
                    let base = r.wide_string()?;
                    let sequence = r.u32()?;
                    r.u32()?;
                    let feature_name = r.wide_string()?;
                    let (length, suppression) = match inherited_version {
                        1 => (65, 50),
                        2 => (66, 51),
                        _ => return Err(invalid("unsupported inherited feature schema")),
                    };
                    let tail = r.take(length)?;
                    let suppressed = match (tail.get(5), tail.get(suppression)) {
                        (Some(0), Some(0)) => false,
                        (Some(1), Some(1)) => true,
                        _ => return Err(invalid("inconsistent feature suppression flags")),
                    };
                    finish(&r)?;
                    if let HistoryKind::Chamfer { sets } = &kind
                        && sets.iter().any(|s| s.suppressed != suppressed)
                    {
                        return Err(invalid("inconsistent chamfer suppression"));
                    }
                    if let HistoryKind::TwoEdgePoint { definition, edges: [a, b], .. } = &kind
                        && dependencies != [*definition, *a, *b]
                    {
                        return Err(invalid("construction-point dependencies disagree with its sources"));
                    }
                    let name = if !custom.is_empty() {
                        custom
                    } else if !feature_name.is_empty() {
                        feature_name
                    } else {
                        format!(
                            "{}{sequence}",
                            match base.as_str() {
                                "WorkPlane" => "Plane",
                                "WorkPoint" => "Point",
                                _ => &base,
                            }
                        )
                    };
                    Ok(HistoryFeature { id, name, suppressed, dependencies, kind })
                })()
                .map_err(|error| {
                    let message = error.to_string();
                    invalid(&format!(
                        "timeline feature {id} ({} v{}): {}",
                        class.id,
                        class.version,
                        message.strip_prefix("F3D: ").unwrap_or(&message)
                    ))
                });
                Ok(TimelineEntry { id, class: class.id.clone(), version: class.version, feature })
            })
            .collect()
    }
}

pub(super) enum ExtrudeLimit {
    Distance(u64),
    SymmetricDistance { distance: u64, full_length: bool },
    ToEntity { offset: u64, container: u64 },
}

pub(super) struct ExtrudeInputs {
    pub operation: Operation,
    pub axis: solidcraft_geom::Vec3,
    pub limit: ExtrudeLimit,
    pub taper: u64,
    pub start_offset: Option<u64>,
}

/// Consume verified distance/To Entity schemas, stopping
/// at the profile container lists. Shared by inspection and reconstruction.
pub(super) fn extrude_inputs(r: &mut Reader<'_>, classes: &BTreeMap<u64, &Class>) -> Result<ExtrudeInputs> {
    empty(r)?;
    zeros(r, 1)?;
    if !super::attributes::read(r)?.is_empty() {
        return Err(invalid("unsupported extrusion attributes"));
    }
    r.take(3)?; // evaluation metadata
    let operation = operation(r.u32()?)?;
    let direction = r.u32()?;
    if !matches!(direction, 1 | 3) {
        return Err(invalid("unsupported extrusion extent layout"));
    }
    let extent_type = r.u32()?;
    if !matches!(extent_type, 1 | 2) {
        return Err(invalid("unsupported extrusion extent type"));
    }
    // ToEntity's direction hint disambiguates targets with several intersections.
    // The importer currently accepts only parallel planes, with one signed endpoint;
    // explicit hints in either direction produce the same observed geometry.
    match r.take(1)? {
        [0] => {}
        [1] if direction == 1 && extent_type == 1 => {}
        _ => return Err(invalid("unsupported extrusion direction hint")),
    }
    if r.take(1)? != [1] {
        return Err(invalid("unsupported extrusion start type"));
    }
    let offset = match r.take(1)? {
        [0] => false,
        [1] => true,
        _ => return Err(invalid("unsupported extrusion start-offset flag")),
    };
    if direction == 3 && offset {
        return Err(invalid("unsupported symmetric extrusion start offset"));
    }
    zeros(r, 3)?;
    let axis = solidcraft_geom::Vec3::new(f64::from_bits(r.u64()?), f64::from_bits(r.u64()?), f64::from_bits(r.u64()?));
    if !axis.is_finite() {
        return Err(invalid("non-finite extrusion axis"));
    }
    let start_offset = if offset {
        let binding = r.reference()?;
        require_class(classes, binding, "D91D429C-1BB5-4640-B49F-69AF14F463E9", 0)?;
        zeros(r, 1)?;
        Some(binding)
    } else {
        zeros(r, 1)?;
        None
    };
    // Modern symmetric records leave the opposite-distance slot empty. Older
    // records (the clamp) repeat the same binding; they must not supply a second,
    // independently editable distance under a symmetric direction.
    let opposite = if start_offset.is_none() {
        if direction == 3 && r.bytes.get(r.at) == Some(&1) {
            Some(r.reference()?)
        } else {
            zeros(r, 1)?;
            None
        }
    } else {
        None
    };
    if direction == 1 && extent_type == 1 {
        zeros(r, 1)?;
        require_class(classes, r.reference()?, "0897AF07-EF1B-42E5-BDD9-1C746A085CBC", 0)?;
        zeros(r, 1)?;
        let taper = r.reference()?;
        require_class(classes, taper, "D91D429C-1BB5-4640-B49F-69AF14F463E9", 0)?;
        let container = r.reference()?;
        require_class(classes, container, "2CA5A1CD-C99B-4C9B-91AF-57989148E841", 1)?;
        if r.u32()? != 2 {
            return Err(invalid("unsupported To Object extrusion flags"));
        }
        let offset = r.reference()?;
        require_class(classes, offset, "D91D429C-1BB5-4640-B49F-69AF14F463E9", 0)?;
        // Older planar-face extents retain 0; explicit minimum (1) and maximum (2)
        // are relevant only to body targets in the public API. Reconstruction accepts
        // only one parallel plane, so these observed modes share one exact endpoint.
        if !matches!(r.u32()?, 0..=2) {
            return Err(invalid("unsupported To Object extrusion end flags"));
        }
        zeros(r, 31)?;
        return Ok(ExtrudeInputs { operation, axis, limit: ExtrudeLimit::ToEntity { offset, container }, taper, start_offset });
    }
    let distance = r.reference()?;
    require_class(classes, distance, "D91D429C-1BB5-4640-B49F-69AF14F463E9", 0)?;
    require_class(classes, r.reference()?, "0897AF07-EF1B-42E5-BDD9-1C746A085CBC", 0)?;
    zeros(r, 1)?;
    let taper = r.reference()?;
    require_class(classes, taper, "D91D429C-1BB5-4640-B49F-69AF14F463E9", 0)?;
    zeros(r, 1)?;
    if r.u32()? != 1 {
        return Err(invalid("unsupported extrusion extent flags"));
    }
    if direction == 3 {
        if opposite.is_some_and(|id| id != distance) {
            return Err(invalid("symmetric extrusion distance bindings disagree"));
        }
        // Controlled full-/half-length exports distinguish this marker from
        // the ordinary two-sided distance extent. The older face-extend form
        // moves the marker by four bytes without changing the trailing inputs.
        zeros(r, if extent_type == 1 { 5 } else { 9 })?;
        if r.u32()? != 1 {
            return Err(invalid("unsupported symmetric extrusion extent flags"));
        }
        zeros(r, if extent_type == 1 { 10 } else { 6 })?;
        if r.reference()? != taper {
            return Err(invalid("symmetric extrusion taper bindings disagree"));
        }
        zeros(r, 1)?;
        let full_length = match r.u32()? {
            1 => true,
            2 => false,
            _ => return Err(invalid("unsupported symmetric extrusion length convention")),
        };
        zeros(r, 11)?;
        return Ok(ExtrudeInputs { operation, axis, limit: ExtrudeLimit::SymmetricDistance { distance, full_length }, taper, start_offset });
    }
    zeros(r, 36)?;
    Ok(ExtrudeInputs { operation, axis, limit: ExtrudeLimit::Distance(distance), taper, start_offset })
}

pub(super) fn operation(value: u32) -> Result<Operation> {
    match value {
        1 => Ok(Operation::Join),
        2 => Ok(Operation::Cut),
        3 => Ok(Operation::Intersect),
        4 => Ok(Operation::NewBody),
        _ => Err(invalid("unsupported extrusion operation")),
    }
}

pub(super) fn body_roles(r: &mut Reader<'_>, classes: &BTreeMap<u64, &Class>, version: u32) -> Result<Vec<Vec<u64>>> {
    let count = match version {
        1 => 3,
        2 => 4,
        _ => return Err(invalid("unsupported inherited body feature schema")),
    };
    let mut roles = Vec::new();
    for _ in 0..count {
        let ids = references(r)?;
        validate_refs(classes, &ids)?;
        roles.push(ids);
    }
    // An unchanged legacy archive gains an empty leading role when saved with
    // schema 2. Keep canonical output/target positions for both encodings.
    if version == 1 {
        roles.insert(0, Vec::new());
    }
    Ok(roles)
}

fn placement(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, geometry: u64, version: u32) -> Result<(SketchPlacement, String)> {
    let mut r = payload(records.secondary(geometry)?, geometry)?;
    empty(&mut r)?;
    let transform = r.reference()?;
    require_class(classes, transform, TRANSFORM, 0)?;
    r.take(6)?;
    // Version 18 and later version-17 streams add a reference to the design's
    // 5275CBA4 object here; earlier version-17 streams continue with the name.
    let mut probe = Reader { bytes: r.bytes, at: r.at };
    let shared = probe.reference().ok().and_then(|id| classes.get(&id)).is_some_and(|c| c.id == SKETCH_CONTEXT && c.version == 4);
    if version == 18 || shared {
        r.take(11)?;
    }
    let name = r.wide_string()?;
    zeros(&mut r, 1)?;
    let entities = references(&mut r)?;
    validate_refs(classes, &entities)?;
    let mut legacy_entity_metadata = Vec::new();
    // v17 and v18 appear both with an extension table (possibly empty) and with
    // only the final marked parent reference (the knob corpus). The record boundary
    // makes the latter unambiguous; never interpret that reference as a u32 table count.
    if matches!(version, 17 | 18) && r.bytes.len().saturating_sub(r.at) != 11 {
        for _ in 0..r.count(15)? {
            let id = r.reference()?;
            validate_refs(classes, &[id])?;
            let flags = r.u32()?;
            if flags != 1 {
                return Err(invalid("unsupported legacy entity metadata"));
            }
            legacy_entity_metadata.push((id, flags));
        }
    }
    let parent = r.reference()?;
    validate_refs(classes, &[parent])?;
    finish(&r)?;
    let mut r = payload(records.primary(transform)?, transform)?;
    empty(&mut r)?;
    zeros(&mut r, 2)?;
    let mut matrix = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
    match r.take(1)? {
        [1] => {}
        [0] => {
            for x in &mut matrix {
                *x = f64::from_bits(r.u64()?);
                if !x.is_finite() {
                    return Err(invalid("non-finite sketch transform"));
                }
            }
        }
        _ => return Err(invalid("unsupported sketch transform encoding")),
    }
    zeros(&mut r, 1)?;
    let owner = r.reference()?;
    validate_refs(classes, &[owner])?;
    finish(&r)?;
    Ok((SketchPlacement { support: 0, matrix, owner, entities, projections: vec![], legacy_entity_metadata }, name))
}

pub(super) fn payload(bytes: &[u8], id: u64) -> Result<Reader<'_>> {
    let mut r = Reader { bytes, at: 0 };
    r.string()?;
    if r.u64()? != id {
        return Err(invalid("history record identity mismatch"));
    }
    Ok(r)
}
pub(super) fn empty(r: &mut Reader<'_>) -> Result<()> {
    if !r.string()?.is_empty() {
        return Err(invalid("unsupported history attributes"));
    }
    Ok(())
}
pub(super) fn zeros(r: &mut Reader<'_>, n: usize) -> Result<()> {
    let at = r.at;
    if r.take(n)?.iter().any(|b| *b != 0) {
        return Err(invalid(&format!("unsupported history flags at record byte {at} ({n} bytes)")));
    }
    Ok(())
}
/// The u32 after a selection type is 0 in most files and 1 throughout files whose
/// design stream header carries flag 1. The fields after it are unchanged.
pub(super) fn selection_flag(r: &mut Reader<'_>) -> Result<()> {
    let at = r.at;
    if r.u32()? > 1 {
        return Err(invalid(&format!("unsupported selection flag at record byte {at}")));
    }
    Ok(())
}
pub(super) fn references(r: &mut Reader<'_>) -> Result<Vec<u64>> {
    (0..r.count(11)?).map(|_| r.reference()).collect()
}
pub(super) fn finish(r: &Reader<'_>) -> Result<()> {
    if r.at != r.bytes.len() {
        return Err(invalid("unsupported history payload tail"));
    }
    Ok(())
}
pub(super) fn require_class(classes: &BTreeMap<u64, &Class>, id: u64, guid: &str, version: u32) -> Result<()> {
    let actual = classes.get(&id).ok_or_else(|| invalid(&format!("missing history reference {id}")))?;
    if actual.id != guid || actual.version != version {
        return Err(invalid(&format!("history reference {id} has class {} v{}; expected {guid} v{version}", actual.id, actual.version)));
    }
    Ok(())
}
fn validate_refs(classes: &BTreeMap<u64, &Class>, ids: &[u64]) -> Result<()> {
    if ids.iter().any(|id| !classes.contains_key(id)) {
        return Err(invalid("missing history reference"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod endpoint_tests {
    use super::*;
    use crate::f3d::import_tests::fixture;
    use crate::f3d::relationship_tests::{reference, u32};

    #[test]
    fn to_entity_extent_keeps_explicit_offset_and_target_inputs() {
        let mut f = fixture(false);
        let old = f.0[&9].2.clone();
        let mut b = old[..57].to_vec();
        b[21..25].copy_from_slice(&1_u32.to_le_bytes());
        b.push(0);
        reference(&mut b, 67);
        b.push(0);
        reference(&mut b, 66);
        reference(&mut b, 102);
        u32(&mut b, 2);
        reference(&mut b, 64);
        u32(&mut b, 1);
        b.extend([0; 31]);
        let end = b.len();
        b.extend_from_slice(&old[132..]);
        f.0.get_mut(&9).unwrap().2 = b;
        let d = crate::f3d::import_tests::Fixture(f.0.clone()).design();
        let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let records = Records::new(&d);
        let mut reader = payload(records.primary(9).unwrap(), 9).unwrap();
        let start = reader.at;
        let input = extrude_inputs(&mut reader, &classes).unwrap();
        assert!(matches!(input.limit, ExtrudeLimit::ToEntity { offset: 64, container: 102 }));
        assert_eq!(input.taper, 66);
        assert_eq!(reader.at - start, end);
        assert!(d.history().is_ok());
        let mut hinted = crate::f3d::import_tests::Fixture(f.0.clone());
        hinted.0.get_mut(&9).unwrap().2[25] = 1;
        assert!(hinted.design().history().is_ok());
        let mut legacy = crate::f3d::import_tests::Fixture(f.0.clone());
        legacy.0.get_mut(&9).unwrap().2[107] = 0;
        assert!(legacy.design().history().is_ok());
        let mut maximum = crate::f3d::import_tests::Fixture(f.0.clone());
        maximum.0.get_mut(&9).unwrap().2[107] = 2;
        assert!(maximum.design().history().is_ok());
        let mut distance = fixture(false);
        distance.0.get_mut(&9).unwrap().2[25] = 1;
        assert!(distance.design().history().is_err());
        for keep in 0..end {
            let mut f = crate::f3d::import_tests::Fixture(f.0.clone());
            f.0.get_mut(&9).unwrap().2.truncate(keep);
            assert!(f.design().history().is_err(), "truncated at {keep}");
        }
        for (at, value) in [(21, 3), (25, 2), (58, 0), (70, 0), (81, 0), (92, 3), (96, 0), (107, 3), (120, 1)] {
            let mut f = crate::f3d::import_tests::Fixture(f.0.clone());
            f.0.get_mut(&9).unwrap().2[at] = value;
            assert!(f.design().history().is_err(), "invalid field {at}");
        }
    }
}
