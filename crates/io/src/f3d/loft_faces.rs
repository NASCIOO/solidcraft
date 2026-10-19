//! Explicit extrusion-cap and side identities observed in local loft controls.
//! Persistent producer/operation references are authoritative; cached geometry
//! is never used to choose a nearby face.
use super::history::{empty, finish, payload, references, require_class, zeros};
use super::*;

#[path = "composite_caps.rs"]
mod composite_caps;
#[cfg(test)]
pub(super) use composite_caps::cap_anchor as joined_cap_anchor;
pub(super) use composite_caps::{anchor as composite_cap_anchor, cap_indices as joined_cap_indices};

const IDENTITY: &str = "7ACC2A03-0261-4879-A14A-A93D661A5BDC";
const HANDLE: &str = "D26351F0-5940-4D23-AA20-2C35475A6D9E";

pub(super) struct Face {
    pub body: u64,
    pub feature: u64,
    pub high: bool,
    pub bounded: bool,
    pub rectangular: bool,
    pub side: Option<u64>,
    pub side_roles: BTreeMap<u64, u64>,
    pub cap_roles: BTreeMap<u64, bool>,
    pub contributions: Vec<u64>,
    pub all_operations: Vec<u64>,
    pub boundary_operations: Vec<u64>,
}

struct Recipe {
    operations: Vec<u64>,
    all_operations: Vec<u64>,
    boundary_operations: Vec<u64>,
    high: bool,
    bounded: bool,
    rectangular: bool,
    side: Option<u64>,
    side_roles: BTreeMap<u64, u64>,
    cap_roles: BTreeMap<u64, bool>,
}

impl Face {
    pub fn cap(feature: u64, operation: u64, high: bool, rectangular: bool) -> Self {
        Self {
            body: 0,
            feature,
            high,
            bounded: true,
            rectangular,
            side: None,
            side_roles: BTreeMap::new(),
            cap_roles: BTreeMap::new(),
            contributions: vec![operation],
            all_operations: vec![operation],
            boundary_operations: Vec::new(),
        }
    }
    pub fn validate_profile(&self, profile: &SketchProfile, circles: &BTreeMap<u64, SketchCircle>, lines: &BTreeMap<u64, SketchLine>) -> Result<()> {
        // An unbounded cap selector names the producer's entire support plane;
        // its identity is independent of the profile's number of boundary edges.
        // The producer/profile and exact cap certificate are checked on import.
        if !self.bounded && self.side.is_none() {
            return Ok(());
        }
        let [loops] = profile.regions.as_slice() else { return Err(invalid("unsupported loft face producer regions")) };
        let [boundary] = loops.as_slice() else { return Err(invalid("unsupported loft face producer holes")) };
        if !boundary.outer || boundary.pieces.iter().any(|piece| *piece != (1, 1)) {
            return Err(invalid("unsupported loft cap boundary pieces"));
        }
        if self.side.is_some() {
            if !(3..=1024).contains(&boundary.curves.len())
                || boundary.curves.iter().any(|id| !lines.get(id).is_some_and(|line| line.sketch == profile.geometry))
            {
                return Err(invalid("loft side requires a complete owned line profile"));
            }
            return Ok(());
        }
        if !self.rectangular {
            let [circle] = boundary.curves.as_slice() else { return Err(invalid("loft face recipe requires a circular producer")) };
            if !circles.get(circle).is_some_and(|c| c.sketch == profile.geometry) {
                return Err(invalid("missing loft cap circle"));
            }
            return Ok(());
        }
        if boundary.curves.len() != 4 {
            return Err(invalid("loft face recipe requires four rectangle sides"));
        }
        use solidcraft_geom::Vec3;
        let mut sides = Vec::new();
        for id in &boundary.curves {
            let line = lines.get(id).filter(|l| l.sketch == profile.geometry).ok_or_else(|| invalid("missing loft rectangle side"))?;
            let [x, y, z] = line.start;
            let [u, v, w] = line.end;
            let (a, b) = (Vec3::new(x, y, z), Vec3::new(u, v, w));
            if z.abs() > 1e-10 || w.abs() > 1e-10 || (b - a).len() < 1e-9 {
                return Err(invalid("degenerate or nonplanar loft rectangle"));
            }
            sides.push((a, b));
        }
        for (i, (a, b)) in sides.iter().enumerate() {
            for vertex in [a, b] {
                let adjacent: Vec<_> =
                    sides.iter().enumerate().filter(|(j, (c, d))| *j != i && ((*vertex - *c).len() < 1e-9 || (*vertex - *d).len() < 1e-9)).collect();
                let [(_, (c, d))] = adjacent.as_slice() else { return Err(invalid("ambiguous loft rectangle incidence")) };
                if (*b - *a).dot(*d - *c).abs() > 1e-9 * (*b - *a).len() * (*d - *c).len() {
                    return Err(invalid("loft cap producer is not a rectangle"));
                }
            }
        }
        Ok(())
    }

    pub fn anchor(
        &self,
        doc: &solidcraft_doc::Document,
        state: &solidcraft_doc::ModelState,
        id: u64,
        contributions: &[(u64, Option<String>)],
    ) -> Result<solidcraft_doc::FaceAnchor> {
        let feature = doc.feature(id).filter(|f| !f.suppressed).ok_or_else(|| invalid("missing loft face producer"))?;
        let [body] = feature.body_names.as_slice() else {
            return Err(invalid("loft face requires one explicitly produced body"));
        };
        let body = state.body(body).ok_or_else(|| invalid("loft face body no longer exists"))?;
        let sources = body.body.face_sources().ok_or_else(|| invalid("loft face has no construction identities"))?;
        // Native cap labels name start/end along the extrusion's signed travel;
        // kernel certificates name the geometrically low/high profile planes.
        let cap_high = if self.side.is_none() {
            let [(producer, None)] = contributions else { return Err(invalid("unsupported cap contributors")) };
            doc.extrusion_cap_high(*producer, self.high).map_err(|e| invalid(&e.to_string()))?
        } else {
            self.high
        };
        let matches = |source: &solidcraft_kernel::FaceSource, (id, curve): &(u64, Option<String>)| match (source, curve) {
            (solidcraft_kernel::FaceSource::CertifiedExtrudeCap { producer, high, .. }, None) => producer == id && *high == cap_high,
            (solidcraft_kernel::FaceSource::CertifiedExtrudeSide { producer, curve: current, plane: Some(_), .. }, Some(curve)) => {
                producer == id && current == curve
            }
            _ => false,
        };
        let indices: Vec<_> = sources
            .iter()
            .enumerate()
            .filter_map(|(index, source)| {
                let valid = match (source, contributions) {
                    (Some(source), [only]) => matches(source, only),
                    (Some(solidcraft_kernel::FaceSource::PlanarUnion { sources, .. }), parts) if parts.len() >= 2 => {
                        sources.len() == parts.len() && parts.iter().all(|part| sources.iter().any(|source| matches(source, part)))
                    }
                    _ => false,
                };
                valid.then_some(index)
            })
            .collect();
        let [index] = indices.as_slice() else {
            if indices.is_empty()
                && self.side.is_none()
                && self.high
                && let [(producer, None)] = contributions
            {
                return cut_end_anchor(doc, state, body, *producer);
            }
            return Err(invalid("native loft face is missing or ambiguous"));
        };
        let plane = body.body.face_plane(*index).ok_or_else(|| invalid("loft face has no exact plane"))?;
        let mut anchor = doc.capture_face_anchor(body, *index, &plane, &[], 0, &[]).map_err(|e| invalid(&e.to_string()))?;
        if self.side.is_none() {
            anchor.target = doc.track_extrusion_cap(&anchor.target, self.high).map_err(|e| invalid(&e.to_string()))?;
        }
        Ok(anchor)
    }
}

/// Coplanar Cut tools may be widened outside the stock. Their profile no longer
/// has an exact sketch-boundary certificate, but Boolean history still names the
/// exposed cap. Resolve that explicit producer role and verify its nominal end
/// plane; never select a face by proximity or by a generated face index.
fn cut_end_anchor(
    doc: &solidcraft_doc::Document,
    state: &solidcraft_doc::ModelState,
    body: &solidcraft_doc::ModelBody,
    producer: u64,
) -> Result<solidcraft_doc::FaceAnchor> {
    use solidcraft_doc::config::{ConfigFaceTarget, FaceCertificate};
    use solidcraft_doc::{FeatureKind, Operation};
    use solidcraft_geom::Vec3;
    let feature = doc.feature(producer).filter(|f| !f.suppressed).ok_or_else(|| invalid("missing cut cap producer"))?;
    let FeatureKind::Extrude { sketch, extent, operation: Operation::Cut, targets, .. } = &feature.kind else {
        return Err(invalid("native loft face is missing or ambiguous"));
    };
    if extent.from.is_some() || extent.start_offset.is_some() || targets.as_slice() != [body.name.clone()] {
        return Err(invalid("unsupported cut cap extent or target"));
    }
    // Also rejects two-sided, tapered, symmetric, through-all and To extents.
    let high = doc.extrusion_cap_high(producer, true)?;
    let distance = doc.eval(&extent.distance, solidcraft_doc::expr::Kind::Length)?.abs() * if high { 1. } else { -1. };
    let sketch = state.sketch(sketch.0).ok_or_else(|| invalid("missing cut cap sketch"))?;
    let expected = sketch.plane.offset(distance);
    let target = ConfigFaceTarget {
        body: body.definition_name().into(),
        origin_feature: producer,
        name: format!("F{producer}:{}", if high { "top" } else { "bottom" }),
        point: Vec3::ZERO,
        certificate: Some(FaceCertificate::Lineage),
    };
    let indices = doc.configuration_face_indices(body, &target)?;
    let [index] = indices.as_slice() else { return Err(invalid("ambiguous cut cap history")) };
    let plane = body.body.planar_face(*index).ok_or_else(|| invalid("cut cap is not planar"))?;
    let normal = expected.normal();
    let tolerance = 1e-9 * (1. + distance.abs() + body.body.size());
    if (plane.origin - expected.origin).dot(normal).abs() > tolerance || plane.normal().dot(normal * if high { -1. } else { 1. }) < 1. - 1e-9 {
        return Err(invalid("cut cap history does not match the nominal extrusion end"));
    }
    let mut anchor = doc.capture_face_anchor(body, *index, &plane, &[], 0, &[])?;
    anchor.target = target;
    doc.resolve_face_anchor(&anchor, 0, state)?;
    Ok(anchor)
}

pub(super) fn objects(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<Vec<u64>> {
    require_class(classes, id, "2CA5A1CD-C99B-4C9B-91AF-57989148E841", 1)?;
    let bytes = records.primary(id)?;
    let mut r = payload(bytes, id)?;
    empty(&mut r)?;
    zeros(&mut r, 2)?;
    let objects = references(&mut r)?;
    if objects.iter().any(|id| !classes.contains_key(id)) {
        return Err(invalid("missing loft container source"));
    }
    r.at = bytes.len().checked_sub(11).filter(|at| *at >= r.at).ok_or_else(|| invalid("truncated loft container"))?;
    if r.reference()? != owner {
        return Err(invalid("inconsistent loft container owner"));
    }
    finish(&r)?;
    Ok(objects)
}

fn cap_recipe(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64) -> Result<Recipe> {
    require_class(classes, id, IDENTITY, 3)?;
    let mut r = payload(records.primary(id)?, id)?;
    zeros(&mut r, 6)?;
    integers(&mut r, &[1, 3])?;
    let count = r.u32()?;
    if !matches!(count, 1 | 2 | 3 | 5) {
        return Err(invalid("unsupported loft face term count"));
    }
    let mut terms = Vec::new();
    let mut all_operations = BTreeSet::new();
    for _ in 0..count {
        let n = r.u32()?;
        if !(1..=64).contains(&n) {
            return Err(invalid("unsupported loft face alternative count"));
        }
        let mut term = BTreeMap::new();
        for _ in 0..n {
            let label = r.string()?;
            zeros(&mut r, 4)?;
            if r.u32()? != 1 {
                return Err(invalid("unsupported loft face operation count"));
            }
            let operation = r.u64()?;
            if operation == 0 || term.insert(operation, label).is_some() {
                return Err(invalid("invalid or duplicate loft face operation"));
            }
            all_operations.insert(operation);
        }
        if terms.contains(&term) {
            return Err(invalid("duplicate loft face term"));
        }
        terms.push(term);
    }
    if all_operations.len() > 64 {
        return Err(invalid("too many loft face contributors"));
    }
    let first = terms.first().ok_or_else(|| invalid("missing face terms"))?;
    if count == 1 {
        let (operation, label) = first.iter().next().ok_or_else(|| invalid("missing unbounded face role"))?;
        let role = label
            .parse::<u64>()
            .ok()
            .filter(|role| *role > 0 && role.to_string() == *label)
            .ok_or_else(|| invalid("unsupported unbounded face role"))?;
        zeros(&mut r, 4)?;
        if r.string()? != "face_recipe_data" {
            return Err(invalid("missing unbounded face recipe"));
        }
        integers(&mut r, &[0, -1])?;
        finish(&r)?;
        let side = (role > 2).then_some(role);
        let mut cap_roles = BTreeMap::new();
        if first.len() > 1 {
            for (operation, label) in first {
                let end = match label.as_str() {
                    "1" => false,
                    "2" => true,
                    _ => return Err(invalid("unsupported unbounded face contributors")),
                };
                cap_roles.insert(*operation, end);
            }
        }
        return Ok(Recipe {
            operations: first.keys().copied().collect(),
            all_operations: first.keys().copied().collect(),
            boundary_operations: Vec::new(),
            high: role == 2,
            bounded: false,
            rectangular: false,
            side,
            side_roles: side.map(|role| BTreeMap::from([(*operation, role)])).unwrap_or_default(),
            cap_roles,
        });
    }
    let mut boundary_operations = Vec::new();
    let (high, rectangular, side, side_roles) = if count == 2 {
        let labels: Vec<_> = terms.iter().filter_map(|term| (term.len() == 1).then(|| term.values().next()).flatten()).map(String::as_str).collect();
        let high = match labels.as_slice() {
            ["2", "1"] => true,
            ["1", "1"] => false,
            _ => return Err(invalid("unsupported trimmed circular cap recipe")),
        };
        if all_operations.len() != 2 {
            return Err(invalid("trimmed cap requires distinct cap and boundary operations"));
        }
        boundary_operations.extend(terms.iter().skip(1).flat_map(|term| term.keys()).copied());
        (high, false, None, BTreeMap::new())
    } else if count == 3 {
        let labels: Vec<_> = terms.iter().filter_map(|term| (term.len() == 1).then(|| term.values().next()).flatten()).map(String::as_str).collect();
        let high = match labels.as_slice() {
            ["2", "-1", "3"] => true,
            ["1", "-102", "3"] => false,
            _ => return Err(invalid("unsupported circular loft face recipe")),
        };
        if all_operations.len() != 1 {
            return Err(invalid("unsupported composite circular loft face"));
        }
        (high, false, None, BTreeMap::new())
    } else {
        let roles = terms
            .iter()
            .map(|term| {
                term.iter()
                    .map(|(op, label)| {
                        let role = label
                            .parse::<u64>()
                            .ok()
                            .filter(|role| *role != 0 && role.to_string() == *label)
                            .ok_or_else(|| invalid("unsupported planar loft face label"))?;
                        Ok((*op, role))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let selected = roles.first().ok_or_else(|| invalid("missing planar face roles"))?;
        let role = *selected.values().next().ok_or_else(|| invalid("missing selected face role"))?;
        if role <= 2 {
            if selected.values().any(|r| *r != role) || roles.iter().skip(1).flat_map(|term| term.values()).any(|r| *r < 3) {
                return Err(invalid("unsupported planar cap incidence roles"));
            }
            (role == 2, true, None, BTreeMap::new())
        } else {
            // A side on a joined extrusion can be bounded by the upper cap of
            // an earlier producer and the upper cap of its own producer. Cap
            // orientation and term order therefore do not identify the side.
            let cap_terms = roles.iter().skip(1).filter(|term| term.values().all(|r| *r <= 2)).count();
            let side_terms = roles.iter().skip(1).filter(|term| term.values().all(|r| *r >= 3)).count();
            if selected.values().any(|r| *r < 3) || cap_terms != 2 || side_terms != 2 {
                return Err(invalid("unsupported planar side incidence roles"));
            }
            (false, true, Some(role), selected.clone())
        }
    };
    zeros(&mut r, 4)?;
    if r.string()? != "bounded_face_recipe_data" {
        return Err(invalid("missing bounded loft face recipe"));
    }
    zeros(&mut r, 4)?;
    if count == 2 {
        integers(&mut r, &[-1, 1, -1, -1, 2, 0, -1, 1, -1, 1, -1, 2])?;
        integers(&mut r, &if high { [0, -1, 1, -1, 0, 1, 1, 2, 2, 2, 2, 2, 1, 2, -1] } else { [1, -1, 0, -1, 0, 1, 0, 2, 1, 1, 1, 1, 0, 1, -1] })?;
    } else if rectangular {
        quadrilateral(&mut r)?;
    } else {
        integers(&mut r, &[-1, 1, 1, -1, 2, 2, -1, 0, -1, 1, -1, 2, 2, -1, 0, -1, 1, 1, -1, 0, 0, -1, 1, 0])?;
        integers(&mut r, &if high { [2, 2, 2, 2, 1, 0, 1, -1] } else { [2, 1, 1, 1, 2, 1, 2, -1] })?;
    }
    finish(&r)?;
    Ok(Recipe {
        operations: first.keys().copied().collect(),
        all_operations: all_operations.into_iter().collect(),
        boundary_operations,
        high,
        bounded: true,
        rectangular,
        side,
        side_roles,
        cap_roles: BTreeMap::new(),
    })
}

fn integers(r: &mut Reader<'_>, expected: &[i32]) -> Result<()> {
    for value in expected {
        if r.u32()? != *value as u32 {
            return Err(invalid("unsupported loft face incidence encoding"));
        }
    }
    Ok(())
}

/// A projected cap must name one operation and a complete closed
/// boundary. The edge mappings are checked against this identity separately.
pub(super) fn projection_cap(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64) -> Result<(u64, bool, bool)> {
    let recipe = cap_recipe(records, classes, id)?;
    let [operation] = recipe.operations.as_slice() else { return Err(invalid("projected cap requires one producer")) };
    if !recipe.bounded || recipe.side.is_some() || recipe.all_operations.as_slice() != [*operation] {
        return Err(invalid("unsupported projected face boundary"));
    }
    Ok((*operation, recipe.high, recipe.rectangular))
}

/// Verify the four-edge boundary graph, independent of edge ordering and corner
/// orientation. Every edge uses the selected face and one distinct neighbor;
/// every corner joins two such edges and occurs exactly twice.
fn quadrilateral(r: &mut Reader<'_>) -> Result<()> {
    integers(r, &[-1, 4])?;
    quadrilateral_edges(r, 0)
}

/// Four straight boundary edges after optional inner-loop terms. Term zero is
/// the cap and the four consecutive terms after `offset` are its side faces.
pub(super) fn quadrilateral_edges(r: &mut Reader<'_>, offset: u32) -> Result<()> {
    let allowed = (offset + 1)..=(offset + 4);
    let mut neighbors = BTreeSet::new();
    let mut corners = BTreeMap::new();
    for _ in 0..4 {
        integers(r, &[-1, -1, 2])?;
        let a = r.u32()?;
        integers(r, &[-1])?;
        let b = r.u32()?;
        if !allowed.contains(&a.max(b)) || a.min(b) != 0 || a == b || !neighbors.insert(a.max(b)) {
            return Err(invalid("invalid planar loft boundary edge"));
        }
        integers(r, &[-1, 2])?;
        let mut previous = None;
        for _ in 0..2 {
            integers(r, &[-1, 3])?;
            let mut corner = [0; 3];
            for role in &mut corner {
                *role = r.u32()?;
                integers(r, &[-1])?;
            }
            integers(r, &[0, 0])?;
            corner.sort();
            if corner.iter().any(|role| *role != 0 && !allowed.contains(role))
                || corner.windows(2).any(|pair| pair.first() == pair.get(1))
                || !corner.contains(&a)
                || !corner.contains(&b)
                || previous == Some(corner)
            {
                return Err(invalid("invalid planar loft boundary corner"));
            }
            *corners.entry(corner).or_insert(0) += 1;
            previous = Some(corner);
        }
        integers(r, &[-1])?;
    }
    if corners.len() != 4 || corners.values().any(|count| *count != 2) {
        return Err(invalid("planar loft boundary is not one closed quadrilateral"));
    }
    Ok(())
}

pub(super) fn producer(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, handle: u64) -> Result<u64> {
    extrusion_producer(records, classes, handle, false)
}

fn extrusion_producer(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, handle: u64, allow_mirror: bool) -> Result<u64> {
    require_class(classes, handle, HANDLE, 2)?;
    let mut r = payload(records.primary(handle)?, handle)?;
    zeros(&mut r, 6)?;
    if r.wide_string()?.is_empty() {
        return Err(invalid("unsupported loft body identity"));
    }
    let flags = r.take(2)?;
    // This final-state flag says a later feature consumed the body. Earlier
    // selectors still need its producer; timeline suppression is checked separately.
    if !matches!(flags, [0, 1] | [1, 1]) {
        return Err(invalid("unsupported loft body identity"));
    }
    let body = r.reference()?;
    let feature = r.reference()?;
    finish(&r)?;
    if !classes.get(&body).is_some_and(|c| c.id == "D3937028-C20C-4E65-B010-94AD418A5C20" && matches!(c.version, 18 | 19)) {
        return Err(invalid("unsupported loft source body class or version"));
    }
    let mut r = payload(records.secondary(handle)?, handle)?;
    empty(&mut r)?;
    require_class(classes, r.reference()?, "C3423E04-7E85-4B44-8E31-A5FA9C100052", 2)?;
    finish(&r)?;
    if allow_mirror && classes.get(&feature).is_some_and(|c| c.id == super::circular_patterns::CLASS && c.version == 0) {
        let pattern = super::circular_patterns::read_inputs(records, classes, feature)?;
        if !pattern.outputs.contains(&handle) {
            return Err(invalid("pattern body handle disagrees with its producer"));
        }
        return Ok(feature);
    }
    let mut r = payload(records.primary(feature)?, feature)?;
    if allow_mirror && classes.get(&feature).is_some_and(|c| c.id == super::mirrors::CLASS && c.version == 0) {
        empty(&mut r)?;
        zeros(&mut r, 1)?;
        if !super::attributes::read(&mut r)?.is_empty() {
            return Err(invalid("unsupported mirror source attributes"));
        }
        r.reference()?; // helper and source ownership are checked by mirrors::inputs
        r.reference()?;
    } else {
        require_class(classes, feature, "DD405BC2-D673-44F0-8833-5CB2A1C186C7", 8)?;
        let input = super::history::extrude_inputs(&mut r, classes)?;
        if input.operation != solidcraft_doc::Operation::NewBody || input.start_offset.is_some() {
            return Err(invalid("loft face requires an independent profile-plane extrusion"));
        }
        references(&mut r)?;
    }
    let version = records
        .design
        .classes
        .iter()
        .find(|c| c.id == "2EDC2536-D95B-4469-8ACA-DC07124D9C43")
        .map(|c| c.version)
        .unwrap_or(if records.design.layout == 8 { 2 } else { 0 });
    let roles = super::history::body_roles(&mut r, classes, version)?;
    if roles.get(1).is_none_or(|ids| ids.as_slice() != [handle]) || roles.iter().enumerate().any(|(i, ids)| i != 1 && !ids.is_empty()) {
        return Err(invalid("loft body handle disagrees with its producer"));
    }
    Ok(feature)
}

pub(super) fn selector_tail(r: &mut Reader<'_>, records: &Records<'_>, owner: u64) -> Result<u64> {
    let handles = references(r)?;
    let [handle] = handles.as_slice() else {
        return Err(invalid("loft selector requires one body"));
    };
    selector_owner_tail(r, records, owner)?;
    Ok(*handle)
}

fn selector_owner_tail(r: &mut Reader<'_>, records: &Records<'_>, owner: u64) -> Result<()> {
    let version = records
        .design
        .classes
        .iter()
        .find(|c| c.id == "A0A15D26-1F3B-4120-A3F1-9CDDA189AB74")
        .map(|c| c.version)
        .unwrap_or(if records.design.layout == 8 { 2 } else { 0 });
    r.take(match version {
        1 => 55,
        2 => 56,
        _ => return Err(invalid("unsupported loft selector inherited schema")),
    })?; // inherited evaluation/cache state
    if r.reference()? != owner {
        return Err(invalid("loft selector belongs to another feature"));
    }
    finish(r)?;
    Ok(())
}

pub(super) fn selector_version(classes: &BTreeMap<u64, &Class>, id: u64) -> Result<u32> {
    classes
        .get(&id)
        .filter(|c| c.id == super::body_projections::SELECTOR && matches!(c.version, 2 | 3))
        .map(|c| c.version)
        .ok_or_else(|| invalid("unsupported loft selector class or version"))
}

pub(super) fn face(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<Face> {
    let version = selector_version(classes, id)?;
    let mut r = payload(records.primary(id)?, id)?;
    zeros(&mut r, if version == 2 { 7 } else { 8 })?;
    let recipe = cap_recipe(records, classes, r.reference()?)?;
    if r.u32()? != 1 {
        return Err(invalid("unsupported loft face source count"));
    }
    r.wide_string()?;
    r.wide_string()?;
    if r.u32()? != 2 {
        return Err(invalid("unsupported loft face selection type"));
    }
    super::history::selection_flag(&mut r)?;
    let cached = references(&mut r)?;
    let [cached] = cached.as_slice() else {
        return Err(invalid("loft face requires one cached identity"));
    };
    require_class(classes, *cached, "C46D3EEB-40E1-43FA-83DC-9CB204335417", 1)?;
    let handle = selector_tail(&mut r, records, owner)?;
    let (body, feature) = super::body_projections::body_owner(records, classes, handle)?;
    Ok(Face {
        body,
        feature,
        high: recipe.high,
        bounded: recipe.bounded,
        rectangular: recipe.rectangular,
        side: recipe.side,
        side_roles: recipe.side_roles,
        cap_roles: recipe.cap_roles,
        contributions: recipe.operations,
        all_operations: recipe.all_operations,
        boundary_operations: recipe.boundary_operations,
    })
}

#[derive(Debug, PartialEq)]
pub(super) struct TargetOperation {
    pub operation: u64,
    pub label: u32,
    pub aliases: Vec<u32>,
    pub flags: u32,
}

#[derive(Debug, PartialEq)]
pub(super) struct Target {
    pub feature: u64,
    pub operations: Vec<TargetOperation>,
}

pub(super) fn target(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, container: u64, owner: u64) -> Result<Target> {
    let objects = objects(records, classes, container, owner)?;
    let [id] = objects.as_slice() else {
        return Err(invalid("loft target requires one body"));
    };
    target_object(records, classes, *id, owner)
}

pub(super) fn target_object(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<Target> {
    selected_body(records, classes, id, owner, false).map(|(target, _)| target)
}

pub(super) fn combine_target(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<(Target, Option<u64>)> {
    selected_body(records, classes, id, owner, true)
}

fn selected_body(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64, combine: bool) -> Result<(Target, Option<u64>)> {
    require_class(classes, id, "9716F783-676D-42E0-93F9-EBF273E7C035", 1)?;
    let mut r = payload(records.primary(id)?, id)?;
    zeros(&mut r, 6)?;
    let count = r.count(12)?;
    if !(1..=64).contains(&count) {
        return Err(invalid("unsupported loft target operation count"));
    }
    let mut operations = Vec::new();
    let mut seen = BTreeSet::new();
    for i in 0..count {
        let operation = r.u64()?;
        let flags = r.u32()?;
        if operation == 0
            || !seen.insert(operation)
            || if i == 0 { !(matches!(flags, 32 | 33) || (combine && flags == 3 && count == 1)) } else { !matches!(flags, 4 | 128 | 129) }
        {
            return Err(invalid("unsupported or duplicate loft target operation"));
        }
        operations.push(TargetOperation { operation, label: 0, aliases: Vec::new(), flags });
    }
    let last_loft = operations.iter().rposition(|operation| matches!(operation.flags, 128 | 129));
    for (i, operation) in operations.iter().enumerate() {
        let expected = if i == 0 {
            if combine && operation.flags == 3 {
                3
            } else if last_loft.is_some() {
                32
            } else {
                33
            }
        } else if matches!(operation.flags, 128 | 129) {
            if last_loft == Some(i) { 129 } else { 128 }
        } else {
            4
        };
        if operation.flags != expected {
            return Err(invalid("inconsistent loft target current-body ownership"));
        }
    }
    let identity = r.reference()?;
    require_class(classes, identity, IDENTITY, 3)?;
    let mut identity = payload(records.primary(identity)?, identity)?;
    zeros(&mut identity, 6)?;
    for value in [1, 3, 1] {
        if identity.u32()? != value {
            return Err(invalid("unsupported loft body recipe header"));
        }
    }
    let recipes = identity.count(16)?;
    if !(count..=64).contains(&recipes) || (!combine && recipes != count) {
        return Err(invalid("unsupported body recipe entry count"));
    }
    let mut previous = 0;
    for _ in 0..recipes {
        let tag = identity.string()?;
        let index =
            operations.iter().position(|op| op.operation.to_string() == tag).ok_or_else(|| invalid("loft target recipe disagrees with operation"))?;
        if index < previous {
            return Err(invalid("body recipe operation order disagrees with selector"));
        }
        previous = index;
        let operation = operations.get_mut(index).ok_or_else(|| invalid("missing body recipe operation"))?;
        let label = identity.u32()?;
        if label < if combine && operation.flags == 3 { 1 } else { 3 } {
            return Err(invalid("invalid loft target body label"));
        }
        if operation.label == 0 {
            operation.label = label;
        } else if label == operation.label || operation.aliases.contains(&label) {
            return Err(invalid("duplicate whole-body recipe label"));
        } else {
            operation.aliases.push(label);
        }
        zeros(&mut identity, 8)?;
    }
    if operations.iter().any(|op| op.label == 0) {
        return Err(invalid("body recipe omits a selected operation"));
    }
    zeros(&mut identity, 4)?;
    if identity.string()? != "body_recipe_data" || identity.u32()? != 0 || identity.u32()? != u32::MAX {
        return Err(invalid("unsupported loft body recipe"));
    }
    finish(&identity)?;
    if r.u32()? != 1 {
        return Err(invalid("unsupported loft target source count"));
    }
    r.wide_string()?;
    r.wide_string()?;
    if r.u32()? != 2 {
        return Err(invalid("unsupported loft target selection type"));
    }
    super::history::selection_flag(&mut r)?;
    if combine {
        for cached in references(&mut r)? {
            require_class(classes, cached, "C46D3EEB-40E1-43FA-83DC-9CB204335417", 1)?;
        }
    } else {
        zeros(&mut r, 4)?;
    }
    // A retired pattern copy is encoded as one null handle, not an empty list.
    // Its operation/instance retirement must be certified by the caller.
    let handle = if combine && r.bytes.get(r.at..r.at + 5) == Some(&[1, 0, 0, 0, 0]) {
        r.take(5)?;
        selector_owner_tail(&mut r, records, owner)?;
        None
    } else {
        Some(selector_tail(&mut r, records, owner)?)
    };
    let feature = if let Some(handle) = handle {
        let feature = extrusion_producer(records, classes, handle, combine)?;
        if classes.get(&feature).is_some_and(|c| matches!(c.id.as_str(), super::mirrors::CLASS | super::circular_patterns::CLASS))
            != operations.first().is_some_and(|op| op.flags == 3)
        {
            return Err(invalid("body selector mode disagrees with its producer"));
        }
        feature
    } else {
        if operations.len() != 1 || operations.first().is_none_or(|op| op.flags != 3) {
            return Err(invalid("null body selector is not a pattern copy"));
        }
        0 // resolved from the certified pattern operation by the Combine importer
    };
    Ok((Target { feature, operations }, handle))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::f3d::import_tests::{Fixture, fixture};
    use crate::f3d::relationship_tests::{reference, refs, string, u32, u64, wide};

    #[test]
    fn quadrilateral_incidence_accepts_ordering_but_rejects_broken_topology() {
        let mut edges = Vec::new();
        for (neighbor, ends) in [(1, [2, 4]), (2, [1, 3]), (3, [2, 4]), (4, [3, 1])] {
            let mut edge = vec![-1, -1, 2, 0, -1, neighbor, -1, 2];
            for other in ends {
                edge.extend([-1, 3, 0, -1, neighbor, -1, other, -1, 0, 0]);
            }
            edge.push(-1);
            edges.push(edge);
        }
        let check = |edges: &[Vec<i32>]| {
            let bytes: Vec<_> = [-1_i32, 4].into_iter().chain(edges.iter().flatten().copied()).flat_map(i32::to_le_bytes).collect();
            quadrilateral(&mut Reader { bytes: &bytes, at: 0 })
        };
        assert!(check(&edges).is_ok());
        edges.reverse();
        for edge in &mut edges {
            edge.swap(3, 5);
            edge.swap(10, 14);
            edge.swap(20, 24);
        }
        assert!(check(&edges).is_ok());
        for (index, value) in [(3, 0), (10, 9), (14, 4), (24, 2)] {
            let mut broken = edges.clone();
            broken[0][index] = value;
            assert!(check(&broken).is_err(), "invalid corner/edge at {index}");
        }
        let mut repeated = edges.clone();
        repeated[0] = repeated[1].clone();
        assert!(check(&repeated).is_err());
        assert!(check(&edges[..3]).is_err());
    }

    // Original records with arbitrary IDs, strings and operation values.
    // No exported binary payload or cached geometry is copied into the tests.
    pub(in crate::f3d) fn authored(high: bool) -> Fixture {
        let mut f = fixture(false);
        let mut output = Vec::new();
        refs(&mut output, &[80]);
        f.0.get_mut(&9).unwrap().2.splice(151..155, output);
        let mut handle = vec![0; 6];
        wide(&mut handle, "authored-body");
        handle.extend([0, 1]);
        reference(&mut handle, 81);
        reference(&mut handle, 9);
        let mut owner = vec![0; 4];
        reference(&mut owner, 4);
        f.put(80, HANDLE, 2, handle, Some(owner));
        f.put(81, "D3937028-C20C-4E65-B010-94AD418A5C20", 19, vec![], None);
        f.put(182, "C46D3EEB-40E1-43FA-83DC-9CB204335417", 1, vec![], None);
        let mut identity = vec![0; 6];
        for n in [1, 3, 3] {
            u32(&mut identity, n);
        }
        for label in if high { ["2", "-1", "3"] } else { ["1", "-102", "3"] } {
            u32(&mut identity, 1);
            string(&mut identity, label);
            u32(&mut identity, 0);
            u32(&mut identity, 1);
            u64(&mut identity, 8001);
        }
        u32(&mut identity, 0);
        string(&mut identity, "bounded_face_recipe_data");
        u32(&mut identity, 0);
        let tree = [-1_i32, 1, 1, -1, 2, 2, -1, 0, -1, 1, -1, 2, 2, -1, 0, -1, 1, 1, -1, 0, 0, -1, 1, 0];
        for n in tree.into_iter().chain(if high { [2, 2, 2, 2, 1, 0, 1, -1] } else { [2, 1, 1, 1, 2, 1, 2, -1] }) {
            u32(&mut identity, n as u32);
        }
        f.put(181, IDENTITY, 3, identity, None);
        let mut selector = vec![0; 8];
        reference(&mut selector, 181);
        u32(&mut selector, 1);
        wide(&mut selector, "authored-source");
        wide(&mut selector, "authored-face");
        u32(&mut selector, 2);
        u32(&mut selector, 0);
        refs(&mut selector, &[182]);
        tail(&mut selector);
        f.put(180, super::super::body_projections::SELECTOR, 3, selector, None);
        let mut identity = vec![0; 6];
        for n in [1, 3, 1, 1] {
            u32(&mut identity, n);
        }
        string(&mut identity, "8001");
        u32(&mut identity, 4);
        identity.extend([0; 12]);
        string(&mut identity, "body_recipe_data");
        u32(&mut identity, 0);
        u32(&mut identity, u32::MAX);
        f.put(191, IDENTITY, 3, identity, None);
        let mut selector = vec![0; 6];
        u32(&mut selector, 1);
        u64(&mut selector, 8001);
        u32(&mut selector, 33);
        reference(&mut selector, 191);
        u32(&mut selector, 1);
        wide(&mut selector, "authored-target");
        wide(&mut selector, "authored-body");
        u32(&mut selector, 2);
        selector.extend([0; 8]);
        tail(&mut selector);
        f.put(190, "9716F783-676D-42E0-93F9-EBF273E7C035", 1, selector, None);
        let mut container = vec![0; 6];
        refs(&mut container, &[190]);
        reference(&mut container, 19);
        f.put(192, "2CA5A1CD-C99B-4C9B-91AF-57989148E841", 1, container, None);
        f
    }

    fn tail(b: &mut Vec<u8>) {
        refs(b, &[80]);
        b.extend([0; 56]);
        reference(b, 19);
    }

    #[test]
    fn combine_recipe_aliases_keep_operations_and_reject_missing_or_duplicate_terms() {
        let make = |terms: &[(u64, u32)]| {
            let mut f = authored_rectangle(true);
            let mut b = vec![0; 6];
            for n in [1, 3, 1, terms.len() as u32] {
                u32(&mut b, n);
            }
            for (op, label) in terms {
                string(&mut b, &op.to_string());
                u32(&mut b, *label);
                b.extend([0; 8]);
            }
            u32(&mut b, 0);
            string(&mut b, "body_recipe_data");
            u32(&mut b, 0);
            u32(&mut b, u32::MAX);
            f.0.get_mut(&191).unwrap().2 = b;
            f
        };
        let read = |f: Fixture, combine| {
            let d = f.design();
            let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
            selected_body(&Records::new(&d), &classes, 190, 19, combine)
        };
        let selected = read(make(&[(8001, 7), (8001, 11)]), true).unwrap().0;
        assert_eq!(selected.operations[0].label, 7);
        assert_eq!(selected.operations[0].aliases, [11]);
        assert!(read(make(&[(8001, 7), (8001, 11)]), false).is_err());
        for terms in [vec![], vec![(8001, 7), (8001, 7)], vec![(8001, 7), (9001, 11)], vec![(8001, 0)], vec![(8001, 7); 65]] {
            assert!(read(make(&terms), true).is_err());
        }
        for keep in 0..make(&[(8001, 7), (8001, 11)]).0[&191].2.len() {
            let mut f = make(&[(8001, 7), (8001, 11)]);
            f.0.get_mut(&191).unwrap().2.truncate(keep);
            assert!(read(f, true).is_err(), "prefix {keep}");
        }
    }

    #[test]
    fn null_combine_handles_require_a_pattern_copy_selector_and_owned_tail() {
        let make = |flags: u32, owner: u64| {
            let mut f = authored_rectangle(true);
            let bytes = &mut f.0.get_mut(&190).unwrap().2;
            bytes[18..22].copy_from_slice(&flags.to_le_bytes());
            let mut handle = Vec::new();
            reference(&mut handle, 80);
            let at = bytes.windows(handle.len()).position(|b| b == handle).unwrap();
            bytes.splice(at..at + handle.len(), [0]);
            let end = bytes.len();
            bytes[end - 10..end - 2].copy_from_slice(&owner.to_le_bytes());
            f
        };
        let read = |f: Fixture| {
            let d = f.design();
            let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
            combine_target(&Records::new(&d), &classes, 190, 19)
        };
        let (selection, handle) = read(make(3, 19)).unwrap();
        assert_eq!(handle, None);
        assert_eq!(selection.feature, 0); // importer must resolve and certify retirement
        assert!(read(make(33, 19)).is_err());
        assert!(read(make(3, 9)).is_err());
        for keep in 0..make(3, 19).0[&190].2.len() {
            let mut f = make(3, 19);
            f.0.get_mut(&190).unwrap().2.truncate(keep);
            assert!(read(f).is_err(), "prefix {keep}");
        }
    }

    #[test]
    fn historical_body_selectors_can_resolve_later_consumed_extrusions() {
        let mut f = authored(true);
        let bytes = &mut f.0.get_mut(&80).unwrap().2;
        let at = bytes.len() - 24;
        bytes[at] = 1;
        let d = Fixture(f.0.clone()).design();
        let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        assert_eq!(producer(&Records::new(&d), &classes, 80).unwrap(), 9);
        assert_eq!(target_object(&Records::new(&d), &classes, 190, 19).unwrap().feature, 9);
        f.0.get_mut(&80).unwrap().2[at] = 2;
        assert!(decode(f, true).is_err());
    }

    fn authored_trimmed_cap(high: bool) -> Fixture {
        let mut f = authored(high);
        let mut b = vec![0; 6];
        for n in [1, 3, 2] {
            u32(&mut b, n);
        }
        for (label, operation) in [(if high { "2" } else { "1" }, 8001), ("1", 9013)] {
            u32(&mut b, 1);
            string(&mut b, label);
            u32(&mut b, 0);
            u32(&mut b, 1);
            u64(&mut b, operation);
        }
        u32(&mut b, 0);
        string(&mut b, "bounded_face_recipe_data");
        u32(&mut b, 0);
        let incident = [-1_i32, 1, -1, -1, 2, 0, -1, 1, -1, 1, -1, 2];
        let orientation = if high { [0, -1, 1, -1, 0, 1, 1, 2, 2, 2, 2, 2, 1, 2, -1] } else { [1, -1, 0, -1, 0, 1, 0, 2, 1, 1, 1, 1, 0, 1, -1] };
        for n in incident.into_iter().chain(orientation) {
            u32(&mut b, n as u32);
        }
        f.put(181, IDENTITY, 3, b, None);
        f
    }

    #[test]
    fn trimmed_cap_keeps_its_boundary_operation_and_rejects_broken_incidence() {
        for high in [false, true] {
            let decode = |f: Fixture| {
                let d = f.design();
                let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
                face(&Records::new(&d), &classes, 180, 19)
            };
            let face = decode(authored_trimmed_cap(high)).unwrap();
            assert_eq!(face.contributions, [8001]);
            assert_eq!(face.boundary_operations, [9013]);
            assert_eq!(face.all_operations, [8001, 9013]);
            assert_eq!(face.high, high);
            let bytes = authored_trimmed_cap(high).0[&181].2.clone();
            for keep in 0..bytes.len() {
                let mut f = authored_trimmed_cap(high);
                f.0.get_mut(&181).unwrap().2.truncate(keep);
                assert!(decode(f).is_err(), "truncated trimmed cap {high}/{keep}");
            }
            for at in [18, 47, bytes.len() - 5] {
                let mut f = authored_trimmed_cap(high);
                f.0.get_mut(&181).unwrap().2[at] ^= 1;
                assert!(decode(f).is_err(), "invalid trimmed cap {high}/{at}");
            }
            let mut f = authored_trimmed_cap(high);
            let b = &mut f.0.get_mut(&181).unwrap().2;
            let at = b.windows(8).position(|w| w == 9013_u64.to_le_bytes()).unwrap();
            b[at..at + 8].copy_from_slice(&8001_u64.to_le_bytes());
            assert!(decode(f).is_err(), "a cap cannot trim itself");
        }
    }

    fn authored_rectangle(high: bool) -> Fixture {
        let mut f = authored(high);
        let mut b = vec![0; 6];
        for n in [1, 3, 5] {
            u32(&mut b, n);
        }
        for label in [if high { "2" } else { "1" }, "3", "6", "4", "5"] {
            u32(&mut b, 1);
            string(&mut b, label);
            u32(&mut b, 0);
            u32(&mut b, 1);
            u64(&mut b, 8001);
        }
        u32(&mut b, 0);
        string(&mut b, "bounded_face_recipe_data");
        u32(&mut b, 0);
        u32(&mut b, u32::MAX);
        u32(&mut b, 4);
        // Field-authored edge/corner incidence, independent of rectangle_tree.
        let edges = if high {
            [[1, 2, 1, 0, 3, 0, 1], [3, 3, 0, 1, 4, 0, 3], [4, 4, 0, 3, 2, 0, 4], [2, 2, 0, 4, 2, 1, 0]]
        } else {
            [[1, 2, 0, 1, 3, 1, 0], [2, 2, 4, 0, 2, 0, 1], [4, 4, 3, 0, 2, 4, 0], [3, 3, 1, 0, 4, 3, 0]]
        };
        for [side, a, b0, c, d, e, g] in edges {
            for n in [-1_i32, -1, 2, side, -1, 0, -1, 2, -1, 3, a, -1, b0, -1, c, -1, 0, 0, -1, 3, d, -1, e, -1, g, -1, 0, 0, -1] {
                u32(&mut b, n as u32);
            }
        }
        f.put(181, IDENTITY, 3, b, None);
        f.0.get_mut(&191).unwrap().2[30..34].copy_from_slice(&7_u32.to_le_bytes());
        f
    }

    fn authored_joined_side() -> Fixture {
        let mut f = authored(true);
        let mut b = vec![0; 6];
        for n in [1, 3, 5] {
            u32(&mut b, n);
        }
        for (label, operations) in [("5", &[8001, 9001][..]), ("1", &[8001, 9001]), ("4", &[8001, 9001]), ("6", &[8001, 9001]), ("2", &[9001])] {
            u32(&mut b, operations.len() as u32);
            for operation in operations {
                string(&mut b, label);
                u32(&mut b, 0);
                u32(&mut b, 1);
                u64(&mut b, *operation);
            }
        }
        u32(&mut b, 0);
        string(&mut b, "bounded_face_recipe_data");
        u32(&mut b, 0);
        u32(&mut b, u32::MAX);
        u32(&mut b, 4);
        // Each edge joins two incident surfaces and two three-surface corners.
        let edges =
            [([0, 1], [[0, 2, 1], [3, 0, 1]]), ([0, 3], [[3, 0, 1], [3, 4, 0]]), ([0, 4], [[0, 4, 2], [3, 4, 0]]), ([2, 0], [[0, 2, 1], [0, 4, 2]])];
        for ([a, b0], vertices) in edges {
            for n in [-1_i32, -1, 2, a, -1, b0, -1, 2] {
                u32(&mut b, n as u32);
            }
            for [a, b0, c] in vertices {
                for n in [-1_i32, 3, a, -1, b0, -1, c, -1, 0, 0] {
                    u32(&mut b, n as u32);
                }
            }
            u32(&mut b, u32::MAX);
        }
        f.put(181, IDENTITY, 3, b, None);
        f
    }

    #[test]
    fn joined_side_recipe_keeps_body_creator_separate_from_contributors() {
        let d = authored_joined_side().design();
        let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let selected = face(&Records::new(&d), &classes, 180, 19).unwrap();
        assert_eq!(selected.feature, 9);
        assert_eq!(selected.side, Some(5));
        assert_eq!(selected.contributions, [8001, 9001]);
        assert_eq!(selected.all_operations, [8001, 9001]);
        assert!(selected.rectangular);
        let mut f = authored_joined_side();
        f.0.get_mut(&181).unwrap().2[47] = b'6';
        let d = f.design();
        let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let selected = face(&Records::new(&d), &classes, 180, 19).unwrap();
        assert_eq!(selected.side_roles, BTreeMap::from([(8001, 5), (9001, 6)]), "each producer retains its own explicit curve label");
    }

    #[test]
    fn joined_side_can_have_caps_from_different_producers_with_the_same_orientation() {
        let mut f = authored_joined_side();
        let bytes = &mut f.0.get_mut(&181).unwrap().2;
        let mut r = Reader { bytes, at: 18 };
        let mut labels = Vec::new();
        for term in 0..5 {
            for _ in 0..r.u32().unwrap() {
                assert_eq!(r.u32().unwrap(), 1);
                if term == 1 {
                    labels.push(r.at);
                }
                r.take(1).unwrap();
                r.u32().unwrap();
                assert_eq!(r.u32().unwrap(), 1);
                r.u64().unwrap();
            }
        }
        for at in &labels {
            bytes[*at] = b'2';
        }
        let d = Fixture(f.0.clone()).design();
        let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        assert!(face(&Records::new(&d), &classes, 180, 19).is_ok());
        for at in labels {
            f.0.get_mut(&181).unwrap().2[at] = b'8';
        }
        assert!(decode(f, false).is_err(), "a third side neighbor cannot replace a cap boundary");
    }

    #[test]
    fn composite_side_recipe_rejects_ambiguous_roles_operations_and_incidence() {
        for keep in 0..authored_joined_side().0[&181].2.len() {
            let mut f = authored_joined_side();
            f.0.get_mut(&181).unwrap().2.truncate(keep);
            assert!(decode(f, false).is_err(), "prefix {keep}");
        }
        for (at, bytes) in [(18, 65_u32.to_le_bytes().to_vec()), (56, 8001_u64.to_le_bytes().to_vec())] {
            let mut f = authored_joined_side();
            f.0.get_mut(&181).unwrap().2[at..at + bytes.len()].copy_from_slice(&bytes);
            assert!(decode(f, false).is_err(), "offset {at}");
        }
        let mut f = authored_joined_side();
        *f.0.get_mut(&181).unwrap().2.last_mut().unwrap() = 0;
        assert!(decode(f, false).is_err());
    }

    #[test]
    fn rectangular_cap_incidence_and_target_markers_are_not_circular_aliases() {
        for high in [false, true] {
            let d = authored_rectangle(high).design();
            let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
            let records = Records::new(&d);
            let selected = face(&records, &classes, 180, 19).unwrap();
            assert!(selected.rectangular);
            assert_eq!((selected.feature, selected.contributions[0], selected.high), (9, 8001, high));
            assert_eq!(
                target(&records, &classes, 192, 19).unwrap(),
                Target { feature: 9, operations: vec![TargetOperation { operation: 8001, label: 7, aliases: vec![], flags: 33 }] }
            );
            for keep in 0..authored_rectangle(high).0[&181].2.len() {
                let mut f = authored_rectangle(high);
                f.0.get_mut(&181).unwrap().2.truncate(keep);
                assert!(decode(f, false).is_err(), "{high}/{keep}");
            }
            let mut f = authored_rectangle(high);
            let bytes = &mut f.0.get_mut(&181).unwrap().2;
            let end = bytes.len();
            bytes[end - 1] ^= 1;
            assert!(decode(f, false).is_err());
            let mut f = authored_rectangle(high);
            f.0.get_mut(&191).unwrap().2[30..34].copy_from_slice(&0_u32.to_le_bytes());
            assert!(decode(f, true).is_err());
        }
    }

    #[test]
    fn rectangular_cap_profile_requires_owned_whole_connected_orthogonal_sides() {
        let design = super::super::polygon_tests::rectangle().design();
        let mut profiles = design.sketch_profiles().unwrap();
        let profile = &mut profiles[0];
        let mut lines: BTreeMap<_, _> = design.sketch_lines().unwrap().into_iter().map(|l| (l.id, l)).collect();
        let face = Face::cap(9, 8001, true, true);
        face.validate_profile(profile, &BTreeMap::new(), &lines).unwrap();
        profile.regions[0][0].curves.reverse();
        face.validate_profile(profile, &BTreeMap::new(), &lines).unwrap();
        profile.regions[0][0].pieces[0] = (1, 2);
        assert!(face.validate_profile(profile, &BTreeMap::new(), &lines).is_err());
        profile.regions[0][0].pieces[0] = (1, 1);
        let line = *lines.keys().next().unwrap();
        lines.get_mut(&line).unwrap().sketch += 1;
        assert!(face.validate_profile(profile, &BTreeMap::new(), &lines).is_err());
        lines.get_mut(&line).unwrap().sketch -= 1;
        lines.get_mut(&line).unwrap().end[1] += 0.5;
        assert!(face.validate_profile(profile, &BTreeMap::new(), &lines).is_err());
    }

    fn decode(f: Fixture, body: bool) -> Result<(u64, u64, bool)> {
        let d = f.design();
        decode_design(&d, body)
    }

    fn decode_design(d: &Design, body: bool) -> Result<(u64, u64, bool)> {
        let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let records = Records::new(d);
        if body {
            let target = target(&records, &classes, 192, 19)?;
            Ok((target.feature, target.operations[0].operation, false))
        } else {
            let face = face(&records, &classes, 180, 19)?;
            Ok((face.feature, face.contributions[0], face.high))
        }
    }

    #[test]
    fn authored_cap_and_join_target_preserve_producer_operation_and_side() {
        for high in [false, true] {
            assert_eq!(decode(authored(high), false).unwrap(), (9, 8001, high));
            assert_eq!(decode(authored(high), true).unwrap(), (9, 8001, false));
        }
    }

    fn compound_target(operations: &[(u64, u32, u32)]) -> Fixture {
        let mut f = authored(true);
        let suffix = f.0[&190].2[33..].to_vec();
        let mut selector = vec![0; 6];
        u32(&mut selector, operations.len() as u32);
        for (operation, flags, _) in operations {
            u64(&mut selector, *operation);
            u32(&mut selector, *flags);
        }
        reference(&mut selector, 191);
        selector.extend(suffix);
        f.0.get_mut(&190).unwrap().2 = selector;
        let mut recipe = vec![0; 6];
        for n in [1, 3, 1, operations.len() as u32] {
            u32(&mut recipe, n);
        }
        for (operation, _, marker) in operations {
            string(&mut recipe, &operation.to_string());
            u32(&mut recipe, *marker);
            recipe.extend([0; 8]);
        }
        u32(&mut recipe, 0);
        string(&mut recipe, "body_recipe_data");
        u32(&mut recipe, 0);
        u32(&mut recipe, u32::MAX);
        f.0.get_mut(&191).unwrap().2 = recipe;
        f
    }

    #[test]
    fn compound_body_target_retains_each_operation_and_its_explicit_label() {
        let d = compound_target(&[(8001, 33, 7), (9013, 4, 11)]).design();
        let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        assert_eq!(
            target(&Records::new(&d), &classes, 192, 19).unwrap(),
            Target {
                feature: 9,
                operations: vec![
                    TargetOperation { operation: 8001, label: 7, aliases: vec![], flags: 33 },
                    TargetOperation { operation: 9013, label: 11, aliases: vec![], flags: 4 }
                ]
            }
        );
        for operations in [
            vec![],
            vec![(8001, 33, 7), (8001, 4, 7)],
            vec![(0, 33, 4)],
            vec![(8001, 4, 7)],
            vec![(8001, 33, 7), (9001, 33, 7)],
            vec![(8001, 33, 0)],
            vec![(8001, 33, 2)],
        ] {
            assert!(decode(compound_target(&operations), true).is_err(), "{operations:?}");
        }
        let operations: Vec<_> = (0..65).map(|i| (8001 + i, if i == 0 { 33 } else { 4 }, 7)).collect();
        assert!(decode(compound_target(&operations), true).is_err());
        for id in [190, 191] {
            let f = compound_target(&[(8001, 33, 7), (9013, 4, 11)]);
            for keep in 0..f.0[&id].2.len() {
                let mut truncated = Fixture(f.0.clone());
                truncated.0.get_mut(&id).unwrap().2.truncate(keep);
                assert!(decode(truncated, true).is_err(), "{id}/{keep}");
            }
        }
        let mut f = compound_target(&[(8001, 33, 7), (9013, 4, 11)]);
        f.0.get_mut(&191).unwrap().2[26] = b'9';
        assert!(decode(f, true).is_err(), "recipe and selector must name the same operations");
    }

    #[test]
    fn serial_loft_targets_keep_one_current_owner_and_prior_loft_roles() {
        for operations in [vec![(8001, 32, 7), (9001, 129, 61)], vec![(8001, 32, 7), (9001, 4, 11), (9101, 128, 61), (9201, 129, 208)]] {
            let d = compound_target(&operations).design();
            let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
            let target = target(&Records::new(&d), &classes, 192, 19).unwrap();
            assert_eq!(target.feature, 9);
            assert_eq!(target.operations.iter().map(|o| (o.operation, o.flags, o.label)).collect::<Vec<_>>(), operations);
        }
        for operations in [
            vec![(8001, 32, 7)],
            vec![(8001, 33, 7), (9001, 129, 61)],
            vec![(8001, 32, 7), (9001, 128, 61)],
            vec![(8001, 32, 7), (9001, 129, 61), (9101, 128, 208)],
            vec![(8001, 32, 7), (9001, 129, 61), (9101, 129, 208)],
            vec![(8001, 32, 7), (9001, 130, 61)],
        ] {
            assert!(decode(compound_target(&operations), true).is_err(), "{operations:?}");
        }
    }

    fn legacy(mut f: Fixture) -> Design {
        f.0.get_mut(&180).unwrap().1 = 2;
        f.0.get_mut(&180).unwrap().2.remove(0);
        f.0.get_mut(&81).unwrap().1 = 18;
        for id in [180, 190] {
            let bytes = &mut f.0.get_mut(&id).unwrap().2;
            bytes.remove(bytes.len() - 25);
        }
        let d = Fixture(f.0.clone()).design();
        let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let mut r = Reader { bytes: &f.0[&9].2, at: 0 };
        super::super::history::extrude_inputs(&mut r, &classes).unwrap();
        references(&mut r).unwrap(); // profile; legacy omits the first body role
        let at = r.at;
        assert_eq!(r.u32().unwrap(), 0);
        f.0.get_mut(&9).unwrap().2.drain(at..at + 4);
        let mut d = f.design();
        for id in ["A0A15D26-1F3B-4120-A3F1-9CDDA189AB74", "2EDC2536-D95B-4469-8ACA-DC07124D9C43"] {
            d.classes.push(Class { id: id.into(), parent: String::new(), version: 1, group: String::new(), objects: Vec::new() });
        }
        d
    }

    #[test]
    fn legacy_selector_and_inherited_roles_preserve_face_and_target_ownership() {
        for high in [false, true] {
            let d = legacy(authored(high));
            assert_eq!(decode_design(&d, false).unwrap(), (9, 8001, high));
            assert_eq!(decode_design(&d, true).unwrap(), (9, 8001, false));
        }
        for id in [super::super::body_projections::SELECTOR, "A0A15D26-1F3B-4120-A3F1-9CDDA189AB74", "2EDC2536-D95B-4469-8ACA-DC07124D9C43"] {
            for version in [0, 4] {
                let mut d = legacy(authored(true));
                d.classes.iter_mut().find(|c| c.id == id).unwrap().version = version;
                assert!(decode_design(&d, false).is_err(), "{id}/{version}");
            }
        }
        // A newer header must not silently consume the legacy record's owner.
        let mut d = legacy(authored(true));
        d.classes.iter_mut().find(|c| c.id == "A0A15D26-1F3B-4120-A3F1-9CDDA189AB74").unwrap().version = 2;
        assert!(decode_design(&d, false).is_err());
        assert!(decode_design(&d, true).is_err());
    }

    #[test]
    #[ignore = "requires local original adapter and unchanged public API resave"]
    fn original_adapter_loft_face_selectors_match_public_api_resave() {
        let corpus = std::path::PathBuf::from(solidcraft_geom::env::var_os("F3D_CORPUS").unwrap());
        let probes = std::path::PathBuf::from(solidcraft_geom::env::var_os("F3D_PROBES").unwrap());
        let mut results = Vec::new();
        for path in [corpus.join("adapter-x20-makita.f3d"), probes.join("corpus-adapter-x20-makita.f3d")] {
            let designs = read_f3d(&std::fs::read(&path).unwrap()).unwrap();
            let d = &designs[0];
            let records = Records::new(d);
            let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
            let mut faces = Vec::new();
            for entry in d.timeline().unwrap() {
                if entry.class != super::super::lofts::CLASS {
                    continue;
                }
                let feature = entry.feature.unwrap();
                let HistoryKind::Loft { section_containers, .. } = feature.kind else { panic!("wrong loft kind") };
                for container in section_containers {
                    for object in objects(&records, &classes, container, feature.id).unwrap() {
                        if selector_version(&classes, object).is_err() {
                            continue;
                        }
                        let selected = face(&records, &classes, object, feature.id).unwrap_or_else(|e| panic!("{} / {object}: {e}", path.display()));
                        faces.push((feature.id, selected.feature, selected.contributions, selected.all_operations, selected.side_roles));
                    }
                }
            }
            assert_eq!(faces.len(), 4);
            results.push((faces, d.operation_owners().unwrap()));
        }
        assert_eq!(results[0], results[1]);
    }

    #[test]
    fn malformed_face_and_target_identities_fail_without_geometry_fallback() {
        for (body, ids) in [(false, &[80, 180, 181][..]), (true, &[80, 190, 191, 192][..])] {
            for id in ids {
                for keep in 0..authored(true).0[id].2.len() {
                    let mut f = authored(true);
                    f.0.get_mut(id).unwrap().2.truncate(keep);
                    assert!(decode(f, body).is_err(), "{body}/{id}/{keep}");
                }
            }
        }
        for (body, id, offset) in [(false, 180, 0), (false, 181, 6), (true, 190, 10), (true, 191, 6)] {
            let mut f = authored(true);
            f.0.get_mut(&id).unwrap().2[offset] ^= 1;
            assert!(decode(f, body).is_err());
        }
    }
}
