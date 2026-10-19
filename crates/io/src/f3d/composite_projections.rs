//! Live projection of a merged or rounded extrusion cap. The selector's cached boundary
//! may describe a later state; mapped curve trackers describe the sketch's
//! original boundary. Both graphs must be complete, and reconstruction must
//! agree with every mapped curve before a live face link is installed.
use super::body_projections::Context;
use super::history::{finish, payload, references, require_class, zeros};
use super::recipe_incidence::{self, Term};
use super::*;
use solidcraft_doc::sketch::{CurveKind, LinkGeom, LinkKind, Sketch};
use solidcraft_doc::{PlaneRef, project};
use solidcraft_geom::{Plane, Vec2};

pub(super) struct Projection {
    pub body: u64,
    creator: u64,
    roles: BTreeMap<u64, bool>,
    boundary_operations: BTreeSet<u64>,
    targets: Vec<u64>,
}

fn cap(term: &Term) -> Result<BTreeMap<u64, bool>> {
    if !(1..=64).contains(&term.len()) {
        return Err(invalid("projected cap requires one to 64 contributors"));
    }
    let mut roles = BTreeMap::new();
    for (operation, label) in term {
        if !matches!(label, 1 | 2) || roles.insert(*operation, *label == 2).is_some() {
            return Err(invalid("invalid projected composite cap roles"));
        }
    }
    Ok(roles)
}

pub(super) fn cached_identity(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64) -> Result<recipe_incidence::Edge> {
    let class = classes.get(&id).ok_or_else(|| invalid("missing projected cap cache"))?;
    let count = match (class.id.as_str(), class.version) {
        ("B0B3FA59-C374-444F-B424-24AA5DE41645", 0) => 12,
        ("50AEE8B4-9456-4DD7-847D-6F6C909BBB9D", 0) => 6,
        _ => return Err(invalid("unsupported projected cap cache type")),
    };
    let mut r = payload(records.primary(id)?, id)?;
    zeros(&mut r, 6)?;
    for _ in 0..count {
        if !f64::from_bits(r.u64()?).is_finite() {
            return Err(invalid("non-finite projected cap cache"));
        }
    }
    zeros(&mut r, 1)?;
    require_class(classes, r.reference()?, "C46D3EEB-40E1-43FA-83DC-9CB204335417", 1)?;
    if r.take(2)? != [0, 1] {
        return Err(invalid("unsupported projected cap cache tracking"));
    }
    let tracker = r.reference()?;
    finish(&r)?;
    let id = super::projections::tracking_identity(records, classes, tracker, "4B31DA1B-FCD6-4527-BC3E-BE1FBAF867FE", 1)?;
    let mut identity = recipe_incidence::identity(records, classes, id, false)?;
    identity.edges.pop().ok_or_else(|| invalid("missing cached cap edge identity"))
}

pub(super) fn decode(
    records: &Records<'_>,
    classes: &BTreeMap<u64, &Class>,
    selector: u64,
    owner: u64,
    mappings: &[(u64, u64)],
) -> Result<Vec<super::projections::Projection>> {
    if !(3..=128).contains(&mappings.len()) {
        return Err(invalid("unsupported composite cap mapping count"));
    }
    let version = super::loft_faces::selector_version(classes, selector)?;
    let mut r = payload(records.primary(selector)?, selector)?;
    zeros(&mut r, if version == 2 { 7 } else { 8 })?;
    let selected = recipe_incidence::identity(records, classes, r.reference()?, true)?;
    let term = selected.terms.first().ok_or_else(|| invalid("missing projected composite cap"))?;
    let roles = cap(term)?;
    let single_cap = roles.len() == 1;
    let boundary_operations: BTreeSet<_> = selected.terms.iter().flatten().map(|(op, _)| *op).collect();
    // A plain extrusion cap belongs to the stricter simple-face decoder. This
    // fallback requires additional boundary provenance, validated against this
    // body's evaluated history; it must not bypass simple-cache checks.
    if single_cap && boundary_operations.len() < 2 {
        return Err(invalid("single-cap projection has no additional boundary provenance"));
    }
    if r.u32()? != 1 {
        return Err(invalid("composite cap requires one selected body"));
    }
    r.wide_string()?;
    r.wide_string()?;
    if r.u32()? != 2 {
        return Err(invalid("unsupported composite cap selector type"));
    }
    super::history::selection_flag(&mut r)?;
    let cached = references(&mut r)?;
    if cached.len() != mappings.len()
        || cached.iter().collect::<BTreeSet<_>>().len() != cached.len()
        || cached.iter().any(|id| {
            classes.get(id).is_none_or(|c| {
                c.version != 0 || !matches!(c.id.as_str(), "50AEE8B4-9456-4DD7-847D-6F6C909BBB9D" | "B0B3FA59-C374-444F-B424-24AA5DE41645")
            })
        })
    {
        return Err(invalid("incomplete composite cap cache"));
    }
    let mut cached = cached.iter().map(|id| cached_identity(records, classes, *id)).collect::<Result<Vec<_>>>()?;
    if single_cap && !recipe_incidence::same_boundary(&selected.edges, &cached)? {
        return Err(invalid("cached cap boundary disagrees with its selector"));
    }
    let handle = super::loft_faces::selector_tail(&mut r, records, owner)?;
    finish(&r)?;
    let (body, creator) = super::body_projections::body_owner(records, classes, handle)?;
    let mut edges = Vec::new();
    let mut targets = Vec::new();
    for (tracker, target) in mappings {
        let id = super::projections::tracking_identity(records, classes, *tracker, "4B31DA1B-FCD6-4527-BC3E-BE1FBAF867FE", 1)?;
        let identity = recipe_incidence::identity(records, classes, id, false)?;
        let [edge] = identity.edges.as_slice() else { return Err(invalid("missing composite cap curve tracker")) };
        if !edge.faces.contains(term)
            || (!single_cap
                && identity
                    .terms
                    .iter()
                    .any(|t| t != term && (t.len() != 1 || t.iter().any(|(op, label)| !roles.contains_key(op) || matches!(label, 1 | 2)))))
            || (single_cap && identity.terms.iter().any(|t| !selected.terms.contains(t)))
        {
            return Err(invalid("projected curve does not belong to its composite cap"));
        }
        if !single_cap {
            let index = cached
                .iter()
                .position(|candidate| candidate == edge)
                .ok_or_else(|| invalid("projected cap tracker disagrees with its cached identity"))?;
            cached.remove(index);
        }
        edges.push(edge.clone());
        targets.push(*target);
    }
    recipe_incidence::cycle(&edges)?;
    if single_cap && !recipe_incidence::same_boundary(&edges, &selected.edges)? {
        return Err(invalid("projected tracker graph disagrees with its cap boundary"));
    }
    let target = *targets.first().ok_or_else(|| invalid("missing composite cap target"))?;
    Ok(vec![super::projections::Projection {
        target,
        source: super::projections::Source::Composite(Projection { body, creator, roles, boundary_operations, targets }),
    }])
}

struct Expected {
    id: u64,
    geom: LinkGeom,
    points: Vec<(u64, Vec2)>,
    construction: bool,
    centerline: bool,
    fixed: bool,
}

fn expected(native: &super::projections::Geometry<'_>, id: u64, geometry: u64) -> Result<Expected> {
    let point = |id: u64, expected: Vec2| -> Result<(u64, Vec2)> {
        let p =
            native.points.get(&id).filter(|p| p.sketch == geometry && p.linked && !p.origin).ok_or_else(|| invalid("missing linked cap point"))?;
        let value = Vec2::new(p.position[0], p.position[1]) * 10.0;
        if p.position[2].abs() > 1e-10 || value.dist(expected) > 1e-6 {
            return Err(invalid("inconsistent projected cap point"));
        }
        Ok((id, value))
    };
    if let Some(line) = native.lines.get(&id).filter(|l| l.sketch == geometry && l.reference) {
        let a = Vec2::new(line.start[0], line.start[1]) * 10.0;
        let b = Vec2::new(line.end[0], line.end[1]) * 10.0;
        if line.start[2].abs() > 1e-10 || line.end[2].abs() > 1e-10 {
            return Err(invalid("nonplanar projected cap line"));
        }
        return Ok(Expected {
            id,
            geom: LinkGeom::Line(a, b),
            points: vec![point(line.start_point, a)?, point(line.end_point, b)?],
            construction: line.construction,
            centerline: line.centerline,
            fixed: line.fixed,
        });
    }
    let arc = native
        .arcs
        .get(&id)
        .filter(|a| a.circle.sketch == geometry && a.circle.reference)
        .ok_or_else(|| invalid("unsupported projected cap curve"))?;
    let circle = &arc.circle;
    if circle.normal.iter().zip([0., 0., 1.]).any(|(value, expected)| (value - expected).abs() > 1e-10)
        || circle.center[2].abs() > 1e-10
        || circle.x_axis[2].abs() > 1e-10
        || arc.end_angle <= arc.start_angle
        || arc.end_angle - arc.start_angle >= std::f64::consts::TAU
    {
        return Err(invalid("unsupported projected cap arc orientation"));
    }
    let c = Vec2::new(circle.center[0], circle.center[1]) * 10.0;
    let x = Vec2::new(circle.x_axis[0], circle.x_axis[1]);
    let y = Vec2::new(-x.y, x.x);
    let at = |angle: f64| c + (x * angle.cos() + y * angle.sin()) * (circle.radius * 10.0);
    let (a, b) = (at(arc.start_angle), at(arc.end_angle));
    Ok(Expected {
        id,
        geom: LinkGeom::Arc { c, a, b },
        points: vec![point(circle.center_point, c)?, point(arc.start_point, a)?, point(arc.end_point, b)?],
        construction: circle.construction,
        centerline: false,
        fixed: circle.fixed,
    })
}

fn same(a: &LinkGeom, b: &LinkGeom) -> bool {
    let near = |a: Vec2, b: Vec2| a.dist(b) < 1e-5;
    match (a, b) {
        (LinkGeom::Line(a, b), LinkGeom::Line(c, d)) => (near(*a, *c) && near(*b, *d)) || (near(*a, *d) && near(*b, *c)),
        (LinkGeom::Arc { c, a, b }, LinkGeom::Arc { c: d, a: p, b: q }) => near(*c, *d) && near(*a, *p) && near(*b, *q),
        _ => false,
    }
}

impl Projection {
    pub fn attach(
        &self,
        context: &Context<'_>,
        history: &solidcraft_doc::Model,
        geometry: u64,
        plane: Plane,
        support: &PlaneRef,
        sketch: &mut Sketch,
    ) -> Result<super::projections::Entities> {
        let creator = *context.features.get(&self.creator).ok_or_else(|| invalid("missing composite projection body creator"))?;
        let mut roles = BTreeMap::new();
        for (op, end) in &self.roles {
            let id = context
                .operations
                .get(op)
                .and_then(|native| context.features.get(native))
                .ok_or_else(|| invalid("projection precedes its cap producer"))?;
            if roles.insert(*id, *end).is_some() {
                return Err(invalid("duplicate projected cap producer"));
            }
        }
        let anchor = if roles.len() == 1 {
            let (&producer, &end) = roles.first_key_value().ok_or_else(|| invalid("missing projected cap producer"))?;
            // A fillet can replace the cap's raw history alias while retaining
            // its extrusion certificate. Resolve that certificate, then require
            // the same live support and the complete mapped geometric outline.
            let anchor = super::loft_faces::Face::cap(0, 0, end, false).anchor(context.doc, context.model, creator, &[(producer, None)])?;
            let mut modifiers = 0;
            for operation in &self.boundary_operations {
                let id = context
                    .operations
                    .get(operation)
                    .and_then(|f| context.features.get(f))
                    .ok_or_else(|| invalid("projection precedes a boundary producer"))?;
                let feature = context.doc.feature(*id).filter(|f| !f.suppressed).ok_or_else(|| invalid("missing cap boundary producer"))?;
                match &feature.kind {
                    solidcraft_doc::FeatureKind::Extrude { operation, targets, .. }
                        if (*id == creator && *operation == solidcraft_doc::Operation::NewBody)
                            || (*operation == solidcraft_doc::Operation::Join && targets.as_slice() == [anchor.target.body.clone()]) => {}
                    solidcraft_doc::FeatureKind::Fillet { body: Some(body), .. } if *body == anchor.target.body => {
                        modifiers += 1;
                    }
                    _ => return Err(invalid("unsupported single-cap boundary producer")),
                }
            }
            if modifiers == 0 {
                // Joined extrusion caps can have a merged cylindrical boundary
                // without any rounds. Accept that case only while the body is
                // still the exact evaluated result of this cap's producer. An
                // unreported later fillet must not pass as an unrounded join.
                let source = history
                    .result(producer)
                    .filter(|r| r.error.is_none() && !r.skipped && !r.stale)
                    .and_then(|r| r.state.body(&anchor.target.body))
                    .ok_or_else(|| invalid("joined cap has no evaluated producer"))?;
                let current = context.model.body(&anchor.target.body).ok_or_else(|| invalid("projected cap body is missing"))?;
                if producer == creator || !current.body.same(&source.body) {
                    return Err(invalid("unrounded joined-cap projection has later body changes"));
                }
            }
            anchor
        } else {
            super::loft_faces::composite_cap_anchor(context.doc, context.model, creator, &roles)?
        };
        let PlaneRef::Face { anchor: Some(support), .. } = support else {
            return Err(invalid("composite projection requires a live face attachment"));
        };
        if support.target != anchor.target {
            return Err(invalid("composite projection disagrees with sketch support"));
        }
        let resolved = project::resolve_joined_face(context.doc, context.model, &plane, &anchor)?;
        if !resolved.wires.is_empty() || resolved.geom.len() != self.targets.len() {
            return Err(invalid("regenerated composite cap has a different boundary"));
        }
        let expected = self.targets.iter().map(|id| expected(context.native, *id, geometry)).collect::<Result<Vec<_>>>()?;
        let mut assignment = Vec::new();
        let mut selected = BTreeSet::new();
        for curve in &resolved.geom {
            let matches = expected.iter().enumerate().filter(|(_, e)| same(curve, &e.geom)).map(|(i, _)| i).collect::<Vec<_>>();
            let [i] = matches.as_slice() else { return Err(invalid("regenerated cap curve disagrees with projection trackers")) };
            if !selected.insert(*i) {
                return Err(invalid("duplicate regenerated cap curve"));
            }
            assignment.push(*i);
        }
        let link = sketch.add_link(LinkKind::Project, resolved.source, &resolved.geom).map_err(|e| invalid(&e.to_string()))?;
        let entry = sketch.links.iter_mut().find(|l| l.id == link).ok_or_else(|| invalid("missing composite face link"))?;
        entry.face_name = Some(anchor.target.name);
        entry.auto_face = true;
        entry.join_arcs = true;
        let curves = sketch.link_curves(&link);
        if curves.len() != assignment.len() {
            return Err(invalid("unexpected composite projection curve count"));
        }
        let mut points = BTreeMap::new();
        let mut renamed = BTreeSet::new();
        let mut entities = Vec::new();
        for (index, assigned) in curves.into_iter().zip(assignment) {
            let source = expected.get(assigned).ok_or_else(|| invalid("missing projected cap curve"))?;
            let curve = sketch.curves.get(index).ok_or_else(|| invalid("missing linked cap curve"))?;
            let indices = match curve.kind {
                CurveKind::Line { a, b } => vec![a, b],
                CurveKind::Arc { c, a, b } => vec![c, a, b],
                _ => return Err(invalid("unexpected linked cap curve type")),
            };
            let mut native_order = Vec::new();
            for (id, position) in &source.points {
                let matches = indices.iter().copied().filter(|i| sketch.point(*i).is_some_and(|p| p.dist(*position) < 1e-5)).collect::<Vec<_>>();
                let [i] = matches.as_slice() else { return Err(invalid("projected cap endpoint does not match its curve")) };
                if points.insert(*id, *i).is_some_and(|previous| previous != *i) {
                    return Err(invalid("shared projected cap point is inconsistent"));
                }
                if renamed.insert(*i) {
                    sketch.points.get_mut(*i).ok_or_else(|| invalid("missing linked cap point"))?.id = format!("p{id}");
                }
                native_order.push(*i);
            }
            let curve = sketch.curves.get_mut(index).ok_or_else(|| invalid("missing linked cap curve"))?;
            if let (CurveKind::Line { a, b }, [start, end]) = (&mut curve.kind, native_order.as_slice()) {
                (*a, *b) = (*start, *end);
            }
            curve.id = format!("c{}", source.id);
            curve.construction = source.construction;
            curve.centerline = source.centerline;
            curve.fixed = source.fixed;
            entities.push((source.id, index));
        }
        Ok(super::projections::Entities { points: points.into_iter().map(|(id, index)| (id, index, index as u64)).collect(), curves: entities })
    }
}

#[cfg(test)]
#[path = "rounded_cap_projection_tests.rs"]
mod rounded_cap_tests;

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires a local joined-cap F3D archive"]
    fn native_joined_cap_projection_preserves_cut_volume_and_parameter_edits() {
        let path = std::env::var("SOLIDCRAFT_JOINED_PROJECTION_ARCHIVE").unwrap();
        let bytes = std::fs::read(path).unwrap();
        let outcome = super::super::import_f3d_with_preference(&bytes, "Joined cap", super::super::ImportPreference::Editability).unwrap();
        let mut doc = outcome.document;
        for f in &doc.features {
            println!("FEATURE {} {}", f.id, f.name);
        }
        println!("WARNINGS {:?}", doc.import_warnings);
        let sketch_id = doc.find_feature("Sketch6").expect("projected cap sketch must import").id;
        let cut = doc.features.iter().position(|f| f.name == "Extrude6").expect("projected profile must drive its cut");
        // Isolate the cap and its circular-strip cut from later blends.
        doc.features.truncate(cut + 1);
        for (diameter, width, height, grip, inset) in [(35_f64, 10., 10., 12.5, 5.), (37., 11., 11., 14.5, 6.)] {
            for (name, value) in [("Diameter", diameter), ("KnobWidth", width), ("Height", height), ("KnobHeight", grip), ("KnobInset", inset)] {
                doc.change_param(name, &format!("{value} mm"), None, None).unwrap();
            }
            let reopened = crate::read_design(&crate::write_design_container(&doc).unwrap()).unwrap();
            assert_eq!(doc, reopened);
            let mut model = solidcraft_doc::Model::new();
            model.evaluate(&reopened);
            assert!(model.results.iter().all(|r| r.error.is_none()), "{:?}", model.results);
            let state = model.state();
            let projected = state.sketch(sketch_id).unwrap();
            let link = projected.sketch.links.iter().find(|l| l.auto_face && l.join_arcs).unwrap();
            assert!(!link.lost);
            assert_eq!(projected.sketch.link_curves(&link.id).len(), 4);
            assert!((projected.plane.origin.z - height - grip).abs() < 1e-7);
            let r = (diameter - inset) / 2.;
            let a = width / 2.;
            let strip = 2. * (a * (r * r - a * a).sqrt() + r * r * (a / r).asin());
            let expected = std::f64::consts::PI * (diameter / 2.).powi(2) * height + strip * grip;
            let volume: f64 = state.bodies.iter().map(|b| solidcraft_kernel::measure(&b.body).unwrap().volume).sum();
            assert!((volume - expected).abs() < expected * 1e-3, "{volume} != {expected}");
            let fine: f64 = state.bodies.iter().map(|b| b.body.tessellate_with(1e-5, true).unwrap().measure().volume).sum();
            assert!((fine - expected).abs() < 0.05, "fine volume {fine} != {expected}");
            println!("VOLUME {volume:.6}, fine={fine:.6}, expected={expected:.6}");
        }
    }

    #[test]
    #[ignore = "requires a local rounded-cap F3D archive"]
    fn native_rounded_cap_projection_retains_live_outline() {
        let path = std::env::var("SOLIDCRAFT_PROJECTION_ARCHIVE").unwrap();
        let bytes = std::fs::read(path).unwrap();
        let outcome = super::super::import_f3d_with_preference(&bytes, "Rounded cap", super::super::ImportPreference::Editability).unwrap();
        let mut doc = outcome.document;
        let projected = doc.find_feature("Sketch7").unwrap_or_else(|| panic!("missing projected history: {:?}", doc.import_warnings));
        let id = projected.id;
        let solidcraft_doc::FeatureKind::Sketch { sketch, .. } = &projected.kind else { panic!("sketch") };
        let link = sketch.links.iter().find(|link| link.auto_face && link.join_arcs).unwrap();
        assert_eq!(sketch.link_curves(&link.id).len(), 8);
        assert!(doc.find_feature("Extrude6").is_some());
        let reopened = crate::read_design(&crate::write_design_container(&doc).unwrap()).unwrap();
        assert_eq!(doc, reopened);
        let mut model = solidcraft_doc::Model::new();
        model.evaluate(&reopened);
        assert!(model.results.iter().all(|r| r.error.is_none()));
        let measure =
            |model: &solidcraft_doc::Model| -> f64 { model.state().bodies.iter().map(|b| solidcraft_kernel::measure(&b.body).unwrap().volume).sum() };
        let baseline = measure(&model);
        let before = model.state().sketch(id).unwrap().clone();
        assert!(before.sketch.links.iter().all(|l| !l.lost));
        doc.change_param("Height", "11 mm", None, None).unwrap();
        model.evaluate(&doc);
        assert!(model.results.iter().all(|r| r.error.is_none()));
        let state = model.state();
        let linked = state.sketch(id).unwrap();
        assert!(linked.sketch.links.iter().all(|l| !l.lost));
        assert!((linked.plane.origin.z - before.plane.origin.z - 1.).abs() < 1e-7);
        // One extra millimetre of the radius-17.5 base, with the grip and
        // pointer recess translated together.
        assert!((measure(&model) - baseline - std::f64::consts::PI * 17.5 * 17.5).abs() < 1.);
        doc.change_param("d18", "1 mm", None, None).unwrap();
        let edited = crate::read_design(&crate::write_design_container(&doc).unwrap()).unwrap();
        assert_eq!(doc, edited);
        model.evaluate(&edited);
        assert!(model.results.iter().all(|r| r.error.is_none()));
        let state = model.state();
        let linked = state.sketch(id).unwrap();
        assert!(linked.sketch.links.iter().all(|l| !l.lost));
        let positions = |sketch: &solidcraft_doc::sketch::Sketch| (0..sketch.points.len()).map(|i| sketch.point(i)).collect::<Vec<_>>();
        assert_ne!(positions(&before.sketch), positions(&linked.sketch), "the rounded projection must update after changing radius");
        println!("WARNINGS {:?}", doc.import_warnings);
        if let Ok(path) = std::env::var("SOLIDCRAFT_PROJECTION_SAVE") {
            std::fs::write(path, crate::write_design_container(&reopened).unwrap()).unwrap();
        }
    }

    #[test]
    fn cap_roles_and_arc_direction_are_not_inferred_from_positions() {
        use super::*;
        assert_eq!(cap(&BTreeSet::from([(71, 2), (83, 1)])).unwrap(), BTreeMap::from([(71, true), (83, false)]));
        assert_eq!(cap(&BTreeSet::from([(71, 2)])).unwrap(), BTreeMap::from([(71, true)]));
        for term in [BTreeSet::new(), BTreeSet::from([(71, 2), (71, 1)]), BTreeSet::from([(71, 2), (83, 3)])] {
            assert!(cap(&term).is_err());
        }
        let arc = LinkGeom::Arc { c: Vec2::ZERO, a: Vec2::X, b: Vec2::Y };
        assert!(same(&arc, &arc));
        assert!(!same(&arc, &LinkGeom::Arc { c: Vec2::ZERO, a: Vec2::Y, b: Vec2::X }));
    }

    #[test]
    #[ignore = "requires the local composite-cap F3D archive"]
    fn native_composite_cap_keeps_linked_outline_and_reopens() {
        let path = std::env::var("SOLIDCRAFT_COMPOSITE_ARCHIVE").unwrap();
        let bytes = std::fs::read(path).unwrap();
        let outcome = super::super::import_f3d_with_preference(&bytes, "Composite cap", super::super::ImportPreference::Editability).unwrap();
        let mut doc = outcome.document;
        for f in &doc.features {
            println!("FEATURE {} {}", f.id, f.name);
        }
        for w in &doc.import_warnings {
            println!("WARNING {w}");
        }
        let feature = doc.find_feature("Sketch5").expect("Sketch5 must import");
        let id = feature.id;
        let solidcraft_doc::FeatureKind::Sketch { sketch, .. } = &feature.kind else { panic!("Sketch5") };
        assert!(sketch.links.iter().any(|l| l.auto_face && l.join_arcs));
        assert!(doc.find_feature("Extrude7").is_some(), "the projected profile must drive its extrusion");
        let reopened = crate::read_design(&crate::write_design_container(&doc).unwrap()).unwrap();
        assert_eq!(doc, reopened);
        let mut model = solidcraft_doc::Model::new();
        model.evaluate(&reopened);
        assert!(model.results.iter().all(|r| r.error.is_none()));
        assert!(model.state().sketch(id).unwrap().sketch.links.iter().all(|l| !l.lost));
        let measure =
            |model: &solidcraft_doc::Model| -> f64 { model.state().bodies.iter().map(|b| solidcraft_kernel::measure(&b.body).unwrap().volume).sum() };
        let baseline = measure(&model);
        let before_plane = model.state().sketch(id).unwrap().plane;
        doc.change_param("Height", "10 mm", None, None).unwrap();
        let edited = crate::read_design(&crate::write_design_container(&doc).unwrap()).unwrap();
        assert_eq!(doc, edited);
        model.evaluate(&edited);
        assert!(model.results.iter().all(|r| r.error.is_none()));
        let state = model.state();
        let linked = state.sketch(id).unwrap();
        assert!(linked.sketch.links.iter().all(|l| !l.lost));
        assert!((linked.plane.origin.z - before_plane.origin.z - 1.0).abs() < 1e-7);
        // Height adds one millimetre to the radius-14 base; the shoulder,
        // neck, wing and decal recess translate without changing their volumes.
        let expected = baseline + std::f64::consts::PI * 14.0 * 14.0;
        assert!((measure(&model) - expected).abs() < 1.0);
        println!("VOLUME baseline={baseline:.6}, edited={:.6}, expected={expected:.6}", measure(&model));
        if let Ok(path) = std::env::var("SOLIDCRAFT_COMPOSITE_SAVE") {
            std::fs::write(path, crate::write_design_container(&reopened).unwrap()).unwrap();
        }
    }
}
