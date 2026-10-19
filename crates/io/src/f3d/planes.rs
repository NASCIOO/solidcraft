//! Construction-plane dependencies and certified native sketch face attachments.
use super::history::{empty, finish, payload, references, require_class, zeros};
use super::*;
use solidcraft_geom::{Plane, Vec3};

pub(super) const GEOMETRY: &str = "8D424E29-DE23-4B1F-A3AD-84C52A497D1A";
pub(super) const DEFINITION: &str = "7B6C3C2D-4096-4E6D-B225-699E8C2D9355";

#[derive(Debug, PartialEq)]
pub struct OffsetPlaneDefinition {
    pub feature: u64,
    pub geometry: u64,
    pub base: u64,
    pub distance: u64,
    pub extra: Option<u64>,
    /// Cached placement in millimetres, checked separately against dependencies on import.
    pub plane: Plane,
}

impl Design {
    /// Inspect verified offset-plane dependencies even when unrelated timeline
    /// features remain unsupported. Unknown plane methods still fail explicitly.
    pub fn offset_planes(&self) -> Result<Vec<OffsetPlaneDefinition>> {
        let records = Records::new(self);
        let classes = self.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let mut out = Vec::new();
        for entry in self.timeline()? {
            if entry.class != "D869265F-F339-4751-A066-91D30F81FF08" {
                continue;
            }
            let feature = entry.feature?;
            let HistoryKind::OffsetPlane { geometry, definition: id, extra_offset } = feature.kind else {
                return Err(invalid("unsupported plane history"));
            };
            let d = definition(&records, &classes, id, feature.id)?;
            if d.extra != extra_offset {
                return Err(invalid("inconsistent internal plane offset binding"));
            }
            out.push(OffsetPlaneDefinition {
                feature: feature.id,
                geometry,
                base: d.base,
                distance: d.distance.ok_or_else(|| invalid("missing plane distance"))?,
                extra: d.extra,
                plane: d.plane,
            });
        }
        Ok(out)
    }
}

pub(super) struct Definition {
    pub base: u64,
    pub distance: Option<u64>,
    pub extra: Option<u64>,
    pub plane: Plane,
}

pub(super) fn definition(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<Definition> {
    let mut definition = raw_definition(records, classes, id, owner, false)?;
    definition.base = tracked_plane(records, classes, definition.base, owner)?;
    Ok(definition)
}

/// A sketch attached to a body face carries both a plane selector and a body
/// relationship. Require those two explicit identities to agree.
pub(super) fn face_definition(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64, body: u64) -> Result<Definition> {
    let definition = raw_definition(records, classes, id, owner, true)?;
    if definition.extra.is_some() || definition.distance.is_some() {
        return Err(invalid("unsupported face-attached sketch offset"));
    }
    let face = super::loft_faces::face(records, classes, definition.base, owner)?;
    if face.body != body {
        return Err(invalid("sketch support body disagrees with its face selector"));
    }
    Ok(definition)
}

fn raw_definition(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64, attached: bool) -> Result<Definition> {
    let version = classes
        .get(&id)
        .filter(|class| class.id == DEFINITION && matches!(class.version, 8 | 15))
        .map(|class| class.version)
        .ok_or_else(|| invalid("unsupported plane definition class"))?;
    let mut r = payload(records.primary(id)?, id)?;
    empty(&mut r)?;
    let mut early_marker = false;
    let extra = if version == 8 {
        // Original layout-5 offset planes have no extra-offset slot; the
        // current public-API resave adds the version-15 header and null slot.
        // Like version 15, face attachments carry the marker in one of two
        // placements eight bytes apart; the earlier one keeps the full evaluation tail.
        early_marker = plane_header(&mut r, 31, &[10, 18], attached)? == Some(10);
        None
    } else {
        // Current files have two observed face marker placements, including
        // legacy attachments after resaving. Both retain the same following fields;
        // the explicit face/body identities and placement are checked below.
        plane_header(&mut r, 36, &[11, 19], attached)?;
        if r.take(2)? != [1, 0] {
            return Err(invalid("unsupported plane definition header"));
        }
        match r.bytes.get(r.at) {
            Some(0) => {
                r.take(1)?;
                None
            }
            Some(1) => Some(r.reference()?),
            _ => return Err(invalid("unsupported plane extra offset")),
        }
    };
    if version >= 15 {
        zeros(&mut r, 7)?;
    }
    let mut matrix = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
    match r.take(1)? {
        [1] => {}
        [0] => {
            for n in &mut matrix {
                *n = f64::from_bits(r.u64()?);
            }
        }
        _ => return Err(invalid("unsupported plane transform encoding")),
    }
    let plane = super::import::plane(&matrix)?;
    zeros(&mut r, 35)?;
    if r.u32()? != 2 {
        return Err(invalid("unsupported construction-plane method"));
    }
    if version >= 15 {
        zeros(&mut r, 1)?;
    }
    let distance = match references(&mut r)?.as_slice() {
        [] => None,
        [id] => Some(*id),
        _ => return Err(invalid("unsupported plane distance bindings")),
    };
    let bases = references(&mut r)?;
    let [base] = bases.as_slice() else { return Err(invalid("unsupported plane base references")) };
    zeros(&mut r, 2)?;
    let tail = if attached && version == 8 && !early_marker { 59 } else { 60 };
    evaluation_tail(&mut r, classes, owner, tail)?;
    Ok(Definition { base: *base, distance, extra, plane })
}

/// Returns the face marker placement, if the header carries one.
fn plane_header(r: &mut Reader<'_>, length: usize, flags: &[usize], attached: bool) -> Result<Option<usize>> {
    let bytes = r.take(length)?;
    if bytes.iter().all(|b| *b == 0) {
        return Ok(None);
    }
    if attached {
        for flag in flags {
            let mut face = vec![0; length];
            let field = face.get_mut(*flag..*flag + 8).ok_or_else(|| invalid("invalid plane header schema"))?;
            field.copy_from_slice(&1_f64.to_le_bytes());
            if bytes == face {
                return Ok(Some(*flag));
            }
        }
    }
    Err(invalid("unsupported plane definition header"))
}

/// Keep the sketch's in-plane frame relative to the certified source face.
/// Cached coordinates validate placement, but never select or replace a face.
pub(super) fn attach_sketch(
    doc: &solidcraft_doc::Document,
    state: &solidcraft_doc::ModelState,
    mut anchor: solidcraft_doc::FaceAnchor,
    sketch: Plane,
) -> Result<solidcraft_doc::PlaneRef> {
    anchor.coordinates = [0., 0.];
    anchor.x_axis = [1., 0.];
    let canonical = doc.resolve_face_anchor(&anchor, 0, state).map_err(|e| invalid(&e.to_string()))?;
    let delta = sketch.origin - canonical.origin;
    if sketch.normal().dot(canonical.normal()) < 1. - 1e-8 || delta.dot(canonical.normal()).abs() > 1e-7 {
        return Err(invalid("sketch placement disagrees with its source face"));
    }
    anchor.coordinates = [delta.dot(canonical.x), delta.dot(canonical.y)];
    anchor.x_axis = [sketch.x.dot(canonical.x), sketch.x.dot(canonical.y)];
    anchor.validate().map_err(|e| invalid(&e.to_string()))?;
    Ok(solidcraft_doc::PlaneRef::Face { plane: sketch, at: sketch.origin, name: Some(anchor.target.name.clone()), anchor: Some(Box::new(anchor)) })
}

pub(super) fn evaluation_tail(r: &mut Reader<'_>, classes: &BTreeMap<u64, &Class>, owner: u64, length: usize) -> Result<()> {
    for id in references(r)? {
        if !classes.contains_key(&id) {
            return Err(invalid("missing plane evaluation reference"));
        }
    }
    r.take(length)?; // inherited evaluation state, stamps and presentation references
    if r.reference()? != owner {
        return Err(invalid("plane definition belongs to another feature"));
    }
    finish(r)
}

pub(super) fn tracked_plane(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<u64> {
    let entity = tracked_entity(records, classes, id, owner)?;
    geometry_version(classes, entity)?;
    Ok(entity)
}

pub(super) fn tracked_entity(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<u64> {
    require_class(classes, id, "5A1BF548-241F-46FD-9FB5-E4B05126EB9D", 1)?;
    let mut r = payload(records.primary(id)?, id)?;
    empty(&mut r)?;
    zeros(&mut r, 2)?;
    let tracker = r.reference()?;
    require_class(classes, tracker, "90055C05-546C-4EE7-B3C9-3DD922AD0C9C", 0)?;
    if r.u32()? != 1 {
        return Err(invalid("unsupported plane tracking layout"));
    }
    r.wide_string()?;
    r.wide_string()?;
    if r.u32()? != 2 {
        return Err(invalid("unsupported plane tracking kind"));
    }
    super::history::selection_flag(&mut r)?;
    let version = records
        .design
        .classes
        .iter()
        .find(|c| c.id == "A0A15D26-1F3B-4120-A3F1-9CDDA189AB74")
        .map(|c| c.version)
        .unwrap_or(if records.design.layout == 8 { 2 } else { 0 });
    let length = match version {
        1 => 59,
        2 => 60,
        _ => return Err(invalid("unsupported tracked-plane inherited schema")),
    };
    evaluation_tail(&mut r, classes, owner, length)?;
    let mut r = payload(records.primary(tracker)?, tracker)?;
    empty(&mut r)?;
    zeros(&mut r, 2)?;
    let entity = r.u64()?;
    finish(&r)?;
    Ok(entity)
}

/// Only root origin planes may terminate an imported offset chain. A cached face
/// or a later construction plane must never become a frozen custom base plane.
pub(super) fn origin(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, root: u64) -> Result<(String, Plane)> {
    let version = geometry_version(classes, id)?;
    let mut r = payload(records.primary(id)?, id)?;
    empty(&mut r)?;
    zeros(&mut r, 1)?;
    super::attributes::read(&mut r)?;
    if version == 1 {
        zeros(&mut r, 4)?; // explicit origin role added in geometry version 1
    }
    r.take(17)?; // display extents and flags
    let mut vector = || -> Result<Vec3> { Ok(Vec3::new(f64::from_bits(r.u64()?), f64::from_bits(r.u64()?), f64::from_bits(r.u64()?))) };
    let (o, x, y) = (vector()?, vector()?, vector()?);
    if !o.is_finite() || !x.is_finite() || !y.is_finite() {
        return Err(invalid("non-finite origin plane"));
    }
    r.take(if version == 0 { 21 } else { 26 })?; // presentation extents and origin identity flags
    let name = r.wide_string()?;
    let plane = match name.as_str() {
        "XY" => Plane::XY,
        "XZ" => Plane::XZ,
        "YZ" => Plane::YZ,
        _ => return Err(invalid("unsupported origin-plane name")),
    };
    if o.len() > 1e-10 || (x.cross(y) - plane.normal()).len() > 1e-10 {
        return Err(invalid("origin plane name and geometry disagree"));
    }
    r.take(if version == 0 { 36 } else { 37 })?;
    let group = r.reference()?;
    finish(&r)?;
    require_class(classes, group, "2972A3B8-3627-49BC-8C3D-18707605EC44", 1)?;
    let mut r = payload(records.primary(group)?, group)?;
    empty(&mut r)?;
    zeros(&mut r, 4)?;
    if !references(&mut r)?.contains(&id) || r.reference()? != root {
        return Err(invalid("plane is not part of the root origin"));
    }
    finish(&r)?;
    Ok((name, plane))
}

fn geometry_version(classes: &BTreeMap<u64, &Class>, id: u64) -> Result<u32> {
    classes
        .get(&id)
        .filter(|c| c.id == GEOMETRY && matches!(c.version, 0 | 1))
        .map(|c| c.version)
        .ok_or_else(|| invalid("unsupported plane geometry class or version"))
}

pub(super) fn same_surface(a: Plane, b: Plane) -> bool {
    (a.origin - b.origin).len() < 1e-7 && (a.normal() - b.normal()).len() < 1e-9
}

pub(super) fn same_support_plane(a: Plane, b: Plane) -> bool {
    (a.origin - b.origin).dot(a.normal()).abs() < 1e-7 && (a.normal() - b.normal()).len() < 1e-9
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::f3d::import_tests::Fixture;
    use crate::f3d::relationship_tests::{reference, refs, u32};

    /// Original version-8 definition with a face marker at `marker` and an
    /// evaluation tail of `tail` bytes.
    fn attached(marker: Option<usize>, tail: usize) -> Design {
        let mut b = vec![0; 4];
        let mut header = [0; 31];
        if let Some(at) = marker {
            header[at..at + 8].copy_from_slice(&1_f64.to_le_bytes());
        }
        b.extend(header);
        b.push(1); // identity transform
        b.extend([0; 35]);
        u32(&mut b, 2);
        refs(&mut b, &[]);
        refs(&mut b, &[11]);
        b.extend([0; 2]);
        refs(&mut b, &[]);
        b.extend(vec![0; tail]);
        reference(&mut b, 12);
        let mut f = Fixture(BTreeMap::new());
        f.put(10, DEFINITION, 8, b, None);
        f.put(11, "5A1BF548-241F-46FD-9FB5-E4B05126EB9D", 1, vec![], None);
        f.put(12, "8DA771B7-52ED-42FC-94E2-2353FE141373", 4, vec![], None);
        f.design()
    }

    #[test]
    fn version_8_face_markers_select_their_evaluation_tail() {
        for (marker, tail, admitted) in
            [(Some(18), 59, true), (Some(18), 60, false), (Some(10), 60, true), (Some(10), 59, false), (Some(14), 60, false), (None, 59, true)]
        {
            let d = attached(marker, tail);
            let records = Records::new(&d);
            let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
            let result = raw_definition(&records, &classes, 10, 12, true).map(|_| ());
            assert_eq!(result.is_ok(), admitted, "marker {marker:?}, tail {tail}: {result:?}");
        }
    }
}
