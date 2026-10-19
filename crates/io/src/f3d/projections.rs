//! Sketch-curve projection relationships verified with linked/broken API controls.
use super::history::{empty, finish, payload, references, require_class, zeros};
use super::*;

pub(super) const CLASS: &str = "F1451EC8-11A4-4952-84ED-5AB7C13E2E83";

pub(super) struct Projection {
    pub target: u64,
    pub source: Source,
}

pub(super) enum Source {
    Sketch { geometry: u64, tag: u64 },
    Body(super::body_projections::Edge),
    CutBody(super::cut_face_projections::Projection),
    FaceCap(super::face_cap_projections::Projection),
    ChamferCap(super::chamfer_circle_projections::Projection),
    Composite(super::composite_projections::Projection),
}

pub(super) struct Geometry<'a> {
    pub points: &'a BTreeMap<u64, SketchPoint>,
    pub lines: &'a BTreeMap<u64, SketchLine>,
    pub circles: &'a BTreeMap<u64, SketchCircle>,
    pub arcs: &'a BTreeMap<u64, SketchArc>,
}

pub(super) struct Entities {
    pub points: Vec<(u64, usize, u64)>,
    pub curves: Vec<(u64, usize)>,
}

impl Geometry<'_> {
    pub fn source(&self, p: &Projection) -> Result<u64> {
        let Source::Sketch { geometry, tag } = &p.source else { return Err(invalid("expected projected sketch source")) };
        let mut matching = self
            .lines
            .values()
            .map(|c| (c.id, c.sketch, c.profile_id))
            .chain(self.circles.values().map(|c| (c.id, c.sketch, c.profile_id)))
            .chain(self.arcs.values().map(|a| (a.circle.id, a.circle.sketch, a.circle.profile_id)))
            .filter(|(_, sketch, candidate)| sketch == geometry && candidate.is_some_and(|(candidate, _)| candidate == *tag));
        let source = matching.next().ok_or_else(|| invalid("missing projected source curve"))?.0;
        if matching.next().is_some() {
            return Err(invalid("ambiguous projected source curve tag"));
        }
        Ok(source)
    }

    pub fn attach(
        &self,
        p: &Projection,
        geometry: u64,
        source_feature: u64,
        planes: (solidcraft_geom::Plane, solidcraft_geom::Plane),
        sketch: &mut solidcraft_doc::sketch::Sketch,
    ) -> Result<Entities> {
        use solidcraft_doc::sketch::{LinkGeom, LinkKind, LinkSource};
        use solidcraft_geom::Vec2;
        let Source::Sketch { geometry: source_geometry, .. } = &p.source else { return Err(invalid("expected projected sketch source")) };
        let source = self.source(p)?;
        let point = |id, expected: [f64; 3]| -> Result<Vec2> {
            let v = self
                .points
                .get(&id)
                .filter(|v| v.sketch == geometry && v.linked && !v.origin)
                .ok_or_else(|| invalid("missing linked projection point"))?;
            if v.position[2].abs() > 1e-10 || v.position.iter().zip(expected).any(|(a, b)| (a - b).abs() > 1e-9) {
                return Err(invalid("inconsistent projected point geometry"));
            }
            Ok(Vec2::new(v.position[0] * 10., v.position[1] * 10.))
        };
        let (geom, point_ids, construction, centerline, fixed) = if let Some(c) = self.circles.get(&p.target) {
            if c.sketch != geometry || !c.reference || c.normal != [0., 0., 1.] {
                return Err(invalid("unsupported projected circle placement"));
            }
            (LinkGeom::Circle(point(c.center_point, c.center)?, c.radius * 10.), vec![c.center_point], c.construction, false, c.fixed)
        } else if let Some(l) = self.lines.get(&p.target) {
            if l.sketch != geometry || !l.reference {
                return Err(invalid("unsupported projected line placement"));
            }
            (
                LinkGeom::Line(point(l.start_point, l.start)?, point(l.end_point, l.end)?),
                vec![l.start_point, l.end_point],
                l.construction,
                l.centerline,
                l.fixed,
            )
        } else {
            return Err(invalid("unsupported projected curve type"));
        };
        let source_points = if let Some(c) = self.circles.get(&source) {
            if !self.circles.contains_key(&p.target) {
                return Err(invalid("projected curve changed type"));
            }
            let target = self.circles.get(&p.target).ok_or_else(|| invalid("missing projected circle"))?;
            if (c.radius - target.radius).abs() > 1e-9 || (planes.0.normal().dot(planes.1.normal()).abs() - 1.).abs() > 1e-9 {
                return Err(invalid("inconsistent projected circle support"));
            }
            vec![c.center_point]
        } else if let Some(l) = self.lines.get(&source) {
            if !self.lines.contains_key(&p.target) {
                return Err(invalid("projected curve changed type"));
            }
            vec![l.start_point, l.end_point]
        } else {
            return Err(invalid("unsupported projected source curve type"));
        };
        for (target, source) in point_ids.iter().zip(&source_points) {
            let source =
                self.points.get(source).filter(|v| v.sketch == *source_geometry).ok_or_else(|| invalid("missing projection source point"))?;
            let target = self.points.get(target).ok_or_else(|| invalid("missing projection target point"))?;
            let world = planes.0.to_world(Vec2::new(source.position[0] * 10., source.position[1] * 10.));
            let expected = planes.1.to_local(world);
            if source.position[2].abs() > 1e-10 || expected.dist(Vec2::new(target.position[0] * 10., target.position[1] * 10.)) > 1e-7 {
                return Err(invalid("projected geometry disagrees with its source"));
            }
        }
        let link = sketch
            .add_link(LinkKind::Project, LinkSource::SketchCurve { sketch: source_feature, curve: format!("c{source}") }, &[geom])
            .map_err(|e| invalid(&e.to_string()))?;
        let indices = sketch.link_points(&link);
        let curves = sketch.link_curves(&link);
        let [curve] = curves.as_slice() else { return Err(invalid("unexpected projected curve count")) };
        if indices.len() != point_ids.len() {
            return Err(invalid("unexpected projected point count"));
        }
        for (id, index) in point_ids.iter().zip(&indices) {
            let point = sketch.points.get_mut(*index).ok_or_else(|| invalid("missing projected point"))?;
            point.id = format!("p{id}");
        }
        let c = sketch.curves.get_mut(*curve).ok_or_else(|| invalid("missing projected curve"))?;
        c.id = format!("c{}", p.target);
        c.construction = construction;
        c.centerline = centerline;
        c.fixed = fixed;
        Ok(Entities {
            points: point_ids.into_iter().zip(indices).zip(source_points).map(|((id, index), source)| (id, index, source)).collect(),
            curves: vec![(p.target, *curve)],
        })
    }
}

pub(super) fn version(classes: &BTreeMap<u64, &Class>, id: u64) -> Result<u32> {
    classes
        .get(&id)
        .filter(|c| c.id == CLASS && matches!(c.version, 1 | 3 | 4))
        .map(|c| c.version)
        .ok_or_else(|| invalid("unsupported projection class or version"))
}

pub(super) fn decode(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<Vec<Projection>> {
    let version = version(classes, id)?;
    let mut r = payload(records.primary(id)?, id)?;
    empty(&mut r)?;
    zeros(&mut r, 2)?;
    // Version 3 matches version 4 except for a two-byte evaluation state.
    r.take(match version {
        1 => 0,
        3 => 2,
        _ => 3,
    })?; // evaluation state
    if r.take(1)? != [1] {
        return Err(invalid("unsupported projection mapping flag"));
    }
    let mapping = r.reference()?;
    if version >= 3 {
        // Automatic face-boundary projections and explicit projections both
        // retain the same selector/tracker relationship and linked entities.
        if !matches!(r.take(1)?, [0] | [1]) {
            return Err(invalid("unsupported projection creation flag"));
        }
        let tolerance = f64::from_bits(r.u64()?);
        if !tolerance.is_finite() || tolerance <= 0. {
            return Err(invalid("invalid projection tolerance"));
        }
    }
    let mode = r.u32()?;
    if !matches!((version, mode), (1, 1) | (3 | 4, 1 | 3)) || r.reference()? != owner {
        return Err(invalid("unsupported sketch projection mode or owner"));
    }
    let container = r.reference()?;
    // Inherited evaluation metadata. Version 3 has 41 or 42 bytes (one optional
    // leading byte before its second reference); the final owner reference bounds it.
    let metadata = match version {
        1 => 41,
        3 => r.bytes.len().saturating_sub(r.at + 11),
        _ => 42,
    };
    if !matches!((version, metadata), (1, 41) | (3, 41 | 42) | (4, 42)) {
        return Err(invalid("unsupported projection metadata length"));
    }
    r.take(metadata)?;
    if r.reference()? != owner {
        return Err(invalid("inconsistent projection owner"));
    }
    finish(&r)?;
    require_class(classes, mapping, "CA72DD8E-2861-4DDD-9721-03039B5FB113", 0)?;
    let mut r = payload(records.primary(mapping)?, mapping)?;
    empty(&mut r)?;
    zeros(&mut r, 2)?;
    let count = r.count(19)?;
    if count == 0 || count > 1024 {
        return Err(invalid("unsupported projection mapping count"));
    }
    let mut mappings = Vec::new();
    let mut trackers = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for _ in 0..count {
        let tracking = r.reference()?;
        let target = r.u64()?;
        if !trackers.insert(tracking) || !targets.insert(target) || !classes.contains_key(&target) {
            return Err(invalid("duplicate or missing projection mapping"));
        }
        mappings.push((tracking, target));
    }
    if r.u64()? != 1 {
        return Err(invalid("unsupported projection mapping state"));
    }
    finish(&r)?;
    require_class(classes, container, "2CA5A1CD-C99B-4C9B-91AF-57989148E841", 1)?;
    let mut r = payload(records.primary(container)?, container)?;
    empty(&mut r)?;
    zeros(&mut r, 2)?;
    let selected = references(&mut r)?;
    let [selected] = selected.as_slice() else { return Err(invalid("projection requires one source")) };
    // Both selectors carry their owning feature in the final reference.
    let tail = r.bytes.len().checked_sub(11).filter(|at| *at >= r.at).ok_or_else(|| invalid("truncated projection container"))?;
    r.at = tail;
    if r.reference()? != owner {
        return Err(invalid("projection container belongs to another feature"));
    }
    finish(&r)?;
    if classes.get(selected).is_some_and(|c| c.id == super::body_projections::SELECTOR) {
        // Edge and bounded-face recipes have disjoint, fully validated grammars.
        // A face may map one circular boundary or four rectangular boundaries.
        let mut edges = match super::body_projections::selector(records, classes, *selected, owner) {
            Ok(edge) => vec![edge],
            Err(edge_error) => match super::body_projections::face_edges(records, classes, *selected, owner) {
                Ok(edges) => edges,
                Err(face_error) => {
                    let chamfer_error = match super::chamfer_circle_projections::decode(records, classes, *selected, owner, &mappings) {
                        Ok(projections) => return Ok(projections),
                        Err(error) => error,
                    };
                    let retired_error = match super::retired_circle_projections::decode(records, classes, *selected, owner, &mappings) {
                        Ok(projections) => return Ok(projections),
                        Err(error) => error,
                    };
                    return super::face_cap_projections::decode(records, classes, *selected, owner, &mappings).or_else(|cap_error| {
                        super::cut_face_projections::decode(records, classes, *selected, owner, &mappings).or_else(|cut_error| {
                            super::composite_projections::decode(records, classes, *selected, owner, &mappings).map_err(|composite_error| {
                                invalid(&format!("unsupported projected body selection: {edge_error}; {face_error}; {chamfer_error}; {retired_error}; {cap_error}; {cut_error}; {composite_error}"))
                            })
                        })
                    });
                }
            },
        };
        let mut projections = Vec::new();
        for (tracking, target) in mappings {
            let identity = tracking_identity(records, classes, tracking, "4B31DA1B-FCD6-4527-BC3E-BE1FBAF867FE", 1)?;
            let recipe = super::body_projections::identity(records, classes, identity, true)?;
            let index =
                edges.iter().position(|edge| edge.recipe == recipe).ok_or_else(|| invalid("projected face mapping disagrees with its boundary"))?;
            projections.push(Projection { target, source: Source::Body(edges.remove(index)) });
        }
        if !edges.is_empty() {
            return Err(invalid("incomplete projected face boundary"));
        }
        return Ok(projections);
    }
    let [(tracking, target)] = mappings.as_slice() else { return Err(invalid("missing projection mapping")) };
    if version == 4 && mode != 3 {
        return Err(invalid("unsupported sketch-curve projection mode"));
    }
    let (geometry, tag) = {
        require_class(classes, *selected, "5A1BF548-241F-46FD-9FB5-E4B05126EB9D", 1)?;
        let mut r = payload(records.primary(*selected)?, *selected)?;
        empty(&mut r)?;
        zeros(&mut r, 2)?;
        let identity = r.reference()?;
        let tail = r.bytes.len().checked_sub(11).filter(|at| *at >= r.at).ok_or_else(|| invalid("truncated projection selector"))?;
        r.at = tail;
        if r.reference()? != owner {
            return Err(invalid("projection selector belongs to another feature"));
        }
        finish(&r)?;
        source_identity(records, classes, identity)?
    };
    let tracked_identity = tracking_identity(records, classes, *tracking, "CB4F4CC2-9D6F-48BA-9B1F-20C1992C9C9C", 2)?;
    if source_identity(records, classes, tracked_identity)? != (geometry, tag) {
        return Err(invalid("projection mapping disagrees with its source selector"));
    }
    Ok(vec![Projection { target: *target, source: Source::Sketch { geometry, tag } }])
}

pub(super) fn tracking_identity(
    records: &Records<'_>,
    classes: &BTreeMap<u64, &Class>,
    tracking: u64,
    tracking_class: &str,
    tracking_version: u32,
) -> Result<u64> {
    require_class(classes, tracking, tracking_class, tracking_version)?;
    let mut r = payload(records.primary(tracking)?, tracking)?;
    empty(&mut r)?;
    zeros(&mut r, 1)?;
    match r.take(1)? {
        [0] => {}
        [1] => {
            let mut names = BTreeSet::new();
            for _ in 0..r.count(19)? {
                if !names.insert(r.string()?) || r.string()? != "StrongRefMetaType" {
                    return Err(invalid("unsupported projection tracking attributes"));
                }
                let related = r.reference()?;
                require_class(classes, related, tracking_class, tracking_version)?;
            }
        }
        _ => return Err(invalid("unsupported projection tracking metadata")),
    }
    let tracked_identity = r.reference()?;
    zeros(&mut r, 1)?;
    finish(&r)?;
    Ok(tracked_identity)
}

pub(super) fn source_identity(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64) -> Result<(u64, u64)> {
    require_class(classes, id, "E2CEFD18-D755-4E09-8E7F-953A2F6D43F8", 0)?;
    let mut r = payload(records.primary(id)?, id)?;
    empty(&mut r)?;
    zeros(&mut r, 10)?;
    let source_geometry = r.u64()?;
    let source_tag = r.u64()?;
    // The identity record's own v0 schema is unchanged across these sketch
    // generations. The referenced sketch is decoded by its versioned reader.
    if !classes.get(&source_geometry).is_some_and(|c| c.id == "44A64366-4BD3-4B24-881A-F94C206E8F2D" && matches!(c.version, 16..=18)) {
        return Err(invalid("unsupported source sketch identity schema"));
    }
    finish(&r)?;
    Ok((source_geometry, source_tag))
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use crate::f3d::import_tests::Fixture;

    #[test]
    #[ignore = "requires a local native projection archive"]
    fn inspect_native_bounded_projection_graph() {
        let path = std::env::var("SOLIDCRAFT_PROJECTION_ARCHIVE").unwrap();
        let bytes = std::fs::read(path).unwrap();
        let designs = read_f3d(&bytes).unwrap();
        let design = &designs[0];
        let records = Records::new(design);
        let classes = design.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let history = design.history().unwrap();
        let name = std::env::var("SOLIDCRAFT_PROJECTION_FEATURE").unwrap_or_else(|_| "Sketch7".into());
        let feature = history.iter().find(|f| f.name == name).unwrap();
        let HistoryKind::Sketch { placement, .. } = &feature.kind else { panic!("sketch") };
        for id in &placement.projections {
            let v = version(&classes, *id).unwrap();
            let mut r = payload(records.primary(*id).unwrap(), *id).unwrap();
            empty(&mut r).unwrap();
            zeros(&mut r, 2).unwrap();
            if v == 4 {
                r.take(3).unwrap();
            }
            assert_eq!(r.take(1).unwrap(), [1]);
            let mapping = r.reference().unwrap();
            if v == 4 {
                r.take(9).unwrap();
            }
            let mode = r.u32().unwrap();
            assert_eq!(r.reference().unwrap(), feature.id);
            let container = r.reference().unwrap();
            let mut r = payload(records.primary(container).unwrap(), container).unwrap();
            empty(&mut r).unwrap();
            zeros(&mut r, 2).unwrap();
            let selected = references(&mut r).unwrap();
            for selector in selected {
                let v = super::super::loft_faces::selector_version(&classes, selector).unwrap();
                let mut r = payload(records.primary(selector).unwrap(), selector).unwrap();
                zeros(&mut r, if v == 2 { 7 } else { 8 }).unwrap();
                let identity = r.reference().unwrap();
                let graph = super::super::recipe_incidence::identity(&records, &classes, identity, true).unwrap();
                println!("SELECT projection={id} mode={mode} selector={selector} terms={:?} edges={}", graph.terms, graph.edges.len());
                assert_eq!(r.u32().unwrap(), 1);
                r.wide_string().unwrap();
                r.wide_string().unwrap();
                assert_eq!(r.u32().unwrap(), 2);
                zeros(&mut r, 4).unwrap();
                for cached in references(&mut r).unwrap() {
                    println!("CACHE {cached}: {:?}", super::super::composite_projections::cached_identity(&records, &classes, cached));
                }
            }
            let mut r = payload(records.primary(mapping).unwrap(), mapping).unwrap();
            empty(&mut r).unwrap();
            zeros(&mut r, 2).unwrap();
            for _ in 0..r.count(19).unwrap() {
                let tracker = r.reference().unwrap();
                let target = r.u64().unwrap();
                let identity = tracking_identity(&records, &classes, tracker, "4B31DA1B-FCD6-4527-BC3E-BE1FBAF867FE", 1).unwrap();
                println!(
                    "MAP target={target} identity={identity}: {:?}",
                    super::super::recipe_incidence::identity(&records, &classes, identity, false)
                );
            }
        }
    }

    #[test]
    fn persistent_curve_identity_is_scoped_to_a_supported_sketch_generation() {
        for version in [15, 16, 17, 18, 19] {
            let mut fixture = Fixture(BTreeMap::new());
            let mut identity = vec![0; 14];
            identity.extend(21_u64.to_le_bytes());
            identity.extend(37_u64.to_le_bytes());
            fixture.put(20, "E2CEFD18-D755-4E09-8E7F-953A2F6D43F8", 0, identity, None);
            fixture.put(21, "44A64366-4BD3-4B24-881A-F94C206E8F2D", version, vec![], None);
            let design = fixture.design();
            let records = Records::new(&design);
            let classes = design.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
            let decoded = source_identity(&records, &classes, 20);
            if (16..=18).contains(&version) {
                assert_eq!(decoded.unwrap(), (21, 37));
            } else {
                assert!(decoded.is_err());
            }
        }
    }
}
