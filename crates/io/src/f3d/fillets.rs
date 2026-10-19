//! Constant-radius edge sets and explicit edge incidence, observed independently.
use super::history::{empty, finish, payload, references, require_class, zeros};
use super::*;

pub(super) const CLASS: &str = "A07D5F17-68CB-464D-9935-BF68E98A865F";
const SET: &str = "4A557CE2-375C-4F69-AA76-2403109F1493";
const CONTAINER: &str = "2CA5A1CD-C99B-4C9B-91AF-57989148E841";

#[derive(Debug, PartialEq)]
pub struct FilletSet {
    pub container: Option<u64>,
    /// Faces are a separate native selection, not an expendable edge cache.
    pub face_container: Option<u64>,
    pub radius: u64,
    pub weight: u64,
    pub tangent_chain: bool,
}

pub(super) fn inputs(r: &mut Reader<'_>, records: &Records<'_>, classes: &BTreeMap<u64, &Class>, version: u32) -> Result<Vec<FilletSet>> {
    empty(r)?;
    zeros(r, 1)?;
    if !super::attributes::read(r)?.is_empty() {
        return Err(invalid("unsupported fillet attributes"));
    }
    let helpers = references(r)?;
    if !(1..=64).contains(&helpers.len()) || helpers.iter().collect::<BTreeSet<_>>().len() != helpers.len() {
        return Err(invalid("invalid fillet edge-set count"));
    }
    zeros(r, 4)?;
    if r.take(1)? != [1] {
        return Err(invalid("unsupported fillet feature mode"));
    }
    if version == 3 {
        zeros(r, 1)?;
    }
    if r.take(1)? != [1] {
        return Err(invalid("unsupported fillet corner mode"));
    }
    zeros(r, 9)?;
    let containers = references(r)?;
    let mut sets = Vec::new();
    for (index, helper) in helpers.into_iter().enumerate() {
        let version = classes
            .get(&helper)
            .filter(|c| c.id == SET && matches!(c.version, 9 | 10))
            .map(|c| c.version)
            .ok_or_else(|| invalid("unsupported fillet edge-set schema"))?;
        let mut h = payload(records.primary(helper)?, helper)?;
        zeros(&mut h, if version == 10 { 8 } else { 7 })?;
        if h.take(2)? != [1, 0] {
            return Err(invalid("unsupported fillet radius mode"));
        }
        if version == 10 {
            zeros(&mut h, 1)?;
        }
        let tangent_chain = match h.take(1)? {
            [0] => false,
            [1] => true,
            _ => return Err(invalid("invalid fillet tangent-chain flag")),
        };
        zeros(&mut h, 2)?;
        let container = optional_container(&mut h, classes)?;
        zeros(&mut h, if version == 10 { 19 } else { 17 })?;
        let face_container = optional_container(&mut h, classes)?;
        zeros(&mut h, 3)?;
        if container.is_none() && face_container.is_none() {
            return Err(invalid("fillet edge set has no edge or face selection"));
        }
        let radius = h.reference()?;
        zeros(&mut h, 4)?;
        if h.u32()? as usize != index {
            return Err(invalid("inconsistent fillet edge-set order"));
        }
        let weight = h.reference()?;
        for id in [radius, weight] {
            require_class(classes, id, "D91D429C-1BB5-4640-B49F-69AF14F463E9", 0)?;
        }
        zeros(&mut h, 4)?;
        finish(&h)?;
        sets.push(FilletSet { container, face_container, radius, weight, tangent_chain });
    }
    let selected = sets.iter().flat_map(|s| s.container.into_iter().chain(s.face_container)).collect::<Vec<_>>();
    if containers != selected || containers.iter().collect::<BTreeSet<_>>().len() != containers.len() {
        return Err(invalid("fillet feature and edge-set selections disagree"));
    }
    Ok(sets)
}

fn optional_container(r: &mut Reader<'_>, classes: &BTreeMap<u64, &Class>) -> Result<Option<u64>> {
    if r.bytes.get(r.at) == Some(&0) {
        r.take(1)?;
        return Ok(None);
    }
    let id = r.reference()?;
    require_class(classes, id, CONTAINER, 1)?;
    Ok(Some(id))
}

#[cfg(test)]
#[path = "fillet_face_tests.rs"]
mod face_tests;

#[derive(Debug, PartialEq)]
pub(super) struct Edge {
    pub feature: u64,
    pub operation: u64,
    pub faces: [u32; 2],
    pub labels: Vec<u32>,
    pub endpoint_cycles: [[u32; 3]; 2],
    pub closed: bool,
    pub primary_face: Option<u32>,
}

pub(super) fn edge(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<Edge> {
    edge_with_mode(records, classes, id, owner, false)
}

pub(super) fn edge_with_mode(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64, intersection: bool) -> Result<Edge> {
    let version = super::loft_faces::selector_version(classes, id)?;
    let mut r = payload(records.primary(id)?, id)?;
    zeros(&mut r, if version == 3 { 7 } else { 6 })?;
    if r.take(1)? != [u8::from(intersection)] {
        return Err(invalid("unsupported edge intersection-selection flag"));
    }
    let identity = r.reference()?;
    require_class(classes, identity, "7ACC2A03-0261-4879-A14A-A93D661A5BDC", 3)?;
    let mut recipe = payload(records.primary(identity)?, identity)?;
    let (operation, labels, endpoint_cycles, closed, primary_face) = match edge_recipe(&mut recipe, intersection) {
        Ok((operation, labels, endpoints)) => (operation, labels, endpoints, false, None),
        Err(error) if !intersection => {
            let (circular, primary) = super::body_projections::identity_with_primary(records, classes, identity, true)
                .map_err(|circle| invalid(&format!("{error}; circular blend: {circle}")))?;
            if circular.side.is_some() {
                return Err(error);
            }
            (circular.operation, vec![3, if circular.end { 2 } else { 1 }], [[0; 3]; 2], true, primary)
        }
        Err(error) => return Err(error),
    };
    if r.u32()? != 1 {
        return Err(invalid("unsupported fillet edge source count"));
    }
    if r.wide_string()?.is_empty() || r.wide_string()?.is_empty() {
        return Err(invalid("empty fillet edge source identity"));
    }
    if r.u32()? != 2 {
        return Err(invalid("unsupported fillet edge selection"));
    }
    super::history::selection_flag(&mut r)?;
    for cached in references(&mut r)? {
        // Fillet selections retain a point cache. Its coordinates never choose
        // the edge: the explicit recipe and producer handle do that below.
        require_class(classes, cached, if closed { "B0B3FA59-C374-444F-B424-24AA5DE41645" } else { "50AEE8B4-9456-4DD7-847D-6F6C909BBB9D" }, 0)?;
    }
    let handle = super::loft_faces::selector_tail(&mut r, records, owner)?;
    let feature = super::loft_faces::producer(records, classes, handle)?;
    let faces = [*labels.first().ok_or_else(|| invalid("missing fillet face"))?, *labels.get(1).ok_or_else(|| invalid("missing fillet face"))?];
    Ok(Edge { feature, operation, faces, labels, endpoint_cycles, closed, primary_face })
}

// Two incident faces and two endpoint faces. The ordered incidence branches
// validate the selected edge; face labels are resolved through the producer table.
type EdgeRecipe = (u64, Vec<u32>, [[u32; 3]; 2]);
fn edge_recipe(r: &mut Reader<'_>, intersection: bool) -> Result<EdgeRecipe> {
    zeros(r, 6)?;
    for n in [1, 3, 4] {
        if r.u32()? != n {
            return Err(invalid("unsupported fillet edge identity"));
        }
    }
    let mut operation = None;
    let mut labels = Vec::new();
    for _ in 0..4 {
        if r.u32()? != 1 {
            return Err(invalid("unsupported composite fillet edge term"));
        }
        let text = r.string()?;
        let label = text.parse::<u32>().ok().filter(|n| *n > 0 && n.to_string() == text).ok_or_else(|| invalid("invalid fillet face label"))?;
        if labels.contains(&label) {
            return Err(invalid("duplicate fillet face label"));
        }
        labels.push(label);
        zeros(r, 4)?;
        if r.u32()? != 1 {
            return Err(invalid("unsupported fillet edge operation count"));
        }
        let tag = r.u64()?;
        if tag == 0 || operation.is_some_and(|operation| operation != tag) {
            return Err(invalid("fillet edge has inconsistent operation ownership"));
        }
        operation = Some(tag);
    }
    zeros(r, 4)?;
    if r.string()? != "edge_recipe_data" || r.u32()? != u32::MAX {
        return Err(invalid("unsupported fillet edge recipe"));
    }
    for n in [u32::MAX, 2, 0, u32::MAX, 1, u32::MAX, 2] {
        if r.u32()? != n {
            return Err(invalid("invalid fillet edge face incidence"));
        }
    }
    let mut endpoint_cycles = [[0; 3]; 2];
    for (end, cycle) in [2, 3].into_iter().zip(&mut endpoint_cycles) {
        if r.u32()? != u32::MAX || r.u32()? != 3 {
            return Err(invalid("invalid fillet edge endpoint"));
        }
        let mut terms = BTreeSet::new();
        for term in cycle {
            *term = r.u32()?;
            if !terms.insert(*term) || r.u32()? != u32::MAX {
                return Err(invalid("invalid fillet edge endpoint incidence"));
            }
        }
        if terms != BTreeSet::from([0, 1, end]) {
            return Err(invalid("fillet edge endpoints disagree with its faces"));
        }
        zeros(r, 8)?;
    }
    if r.u32()? != if intersection { 0 } else { u32::MAX } {
        return Err(invalid("unsupported fillet edge tracking"));
    }
    finish(r)?;
    Ok((operation.ok_or_else(|| invalid("missing fillet operation"))?, labels, endpoint_cycles))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::f3d::import_tests::{Fixture, common, fixture};
    use crate::f3d::relationship_tests::{reference, refs, string, u32, u64, wide};

    fn check_circular_blend(doc: &solidcraft_doc::Document, mode: &str, cap: &str, radius: f64, height: f64, size: f64, suppressed: bool) {
        let pi = std::f64::consts::PI;
        let stock = pi * radius * radius * height;
        let removed = if suppressed {
            0.
        } else if mode == "chamfer" {
            pi * size * size * (radius - size / 3.)
        } else {
            pi * (2. * radius * size * size * (1. - pi / 4.) - size * size * size * (5. / 3. - pi / 2.))
        };
        let bottom_moment = if suppressed {
            0.
        } else {
            let m = pi * (radius * size.powi(3) / 3. - size.powi(4) / 12.);
            if mode == "chamfer" { m } else { size * removed - m }
        };
        let moment = if cap == "top" { height * removed - bottom_moment } else { bottom_moment };
        let want = stock - removed;
        let center = solidcraft_geom::Vec3::new(0., 0., (stock * height / 2. - moment) / want);
        let mut model = solidcraft_doc::Model::new();
        model.evaluate(doc);
        assert!(model.results.iter().all(|r| r.error.is_none()));
        let state = model.state();
        assert_eq!(state.bodies.len(), 1);
        let measured = solidcraft_kernel::measure(&state.bodies[0].body).unwrap();
        assert!((measured.volume - want).abs() < stock * 2e-4 + removed * 0.015 + 0.1, "{} != {want}", measured.volume);
        assert!(measured.centroid.dist(center) < 0.005, "{:?} != {center:?}", measured.centroid);
    }

    #[test]
    #[ignore = "requires local controlled circular blend archives"]
    fn native_circular_blends_preserve_caps_parameters_and_reopening() {
        let dir = std::path::PathBuf::from(solidcraft_geom::env::var_os("F3D_PROBES").unwrap());
        for mode in ["chamfer", "fillet"] {
            for cap in ["top", "bottom"] {
                for (suffix, radius, height, size, suppressed) in [
                    ("baseline", 20., 40., 2., false),
                    ("size4", 20., 40., 4., false),
                    ("diameter60", 30., 40., 2., false),
                    ("height50", 20., 50., 2., false),
                    ("suppressed", 20., 40., 2., true),
                    ("restored", 20., 40., 2., false),
                ] {
                    let name = format!("circular-{mode}-{cap}-{suffix}");
                    let native = read_f3d(&std::fs::read(dir.join(format!("{name}.f3d"))).unwrap()).unwrap();
                    let doc = native[0].to_document(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
                    check_circular_blend(&doc, mode, cap, radius, height, size, suppressed);
                    let reopened = crate::read_design(&crate::write_design_container(&doc).unwrap()).unwrap();
                    assert_eq!(doc, reopened);
                    check_circular_blend(&reopened, mode, cap, radius, height, size, suppressed);
                    if suffix == "baseline" {
                        for (parameter, value, radius, height, size) in [
                            ("d1", "60 mm", 30., 40., 2.),
                            ("d2", "50 mm", 20., 50., 2.),
                            (if mode == "chamfer" { "d4" } else { "d5" }, "4 mm", 20., 40., 4.),
                        ] {
                            let mut edited = doc.clone();
                            edited.change_param(parameter, value, None, None).unwrap();
                            check_circular_blend(&edited, mode, cap, radius, height, size, false);
                            let reopened = crate::read_design(&crate::write_design_container(&edited).unwrap()).unwrap();
                            assert_eq!(edited, reopened);
                            check_circular_blend(&reopened, mode, cap, radius, height, size, false);
                        }
                    }
                }
            }
        }
    }

    fn circular_fixture(end: bool) -> Fixture {
        let mut f = authored_edge();
        let extra = authored(3, false, false);
        for id in [5, 19, 900, 901, 902, 903] {
            f.0.insert(id, extra.0[&id].clone());
        }
        // Normalize the independent fixture's secondary profile identity.
        let circle = &mut f.0.get_mut(&31).unwrap().2;
        let mut r = Reader { bytes: circle, at: 6 };
        let mut secondary = None;
        for _ in 0..r.u32().unwrap() {
            let key = r.string().unwrap();
            r.string().unwrap();
            if key == "crv_secondary_id" {
                secondary = Some(r.at);
            }
            r.u64().unwrap();
        }
        let at = secondary.unwrap();
        circle[at..at + 8].fill(0);
        let mut b = vec![0; 6];
        reference(&mut b, 100);
        for n in [1, 1, 1, 3] {
            u32(&mut b, n);
        }
        u64(&mut b, 17);
        u64(&mut b, 0);
        for n in [0, 1, 1] {
            u32(&mut b, n);
        }
        u64(&mut b, 0);
        b.push(1);
        u32(&mut b, 0);
        f.0.get_mut(&101).unwrap().2 = b;
        let feature = &mut f.0.get_mut(&9).unwrap().2;
        let at = feature.len() - 59;
        let mut slot = Vec::new();
        reference(&mut slot, 600);
        feature[at..at + 11].copy_from_slice(&slot);
        let mut b = vec![0; 6];
        u32(&mut b, 1);
        reference(&mut b, 601);
        u64(&mut b, 3);
        u64(&mut b, 4);
        f.put(600, "B7F34D7B-DAC4-41A5-B67D-D672537C3BFA", 2, b, Some(vec![0; 5]));
        let mut b = vec![0; 5];
        u32(&mut b, 1);
        b.push(0);
        reference(&mut b, 602);
        b.push(0);
        f.put(601, "335CC7BD-BBE5-40AA-9E8C-5EC99D427A3A", 0, b, None);
        let mut b = vec![0; 6];
        for n in [1, 0, 1, 3] {
            u32(&mut b, n);
        }
        u64(&mut b, 17);
        u64(&mut b, 0);
        for n in [1, 1, 1, 0, 0, 0, 0] {
            u32(&mut b, n);
        }
        f.put(602, "2DEE34BE-FA70-4D54-979C-164D86FDA786", 2, b, None);
        let mut b = vec![0; 6];
        for n in [1, 3, 3] {
            u32(&mut b, n);
        }
        for label in if end { ["-1", "3", "2"] } else { ["-102", "3", "1"] } {
            u32(&mut b, 1);
            string(&mut b, label);
            u32(&mut b, 0);
            u32(&mut b, 1);
            u64(&mut b, 8001);
        }
        u32(&mut b, 0);
        string(&mut b, "edge_recipe_data");
        u32(&mut b, 0);
        for n in [-1_i32, 2, 1, -1, 2, -1, 1, -1, 2, 1, -1, 2, -1, 1, 0, -1, 0, 0, -1, 1, 0].into_iter().chain(if end {
            [2, 2, 2, 2, 1, 0, 1, -1]
        } else {
            [2, 1, 1, 1, 2, 1, 2, -1]
        }) {
            u32(&mut b, n as u32);
        }
        f.0.get_mut(&302).unwrap().2 = b;
        f.0.get_mut(&303).unwrap().0 = "B0B3FA59-C374-444F-B424-24AA5DE41645".into();
        let mut b = vec![0; 6];
        refs(&mut b, &[301]);
        reference(&mut b, 19);
        f.0.get_mut(&901).unwrap().2 = b;
        for (id, binding, name, label, unit, expression, value) in
            [(904, 902, "fillet_radius", "Radius", "mm", "2 mm", 0.2_f64), (905, 903, "fillet_weight", "TangencyWeight", "", "1", 1.)]
        {
            let mut b = vec![0; 20];
            u32(&mut b, 0);
            reference(&mut b, binding);
            wide(&mut b, expression);
            b.extend([0; 10]);
            for text in [label, "", unit, name] {
                wide(&mut b, text);
            }
            b.extend(value.to_le_bytes());
            b.extend([0; 12]);
            f.put(id, "7A6A3D31-BE74-4E19-9115-20642944C2E4", 9, b, None);
            let mut b = vec![0; 4];
            b.push(1);
            refs(&mut b, &[19]);
            b.extend([0; 5]);
            b.extend(value.to_le_bytes());
            reference(&mut b, id);
            f.0.get_mut(&binding).unwrap().2 = b;
        }
        let mut b = vec![0; 6];
        u32(&mut b, 1);
        u64(&mut b, 9);
        u32(&mut b, 1);
        u64(&mut b, 8001);
        u32(&mut b, 1);
        u64(&mut b, 8001);
        u64(&mut b, 9);
        u32(&mut b, 0);
        reference(&mut b, 4);
        f.put(180, "5D89B935-3F04-4CAB-81F5-E7B309E7E13F", 1, b, None);
        f
    }

    #[test]
    fn authored_circular_chamfer_preserves_primary_face_and_independent_dimensions() {
        for end in [false, true] {
            for flip in [false, true] {
                let mut f = circular_fixture(end);
                let mut b = vec![0; 6];
                u32(&mut b, 1);
                b.extend([0, 1]);
                u32(&mut b, 1);
                u64(&mut b, 71);
                b.extend([1, 0]);
                refs(&mut b, &[900]);
                refs(&mut b, &[901]);
                common(&mut b, "Circular chamfer", false);
                f.put(19, super::super::chamfers::CLASS, 3, b, None);
                let mut b = vec![0; 6];
                b.extend([0, u8::from(flip), 0]);
                reference(&mut b, 902);
                reference(&mut b, 903);
                u32(&mut b, 2);
                reference(&mut b, 901);
                b.extend([0; 2]);
                u32(&mut b, 1);
                b.extend([1, 0, 0]);
                reference(&mut b, 906);
                b.push(0);
                reference(&mut b, 19);
                f.put(900, "0D383DB2-A6E5-48C7-82D5-ED3DD0C275C5", 2, b, None);
                f.put(906, "6CCF41D5-40BE-48ED-A834-18F3EAED6C57", 1, vec![], None);
                for (id, binding, name, label, expr, value) in
                    [(904, 902, "fillet_radius", "Distance 1", "2 mm", 0.2f64), (905, 903, "second", "Distance 2", "3 mm", 0.3f64)]
                {
                    let mut b = vec![0; 20];
                    u32(&mut b, 0);
                    reference(&mut b, binding);
                    wide(&mut b, expr);
                    b.extend([0; 10]);
                    for text in [label, "", "mm", name] {
                        wide(&mut b, text);
                    }
                    b.extend(value.to_le_bytes());
                    b.extend([0; 12]);
                    f.put(id, "7A6A3D31-BE74-4E19-9115-20642944C2E4", 9, b, None);
                    let mut b = vec![0; 4];
                    b.push(1);
                    refs(&mut b, &[19]);
                    b.extend([0; 5]);
                    b.extend(value.to_le_bytes());
                    reference(&mut b, id);
                    f.0.get_mut(&binding).unwrap().2 = b;
                }
                let mut doc = f.design().to_document("Original asymmetric circular chamfer").unwrap();
                for (first, second) in [(2., 3.), (4., 5.)] {
                    doc.change_param("d7", "60 mm", None, None).unwrap();
                    doc.change_param("height", "17 mm", None, None).unwrap();
                    doc.change_param("fillet_radius", &format!("{first} mm"), None, None).unwrap();
                    doc.change_param("second", &format!("{second} mm"), None, None).unwrap();
                    let mut model = solidcraft_doc::Model::default();
                    model.evaluate(&doc);
                    assert!(model.results.iter().all(|r| r.error.is_none()), "{:?}", model.results);
                    let state = model.state();
                    let body = &state.bodies[0];
                    let radial = if !end ^ flip { second } else { first };
                    let axial = if !end ^ flip { first } else { second };
                    let expected = std::f64::consts::PI * (30. * 30. * 17. - radial * axial * (30. - radial / 3.));
                    assert!((solidcraft_kernel::measure(&body.body).unwrap().volume - expected).abs() < expected * 2e-4);
                    let z = if end { 17. } else { 0. };
                    let cap = body.mesh().edges.iter().flatten().filter(|p| (p.z - z).abs() < 1e-7).map(|p| p.x.hypot(p.y)).fold(0., f64::max);
                    assert!((cap - (30. - radial)).abs() < 0.001);
                    let reopened = crate::read_design(&crate::write_design_container(&doc).unwrap()).unwrap();
                    assert_eq!(doc, reopened);
                    crate::f3d::import_tests::volume(&reopened, expected);
                }
            }
        }
    }

    #[test]
    fn circular_fillet_retains_source_radius_height_and_cap_identity() {
        let pi = std::f64::consts::PI;
        for end in [false, true] {
            let mut doc = circular_fixture(end).design().to_document("Original circular fillet").unwrap();
            for (diameter, height, size) in [(60., 17., 2.), (80., 23., 4.)] {
                doc.change_param("d7", &format!("{diameter} mm"), None, None).unwrap();
                doc.change_param("height", &format!("{height} mm"), None, None).unwrap();
                doc.change_param("fillet_radius", &format!("{size} mm"), None, None).unwrap();
                let radius = diameter / 2.;
                let expected =
                    pi * (radius * radius * height - 2. * radius * size * size * (1. - pi / 4.) + size * size * size * (5. / 3. - pi / 2.));
                crate::f3d::import_tests::volume(&doc, expected);
                let cap = format!("F{}:{}", doc.features[1].id, if end { "top" } else { "start" });
                assert!(doc.features[2].edge_names.iter().all(|name| name.split('|').any(|n| n == cap)));
                let reopened = crate::read_design(&crate::write_design_container(&doc).unwrap()).unwrap();
                assert_eq!(doc, reopened);
                crate::f3d::import_tests::volume(&reopened, expected);
            }
        }
    }

    #[test]
    fn circular_fillet_rejects_unowned_or_partial_source_boundaries() {
        let mut partial = circular_fixture(true);
        partial.0.get_mut(&101).unwrap().2[57..61].copy_from_slice(&2_u32.to_le_bytes());
        assert!(partial.design().to_document("Partial source circle").is_err());
        for (id, offset) in [(302, 22), (602, 22), (180, 22)] {
            let mut f = circular_fixture(true);
            f.0.get_mut(&id).unwrap().2[offset] ^= 1;
            assert!(f.design().to_document("Invalid circular fillet").is_err());
        }
        for id in [301, 302, 600, 601, 602] {
            let f = circular_fixture(true);
            for end in 0..f.0[&id].2.len() {
                let mut copy = Fixture(f.0.clone());
                copy.0.get_mut(&id).unwrap().2.truncate(end);
                assert!(copy.design().to_document("Truncated circular fillet").is_err(), "{id}/{end}");
            }
        }
    }

    pub(super) fn authored(version: u32, tangent: bool, suppressed: bool) -> Fixture {
        let mut f = fixture(false);
        let mut timeline = vec![0; 6];
        reference(&mut timeline, 4);
        refs(&mut timeline, &[8, 9, 19]);
        f.0.get_mut(&5).unwrap().2 = timeline;
        let mut feature = vec![0; 6];
        refs(&mut feature, &[900]);
        feature.extend([0; 4]);
        feature.push(1);
        if version == 3 {
            feature.push(0);
        }
        feature.push(1);
        feature.extend([0; 9]);
        refs(&mut feature, &[901]);
        common(&mut feature, "Fillet", suppressed);
        f.put(19, CLASS, version, feature, None);
        let modern = version == 3;
        let mut set = vec![0; if modern { 8 } else { 7 }];
        set.extend([1, 0]);
        if modern {
            set.push(0);
        }
        set.push(u8::from(tangent));
        set.extend([0; 2]);
        reference(&mut set, 901);
        set.extend(vec![0; if modern { 23 } else { 21 }]);
        reference(&mut set, 902);
        set.extend([0; 8]);
        reference(&mut set, 903);
        set.extend([0; 4]);
        f.put(900, SET, if modern { 10 } else { 9 }, set, None);
        f.put(901, CONTAINER, 1, vec![], None);
        for id in [902, 903] {
            f.put(id, "D91D429C-1BB5-4640-B49F-69AF14F463E9", 0, vec![], None);
        }
        f
    }

    #[test]
    fn fillet_sets_preserve_versions_bindings_chain_and_suppression() {
        for version in [2, 3] {
            for tangent in [false, true] {
                for suppressed in [false, true] {
                    let history = authored(version, tangent, suppressed).design().history().unwrap();
                    let feature = history.last().unwrap();
                    assert_eq!(feature.suppressed, suppressed);
                    assert_eq!(
                        feature.kind,
                        HistoryKind::Fillet {
                            sets: vec![FilletSet { container: Some(901), face_container: None, radius: 902, weight: 903, tangent_chain: tangent }]
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn malformed_fillet_sets_never_drop_modes_or_bindings() {
        for version in [2, 3] {
            for id in [19, 900] {
                for keep in 0..authored(version, false, false).0[&id].2.len() {
                    let mut f = authored(version, false, false);
                    f.0.get_mut(&id).unwrap().2.truncate(keep);
                    assert!(f.design().history().is_err(), "{version}/{id}/{keep}");
                }
            }
            for offset in if version == 3 { [8, 11, 63] } else { [7, 9, 59] } {
                let mut f = authored(version, false, false);
                f.0.get_mut(&900).unwrap().2[offset] = 2;
                assert!(f.design().history().is_err());
            }
            let mut f = authored(version, false, false);
            f.0.get_mut(&902).unwrap().0 = CONTAINER.into();
            assert!(f.design().history().is_err());
        }
    }

    fn recipe() -> Vec<u8> {
        let mut b = vec![0; 6];
        for n in [1, 3, 4] {
            u32(&mut b, n);
        }
        for label in ["19", "42", "1", "2"] {
            u32(&mut b, 1);
            string(&mut b, label);
            u32(&mut b, 0);
            u32(&mut b, 1);
            u64(&mut b, 8001);
        }
        u32(&mut b, 0);
        string(&mut b, "edge_recipe_data");
        u32(&mut b, u32::MAX);
        for n in [-1_i32, 2, 0, -1, 1, -1, 2, -1, 3, 1, -1, 0, -1, 2, -1, 0, 0, -1, 3, 1, -1, 3, -1, 0, -1, 0, 0, -1] {
            u32(&mut b, n as u32);
        }
        b
    }

    #[test]
    fn edge_incidence_accepts_explicit_nonordinal_labels_and_rejects_wrong_ownership() {
        let parse = |b: &[u8]| edge_recipe(&mut Reader { bytes: b, at: 0 }, false);
        let b = recipe();
        assert_eq!(parse(&b).unwrap(), (8001, vec![19, 42, 1, 2], [[1, 0, 2], [1, 3, 0]]));
        for keep in 0..b.len() {
            assert!(parse(&b[..keep]).is_err(), "{keep}");
        }
        let mut wrong = recipe();
        let at = wrong.windows(8).rposition(|b| b == 8001_u64.to_le_bytes()).unwrap();
        wrong[at..at + 8].copy_from_slice(&8002_u64.to_le_bytes());
        assert!(parse(&wrong).is_err());
        let mut wrong = recipe();
        let at = wrong.len() - 9 * 4;
        wrong[at..at + 4].copy_from_slice(&2_u32.to_le_bytes());
        assert!(parse(&wrong).is_err());
    }

    fn authored_edge() -> Fixture {
        let mut f = fixture(false);
        let mut outputs = Vec::new();
        refs(&mut outputs, &[80]);
        f.0.get_mut(&9).unwrap().2.splice(151..155, outputs);
        let mut h = vec![0; 6];
        wide(&mut h, "original-body");
        h.extend([0, 1]);
        reference(&mut h, 81);
        reference(&mut h, 9);
        let mut secondary = vec![0; 4];
        reference(&mut secondary, 4);
        f.put(80, "D26351F0-5940-4D23-AA20-2C35475A6D9E", 2, h, Some(secondary));
        f.put(81, "D3937028-C20C-4E65-B010-94AD418A5C20", 19, vec![], None);
        let mut s = vec![0; 8];
        reference(&mut s, 302);
        u32(&mut s, 1);
        wide(&mut s, "original-selected-body");
        wide(&mut s, "original-selected-owner");
        u32(&mut s, 2);
        u32(&mut s, 0);
        refs(&mut s, &[303]);
        refs(&mut s, &[80]);
        s.extend([0; 56]);
        reference(&mut s, 19);
        f.put(301, super::super::body_projections::SELECTOR, 3, s, None);
        f.put(302, "7ACC2A03-0261-4879-A14A-A93D661A5BDC", 3, recipe(), None);
        f.put(303, "50AEE8B4-9456-4DD7-847D-6F6C909BBB9D", 0, vec![], None);
        f
    }

    #[test]
    fn selected_fillet_edge_uses_its_recipe_and_owned_producer_not_cache_coordinates() {
        let resolve = |f: Fixture, owner| {
            let d = f.design();
            let classes = d.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
            edge(&Records::new(&d), &classes, 301, owner)
        };
        assert_eq!(
            resolve(authored_edge(), 19).unwrap(),
            Edge {
                feature: 9,
                operation: 8001,
                faces: [19, 42],
                labels: vec![19, 42, 1, 2],
                endpoint_cycles: [[1, 0, 2], [1, 3, 0]],
                closed: false,
                primary_face: None
            }
        );
        assert!(resolve(authored_edge(), 9).is_err());
        for id in [80, 301, 302] {
            for keep in 0..authored_edge().0[&id].2.len() {
                let mut f = authored_edge();
                f.0.get_mut(&id).unwrap().2.truncate(keep);
                assert!(resolve(f, 19).is_err(), "{id}/{keep}");
            }
        }
        let mut f = authored_edge();
        f.0.get_mut(&303).unwrap().0 = CONTAINER.into();
        assert!(resolve(f, 19).is_err());
    }
}
