//! Reconstruct verified native feature schemas as editable SolidCraft history.
//! Unsupported schemas fail before a caller replaces its current document.
use super::history::{empty, finish, payload, references, zeros};
use super::*;
use solidcraft_doc::expr::Kind;
use solidcraft_doc::sketch::{ConstraintKind, Sketch};
use solidcraft_doc::{Direction, Document, Extent, FeatureKind, Operation, Param, PlaneRef, ProfileSel};
use solidcraft_geom::{Plane, Vec2, Vec3};

pub fn import_f3d(bytes: &[u8], name: &str) -> Result<Document> {
    let designs = read_f3d(bytes)?;
    if designs.len() != 1 {
        return Err(invalid("multiple design streams are not supported yet"));
    }
    designs.first().ok_or_else(|| invalid("missing design"))?.to_document(name)
}

impl Design {
    pub fn to_document(&self, name: &str) -> Result<Document> {
        let mut omitted = BTreeSet::new();
        let history = if self.recovering() {
            self.recover_collection(
                "timeline",
                self.timeline().map(|entries| {
                    entries
                        .into_iter()
                        .filter_map(|entry| match entry.feature {
                            Ok(feature) => Some(feature),
                            Err(error) => {
                                omitted.insert(entry.id);
                                self.warn(format!("Omitted timeline feature {}: {error}", entry.id));
                                None
                            }
                        })
                        .collect()
                }),
            )?
        } else {
            self.history()?
        };
        let has_offset_planes = history.iter().any(|f| matches!(f.kind, HistoryKind::OffsetPlane { .. }));
        if history.len() > solidcraft_doc::MAX_FEATURES {
            return Err(invalid("too many imported features"));
        }
        let components = self.classes.iter().find(|c| c.id == "E03784ED-5E19-4E14-B9F2-3B07017018CD");
        let root = components
            // Only the root's identity is used here. Origin groups and sketch
            // placement validate their owner independently; no component payload
            // or assembly transform is inferred from any generation (18 to 23).
            .filter(|c| (matches!(c.version, 18..=23) || self.recovering()) && c.objects.len() == 1)
            .and_then(|c| c.objects.first())
            .copied()
            .ok_or_else(|| invalid("component/assembly reconstruction is not supported yet"))?;
        let records = Records::new(self);
        let classes: BTreeMap<_, _> = self.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let mut parameters = self.recover_collection("parameters", self.parameters())?;
        if self.recovering() {
            self.recover_parameters(&mut parameters);
        }
        if parameters.iter().any(|p| p.driven) {
            return Err(invalid("driven parameter measurement dependencies are not supported yet"));
        }
        if parameters.len() > solidcraft_doc::MAX_PARAMS {
            return Err(invalid("too many imported parameters"));
        }
        let bindings: BTreeMap<_, _> = parameters.iter().filter_map(|p| p.binding.map(|binding| (binding, p))).collect();
        let named_parameters: BTreeMap<_, _> = parameters.iter().map(|p| (p.name.as_str(), p)).collect();
        let mut internal_offsets = BTreeSet::new();
        for f in &history {
            if let HistoryKind::OffsetPlane { extra_offset: Some(extra_offset), .. } = &f.kind {
                let p = length_parameter(&bindings, *extra_offset)?;
                let mut expression = p.expression.split_whitespace();
                if p.owner != Some(f.id)
                    || p.label != "ExtraOffset"
                    || p.value != 0.
                    || expression.next().and_then(|s| s.parse::<f64>().ok()) != Some(0.)
                    || expression.next() != Some(p.display_unit.as_str())
                    || expression.next().is_some()
                {
                    return Err(invalid("unsupported internal plane offset"));
                }
                // Fusion does not expose this zero internal slot as a model
                // parameter. It is verified neutral, never a guessed input.
                internal_offsets.insert(p.id);
            }
        }
        let points: BTreeMap<_, _> = self.recover_collection("sketch points", self.sketch_points())?.into_iter().map(|p| (p.id, p)).collect();
        let circles: BTreeMap<_, _> = self.recover_collection("sketch circles", self.sketch_circles())?.into_iter().map(|c| (c.id, c)).collect();
        let arcs: BTreeMap<_, _> = self.recover_collection("sketch arcs", self.sketch_arcs())?.into_iter().map(|a| (a.circle.id, a)).collect();
        let lines: BTreeMap<_, _> = self.recover_collection("sketch lines", self.sketch_lines())?.into_iter().map(|l| (l.id, l)).collect();
        let sketch_graphs: BTreeMap<_, _> =
            self.recover_collection("sketch attachment graphs", self.sketch_constraint_graphs())?.into_iter().map(|g| (g.geometry, g)).collect();
        let constraints: BTreeMap<_, _> =
            self.recover_collection("sketch constraints", self.sketch_constraints())?.into_iter().map(|c| (c.geometry, c)).collect();
        if let Err(error) =
            super::offset_import::metadata(&projections::Geometry { points: &points, lines: &lines, circles: &circles, arcs: &arcs }, &constraints)
        {
            if self.recovering() {
                self.warn(format!("Some sketch entity relationships could not be reconstructed: {error}"));
            } else {
                return Err(error);
            }
        }
        let profiles: BTreeMap<_, _> = self.recover_collection("sketch profiles", self.sketch_profiles())?.into_iter().map(|p| (p.id, p)).collect();
        let mut doc = Document::new(name);
        let mut names = BTreeSet::new();
        for p in &parameters {
            if internal_offsets.contains(&p.id) {
                continue;
            }
            if solidcraft_doc::expr::unit_info(&p.display_unit).is_none() {
                return Err(invalid(&format!("unsupported parameter unit: {}", p.display_unit)));
            }
            if p.name.is_empty() || !names.insert(p.name.clone()) {
                return Err(invalid("duplicate or empty parameter name"));
            }
            if p.favorite {
                doc.favorites.insert(p.name.clone());
            }
            doc.params.push(Param {
                name: p.name.clone(),
                expr: p.expression.clone(),
                unit: p.display_unit.clone(),
                comment: p.comment.clone(),
                model: p.binding.is_some(),
            });
        }
        let mut sketches = BTreeMap::new();
        let mut planes: BTreeMap<u64, (String, Plane)> = BTreeMap::new();
        let mut active_bodies = Vec::new();
        let mut body_number = 0;
        let mut feature_ids = BTreeMap::new();
        let mut source_profiles = BTreeMap::new();
        let mut operation_owners = None;
        let mut projection_model = solidcraft_doc::Model::new();
        for f in history {
            let native_id = f.id;
            let native_name = f.name.clone();
            let snapshot = self.recovering().then(|| {
                (doc.clone(), sketches.clone(), planes.clone(), active_bodies.clone(), body_number, feature_ids.clone(), source_profiles.clone())
            });
            let outcome = (|| -> Result<()> {
                if f.dependencies.iter().any(|id| omitted.contains(id)) {
                    return Err(invalid("depends on an omitted feature"));
                }
                let mut new_body = None;
                let mut new_plane = None;
                let mut new_point = None;
                let mut new_edge_names = Vec::new();
                let mut pending_profiles = BTreeMap::new();
                let mut import_face = |object| -> Result<(solidcraft_doc::FaceAnchor, Plane)> {
                    let face = super::loft_faces::face(&records, &classes, object, f.id)?;
                    if operation_owners.is_none() {
                        operation_owners = Some(self.operation_owners()?);
                    }
                    let owners = operation_owners.as_ref().ok_or_else(|| invalid("missing loft face operations"))?;
                    let id = *feature_ids.get(&face.feature).ok_or_else(|| invalid("missing imported loft body creator"))?;
                    let creator = doc.feature(id).ok_or_else(|| invalid("missing imported loft body creator"))?;
                    let [body_name] = creator.body_names.as_slice() else { return Err(invalid("loft face requires one created body")) };
                    let mut contributions = Vec::new();
                    let mut cap_roles = BTreeMap::new();
                    let mut producers = BTreeSet::new();
                    for operation in &face.all_operations {
                        let native = *owners.get(operation).ok_or_else(|| invalid("missing loft face operation owner"))?;
                        let imported = *feature_ids.get(&native).ok_or_else(|| invalid("loft face operation follows its selector"))?;
                        let current = doc.feature(imported).filter(|f| !f.suppressed).ok_or_else(|| invalid("loft face operation is suppressed"))?;
                        if face.boundary_operations.contains(operation) {
                            let [producer_operation] = face.contributions.as_slice() else {
                                return Err(invalid("trimmed cap requires one producer"));
                            };
                            let producer_native = owners.get(producer_operation).ok_or_else(|| invalid("missing trimmed cap producer"))?;
                            let producer_id = *feature_ids.get(producer_native).ok_or_else(|| invalid("missing imported trimmed cap producer"))?;
                            let source = *source_profiles.get(producer_native).ok_or_else(|| invalid("missing trimmed cap profile"))?;
                            face.validate_profile(source, &circles, &lines)?;
                            let circle = source
                                .regions
                                .first()
                                .and_then(|r| r.first())
                                .and_then(|b| b.curves.first())
                                .ok_or_else(|| invalid("missing trimmed cap circle"))?;
                            let producer = doc.feature(producer_id).ok_or_else(|| invalid("missing trimmed cap extrusion"))?;
                            let FeatureKind::Extrude { extent, .. } = &producer.kind else {
                                return Err(invalid("trimmed cap producer is not an extrusion"));
                            };
                            let end = if !face.high {
                                "start"
                            } else if doc.eval(&extent.distance, Kind::Length)? < 0. {
                                "bottom"
                            } else {
                                "top"
                            };
                            let expected = BTreeSet::from([format!("F{producer_id}:{end}"), format!("F{producer_id}:side:c{circle}")]);
                            if !matches!(&current.kind, FeatureKind::Chamfer { body: Some(body), groups, .. } if body == body_name && groups.is_empty())
                                || current.edge_names.is_empty()
                                || current.edge_names.iter().any(|name| {
                                    solidcraft_doc::naming::strip_edge_piece(name).split('|').map(str::to_owned).collect::<BTreeSet<_>>() != expected
                                })
                            {
                                return Err(invalid("trimmed cap boundary does not identify its circular chamfer"));
                            }
                            continue;
                        }
                        if native != face.feature
                            && !matches!(&current.kind, FeatureKind::Extrude { operation, targets, .. }
                                if targets.as_slice() == [body_name.clone()]
                                    && (*operation == Operation::Join
                                        || (*operation == Operation::Cut && !face.bounded && face.side.is_none())))
                        {
                            return Err(invalid("loft face contributor belongs to another body or operation"));
                        }
                        if face.contributions.contains(operation) {
                            // Unbounded cap recipes identify the certified support
                            // plane, including caps produced from another body face.
                            // They do not require a native sketch-profile record.
                            let source = source_profiles.get(&native).copied();
                            if face.bounded || face.side.is_some() {
                                face.validate_profile(source.ok_or_else(|| invalid("missing loft face contributor profile"))?, &circles, &lines)?;
                            }
                            if !producers.insert(imported) {
                                return Err(invalid("duplicate loft face contribution producer"));
                            }
                            let curve = face
                                .side_roles
                                .get(operation)
                                .copied()
                                .map(|label| {
                                    let source = source.ok_or_else(|| invalid("missing loft side contributor profile"))?;
                                    super::side_roles::curve(&records, &classes, native, label, source, &lines).map(|id| format!("c{id}"))
                                })
                                .transpose()?;
                            contributions.push((imported, curve));
                            if let Some(end) = face.cap_roles.get(operation) {
                                cap_roles.insert(imported, *end);
                            }
                        }
                    }
                    if face.side.is_none() && contributions.len() != 1 && cap_roles.len() != contributions.len() {
                        return Err(invalid("composite cap sections are not supported yet"));
                    }
                    projection_model.evaluate(&doc);
                    if let Some((result, error)) = projection_model.results.iter().find_map(|r| r.error.as_ref().map(|error| (r, error))) {
                        let name = doc.feature(result.id).map(|f| f.name.as_str()).unwrap_or("unknown");
                        return Err(invalid(&format!("cannot regenerate face dependency {} ({name}): {error}", result.id)));
                    }
                    let anchor = if cap_roles.len() > 1 {
                        super::loft_faces::composite_cap_anchor(&doc, &projection_model.state(), id, &cap_roles)?
                    } else {
                        face.anchor(&doc, &projection_model.state(), id, &contributions)?
                    };
                    let plane = doc.resolve_face_anchor(&anchor, 0, &projection_model.state())?;
                    Ok((anchor, plane))
                };
                let import_edges = |objects: &[u64],
                                    intersection: Option<Vec3>,
                                    native_flip: Option<bool>,
                                    body: &mut Option<String>,
                                    edges: &mut Vec<Vec3>,
                                    operation_owners: &mut Option<BTreeMap<u64, u64>>,
                                    projection_model: &mut solidcraft_doc::Model,
                                    new_edge_names: &mut Vec<String>|
                 -> Result<(usize, bool)> {
                    if operation_owners.is_none() {
                        *operation_owners = Some(self.operation_owners()?);
                    }
                    let owners = operation_owners.as_ref().ok_or_else(|| invalid("missing blend operations"))?;
                    projection_model.evaluate(&doc);
                    if let Some((result, error)) = projection_model.results.iter().find_map(|r| r.error.as_ref().map(|error| (r, error))) {
                        let name = doc.feature(result.id).map(|f| f.name.as_str()).unwrap_or("unknown");
                        return Err(invalid(&format!("cannot regenerate blend dependency {} ({name}): {error}", result.id)));
                    }
                    let state = projection_model.state();
                    let count = objects.len();
                    let initial_edges = edges.len();
                    if !(1..=64).contains(&count) || edges.len() + count > 1000 {
                        return Err(invalid("invalid blend edge count"));
                    }
                    if count > 1 && intersection.is_none() && native_flip.is_none() {
                        let context = super::composite_blends::Context {
                            records: &records,
                            classes: &classes,
                            operations: owners,
                            features: &feature_ids,
                            profiles: &source_profiles,
                            lines: &lines,
                            circles: &circles,
                            doc: &doc,
                            state: &state,
                            model: projection_model,
                            before: None,
                            depth: 0,
                        };
                        if let Ok(selected) = context.resolve_group(objects, f.id) {
                            if selected.is_empty() || edges.len() + selected.len() > 1000 {
                                return Err(invalid("invalid reconstructed blend group size"));
                            }
                            for selection in &selected {
                                if !active_bodies.contains(&selection.body)
                                    || body.as_ref().is_some_and(|body| body != &selection.body)
                                    || new_edge_names.contains(&selection.name)
                                {
                                    return Err(invalid("blend group has an unavailable body or duplicate edge"));
                                }
                            }
                            let count = selected.len();
                            for selection in selected {
                                *body = Some(selection.body);
                                edges.push(selection.point);
                                new_edge_names.push(selection.name);
                            }
                            return Ok((count, false));
                        }
                    }
                    let mut resolved_flip = None;
                    for object in objects {
                        // A joined cap can carry contributions from several
                        // extrusions. Resolve its complete incidence graph
                        // before the single-producer/circular legacy path.
                        let joined = if intersection.is_none() {
                            let context = super::composite_blends::Context {
                                records: &records,
                                classes: &classes,
                                operations: owners,
                                features: &feature_ids,
                                profiles: &source_profiles,
                                lines: &lines,
                                circles: &circles,
                                doc: &doc,
                                state: &state,
                                model: projection_model,
                                before: None,
                                depth: 0,
                            };
                            if let Some(flip) = native_flip {
                                context.resolve_asymmetric(*object, f.id, flip).map(|(selected, flip)| (selected, Some(flip)))
                            } else {
                                context.resolve(*object, f.id).map(|selected| (selected, None))
                            }
                        } else {
                            Err(invalid("joined blend selection mode is unsupported"))
                        };
                        let joined_error = match joined {
                            Ok((selected, flip)) => {
                                if let Some(flip) = flip {
                                    if resolved_flip.is_some_and(|old| old != flip) {
                                        return Err(invalid("mixed native chamfer face orientations within one group are not supported yet"));
                                    }
                                    resolved_flip = Some(flip);
                                }
                                if selected.is_empty() || edges.len() + selected.len() > 1000 {
                                    return Err(invalid("invalid reconstructed blend edge count"));
                                }
                                for selected in selected {
                                    if !active_bodies.contains(&selected.body) || body.as_ref().is_some_and(|body| body != &selected.body) {
                                        return Err(invalid("joined blend edges belong to different or unavailable bodies"));
                                    }
                                    if new_edge_names.contains(&selected.name) {
                                        return Err(invalid("duplicate joined blend edge selection"));
                                    }
                                    *body = Some(selected.body);
                                    edges.push(selected.point);
                                    new_edge_names.push(selected.name);
                                }
                                continue;
                            }
                            Err(error) => error,
                        };
                        let edge = if intersection.is_some() {
                            super::fillets::edge_with_mode(&records, &classes, *object, f.id, true)?
                        } else {
                            super::fillets::edge(&records, &classes, *object, f.id)
                                .map_err(|error| invalid(&format!("{error}; joined blend: {joined_error}")))?
                        };
                        if owners.get(&edge.operation) != Some(&edge.feature) {
                            return Err(invalid("blend edge operation belongs to another producer"));
                        }
                        let source_id = *feature_ids.get(&edge.feature).ok_or_else(|| invalid("blend precedes its source"))?;
                        let source = doc.feature(source_id).filter(|f| !f.suppressed).ok_or_else(|| invalid("blend source is suppressed"))?;
                        let FeatureKind::Extrude { extent, sketch, .. } = &source.kind else {
                            return Err(invalid("unsupported blend source feature"));
                        };
                        let distance = doc.eval(&extent.distance, Kind::Length)?;
                        if extent.direction != solidcraft_doc::Direction::Positive
                            || extent.start_offset.is_some()
                            || extent.taper.as_ref().map(|e| doc.eval(e, Kind::Angle)).transpose()?.is_some_and(|a| a.abs() > 1e-12)
                            || distance.abs() <= 1e-12
                        {
                            return Err(invalid("unsupported blend source extent"));
                        }
                        let [name] = source.body_names.as_slice() else { return Err(invalid("blend requires one source body")) };
                        if !active_bodies.contains(name) || body.as_ref().is_some_and(|body| body != name) {
                            return Err(invalid("blend edges belong to different or unavailable bodies"));
                        }
                        *body = Some(name.clone());
                        let target =
                            state.body(name).filter(|b| b.feature == source_id).ok_or_else(|| invalid("blend source body is unavailable"))?;
                        let profile = *source_profiles.get(&edge.feature).ok_or_else(|| invalid("missing blend source profile"))?;
                        let mut face_names = BTreeMap::new();
                        for label in &edge.labels {
                            let name = match label {
                                1 => format!("F{source_id}:start"),
                                // Native label 2 is the travel cap for either
                                // sign; our cap role is relative to the sketch.
                                2 => format!("F{source_id}:{}", if distance < 0. { "bottom" } else { "top" }),
                                _ => {
                                    let solved = state.sketch(sketch.0).ok_or_else(|| invalid("missing blend source sketch"))?;
                                    let resolver = if edge.closed { super::blend_sides::circle } else { super::blend_sides::curve };
                                    let curve = resolver(
                                        &records,
                                        &classes,
                                        edge.feature,
                                        u64::from(*label),
                                        super::blend_sides::Source { profile, lines: &lines, circles: &circles, solved },
                                    )?;
                                    format!("F{source_id}:side:c{curve}")
                                }
                            };
                            face_names.insert(*label, name);
                        }
                        if edge.closed
                            && let Some(native_flip) = native_flip
                        {
                            // The oriented native recipe identifies the primary
                            // setback face. Complete circular caps use the kernel's
                            // cap-first convention, independent of world placement.
                            let flip = (super::chamfers::primary_face(&edge)? == 3) ^ native_flip;
                            if resolved_flip.is_some_and(|old| old != flip) {
                                return Err(invalid("mixed native circular chamfer orientations within one group"));
                            }
                            resolved_flip = Some(flip);
                        } else if let Some(native_flip) = native_flip {
                            let primary = super::chamfers::primary_face(&edge)?;
                            let names = solidcraft_doc::naming::face_names(target);
                            let mut normals = Vec::new();
                            for label in &edge.faces {
                                let expected = face_names.get(label).ok_or_else(|| invalid("missing chamfer side identity"))?;
                                let matches: Vec<_> =
                                    names.iter().enumerate().filter(|(_, name)| solidcraft_doc::naming::strip_piece(name) == expected).collect();
                                let [(index, _)] = matches.as_slice() else {
                                    return Err(invalid("ambiguous chamfer side identity"));
                                };
                                normals.push((
                                    *label,
                                    target.body.planar_face(*index).ok_or_else(|| invalid("nonplanar asymmetric chamfer face"))?.normal(),
                                ));
                            }
                            let [(a, na), (_, nb)] = normals.as_slice() else {
                                return Err(invalid("missing chamfer side planes"));
                            };
                            if na.dot(*nb).abs() > 1e-8 {
                                return Err(invalid("nonorthogonal native asymmetric chamfers are not supported yet"));
                            }
                            let up = Vec3::new(1e-6, 1e-3, 1.);
                            let score = na.dot(up) - nb.dot(up);
                            if score.abs() < 1e-12 {
                                return Err(invalid("ambiguous asymmetric chamfer face ranking"));
                            }
                            let flip = ((primary == *a) != (score > 0.)) ^ native_flip;
                            if resolved_flip.is_some_and(|old| old != flip) {
                                return Err(invalid("mixed native chamfer face orientations within one group are not supported yet"));
                            }
                            resolved_flip = Some(flip);
                        }
                        let adjacent = edge
                            .faces
                            .iter()
                            .map(|label| face_names.get(label).map(String::as_str).ok_or_else(|| invalid("missing blend side")))
                            .collect::<Result<BTreeSet<_>>>()?;
                        let selected: Vec<_> = solidcraft_doc::naming::edge_names(target)
                            .into_iter()
                            .enumerate()
                            .filter(|(_, name)| name.split('|').map(solidcraft_doc::naming::strip_piece).collect::<BTreeSet<_>>() == adjacent)
                            .collect();
                        if edge.closed {
                            if selected.is_empty() || selected.len() > 64 || edges.len() + selected.len() > 1000 {
                                return Err(invalid("missing or excessive circular blend boundary"));
                            }
                            let plane = state.sketch(sketch.0).ok_or_else(|| invalid("missing circular blend sketch"))?.plane;
                            let names: Vec<_> = selected.iter().map(|(_, name)| name.clone()).collect();
                            solidcraft_doc::project::resolve_edge_circle(&state, &plane, name, &names)?;
                            let mesh = target.mesh();
                            for (index, selected) in selected {
                                if new_edge_names.contains(&selected) {
                                    return Err(invalid("duplicate circular blend edge selection"));
                                }
                                let points =
                                    mesh.edges.get(index).filter(|p| p.len() >= 3).ok_or_else(|| invalid("missing circular blend samples"))?;
                                let point = *points.get(points.len() / 2).ok_or_else(|| invalid("missing circular blend sample"))?;
                                edges.push(point);
                                new_edge_names.push(selected);
                            }
                            continue;
                        }
                        let [(index, selected)] = selected.as_slice() else {
                            return Err(invalid("blend edge incidence is missing or ambiguous"));
                        };
                        if new_edge_names.contains(selected) {
                            return Err(invalid("duplicate blend edge selection"));
                        }
                        // The current supported profile-line extrusion edges are
                        // straight. Pick their interior: a shared vertex is also
                        // on neighboring edges and is ambiguous to the kernel.
                        let mesh = target.mesh();
                        let edge = mesh.edges.get(*index).ok_or_else(|| invalid("missing regenerated blend edge"))?;
                        let start = edge.first().ok_or_else(|| invalid("empty regenerated blend edge"))?;
                        let end = edge.last().ok_or_else(|| invalid("empty regenerated blend edge"))?;
                        if start.dist(*end) < 1e-8 {
                            return Err(invalid("degenerate regenerated blend edge"));
                        }
                        if let Some(point) = intersection {
                            super::construction_points::check_line(edge, point)?;
                        }
                        edges.push((*start + *end) * 0.5);
                        new_edge_names.push(selected.clone());
                    }
                    Ok((edges.len() - initial_edges, resolved_flip.unwrap_or(false)))
                };
                let mut kind = match f.kind {
                    HistoryKind::TwoEdgePoint { edges, position, .. } => {
                        use solidcraft_doc::construct::{GeoRef, PointDef};
                        let mut sources = Vec::new();
                        let mut body = None;
                        let mut picks = Vec::new();
                        let mut names = Vec::new();
                        for object in edges {
                            let source = if classes.get(&object).is_some_and(|c| c.id == "5A1BF548-241F-46FD-9FB5-E4B05126EB9D") {
                                let (geometry, tag) = super::construction_points::sketch_edge(&records, &classes, object, f.id)?;
                                let (sketch, _) =
                                    sketches.get(&geometry).copied().ok_or_else(|| invalid("construction point precedes its sketch"))?;
                                let matching: Vec<_> =
                                    lines.values().filter(|l| l.sketch == geometry && l.profile_id.is_some_and(|(id, _)| id == tag)).collect();
                                let [line] = matching.as_slice() else {
                                    return Err(invalid("missing or ambiguous construction-point sketch line"));
                                };
                                let curve = format!("c{}", line.id);
                                projection_model.evaluate(&doc);
                                if projection_model.results.iter().any(|r| r.error.is_some()) {
                                    return Err(invalid("cannot regenerate construction-point sources"));
                                }
                                let state = projection_model.state();
                                let solved = state.sketch(sketch).ok_or_else(|| invalid("missing regenerated construction-point sketch"))?;
                                let index =
                                    solved.sketch.curve_index(&curve).ok_or_else(|| invalid("missing regenerated construction-point line"))?;
                                let points: Vec<_> = solved.sketch.polyline(index).into_iter().map(|p| solved.plane.to_world(p)).collect();
                                super::construction_points::check_line(&points, position)?;
                                GeoRef::SketchCurve { sketch, curve }
                            } else {
                                import_edges(
                                    &[object],
                                    Some(position),
                                    None,
                                    &mut body,
                                    &mut picks,
                                    &mut operation_owners,
                                    &mut projection_model,
                                    &mut names,
                                )?;
                                GeoRef::Edge {
                                    at: *picks.last().ok_or_else(|| invalid("missing construction-point edge"))?,
                                    name: Some(names.last().ok_or_else(|| invalid("missing construction-point edge name"))?.clone()),
                                }
                            };
                            if sources.contains(&source) {
                                return Err(invalid("duplicate construction-point source"));
                            }
                            sources.push(source);
                        }
                        let [a, b] = sources.as_slice() else {
                            return Err(invalid("missing construction-point sources"));
                        };
                        new_point = Some(position);
                        FeatureKind::ConstructionPoint { def: PointDef::TwoEdges { a: a.clone(), b: b.clone() } }
                    }
                    HistoryKind::Chamfer { sets } => {
                        let mut body = None;
                        let mut edges = Vec::new();
                        let mut groups = Vec::new();
                        let mut dimensions = BTreeSet::new();
                        for set in &sets {
                            let parameter = |binding, label, kind| -> Result<String> {
                                let p = bindings.get(&binding).ok_or_else(|| invalid("missing chamfer parameter"))?;
                                if p.owner != Some(f.id) || p.label != label || Kind::from_unit(&p.display_unit) != kind {
                                    return Err(invalid("invalid chamfer parameter role or ownership"));
                                }
                                Ok(p.name.clone())
                            };
                            let distance = parameter(set.distance, if set.distance2.is_some() { "Distance 1" } else { "Distance" }, Kind::Length)?;
                            let distance2 = set.distance2.map(|b| parameter(b, "Distance 2", Kind::Length)).transpose()?;
                            let angle = set.angle.map(|b| parameter(b, "Rotate Angle", Kind::Angle)).transpose()?;
                            for binding in [Some(set.distance), set.distance2, set.angle].into_iter().flatten() {
                                if !dimensions.insert(binding) {
                                    return Err(invalid("chamfer groups reuse a dimension binding"));
                                }
                            }
                            let asymmetric = distance2.is_some() || angle.is_some();
                            let (count, flip) = import_edges(
                                &super::loft_faces::objects(&records, &classes, set.container, f.id)?,
                                None,
                                asymmetric.then_some(set.flip),
                                &mut body,
                                &mut edges,
                                &mut operation_owners,
                                &mut projection_model,
                                &mut new_edge_names,
                            )?;
                            groups.push(solidcraft_doc::ChamferGroup {
                                count,
                                distance: (distance).into(),
                                distance2: distance2.map(Into::into),
                                angle: angle.map(Into::into),
                                flip,
                                tangent_chain: set.tangent_chain,
                            });
                        }
                        if groups.is_empty() {
                            return Err(invalid("missing chamfer group"));
                        }
                        let first = groups.remove(0);
                        FeatureKind::Chamfer {
                            edges: edges.into_iter().map(Into::into).collect(),
                            distance: first.distance,
                            distance2: first.distance2,
                            angle: first.angle,
                            flip: first.flip,
                            tangent_chain: first.tangent_chain,
                            groups,
                            body: body.map(Into::into),
                        }
                    }
                    HistoryKind::Fillet { sets } => {
                        if sets.iter().any(|set| set.face_container.is_some()) {
                            return Err(invalid("fillet face selections require live boundary reconstruction"));
                        }
                        let mut body = None;
                        let mut edges = Vec::new();
                        let mut primary_radius = None;
                        let mut groups = Vec::new();
                        let mut radius_bindings = BTreeSet::new();
                        for (index, set) in sets.iter().enumerate() {
                            let radius = length_parameter(&bindings, set.radius)?;
                            let weight = bindings.get(&set.weight).ok_or_else(|| invalid("missing fillet tangency weight"))?;
                            if radius.owner != Some(f.id)
                                || radius.label != "Radius"
                                || weight.owner != Some(f.id)
                                || weight.label != "TangencyWeight"
                                || weight.value != 1.
                            {
                                return Err(invalid("unsupported fillet parameter ownership or tangency"));
                            }
                            if !radius_bindings.insert(set.radius) {
                                return Err(invalid("fillet groups reuse a radius binding"));
                            }
                            let (count, _) = import_edges(
                                &super::loft_faces::objects(
                                    &records,
                                    &classes,
                                    set.container.ok_or_else(|| invalid("missing fillet edge selection"))?,
                                    f.id,
                                )?,
                                None,
                                None,
                                &mut body,
                                &mut edges,
                                &mut operation_owners,
                                &mut projection_model,
                                &mut new_edge_names,
                            )?;
                            if index == 0 {
                                primary_radius = Some(radius.name.clone());
                            } else {
                                groups.push(solidcraft_doc::FilletRadiusGroup {
                                    count,
                                    radius: (radius.name.clone()).into(),
                                    tangent_chain: set.tangent_chain,
                                });
                            }
                        }
                        FeatureKind::Fillet {
                            edges: edges.into_iter().map(Into::into).collect(),
                            radius: (primary_radius.ok_or_else(|| invalid("missing first fillet radius"))?).into(),
                            body: body.map(Into::into),
                            style: if groups.is_empty() {
                                solidcraft_doc::FilletStyle::Constant
                            } else {
                                solidcraft_doc::FilletStyle::Grouped { groups }
                            },
                            fit: solidcraft_doc::FilletFit::Exact,
                            corner: Default::default(),
                            tangent_chain: sets.first().is_some_and(|s| s.tangent_chain),
                        }
                    }
                    HistoryKind::Combine { operation, keep_tools, operation_tag, tool_containers, target_container, consumed_handles } => {
                        if operation_owners.is_none() {
                            operation_owners = Some(self.operation_owners()?);
                        }
                        let owners = operation_owners.as_ref().ok_or_else(|| invalid("missing Combine operations"))?;
                        if owners.get(&operation_tag) != Some(&f.id) {
                            return Err(invalid("Combine operation belongs to another feature"));
                        }
                        let target_objects = super::combines::objects(&records, &classes, target_container, f.id, None)?;
                        let [target_object] = target_objects.as_slice() else { return Err(invalid("Combine requires one target body")) };
                        let mut selected_handles = BTreeSet::new();
                        let mut body = |object| -> Result<Option<(String, u64)>> {
                            let (mut selected, handle) = super::loft_faces::combine_target(&records, &classes, object, f.id)
                                .map_err(|e| invalid(&format!("Combine selector {object}: {e}")))?;
                            let operation = selected.operations.first().ok_or_else(|| invalid("missing Combine source operation"))?;
                            if handle.is_none() {
                                selected.feature = *owners.get(&operation.operation).ok_or_else(|| invalid("missing retired pattern owner"))?;
                                if !classes.get(&selected.feature).is_some_and(|c| c.id == super::circular_patterns::CLASS && c.version == 0) {
                                    return Err(invalid("null Combine source is not a circular pattern"));
                                }
                            }
                            if owners.get(&operation.operation) != Some(&selected.feature)
                                || handle.is_some_and(|handle| !selected_handles.insert(handle))
                            {
                                return Err(invalid("duplicate or inconsistent Combine source identity"));
                            }
                            let source = feature_ids
                                .get(&selected.feature)
                                .and_then(|id| doc.feature(*id))
                                .filter(|f| !f.suppressed)
                                .ok_or_else(|| invalid("Combine source follows its selector or is suppressed"))?;
                            let patterned = classes.get(&selected.feature).is_some_and(|c| c.id == super::circular_patterns::CLASS);
                            let body = if patterned {
                                let output = super::circular_patterns::body_output(
                                    &records,
                                    &classes,
                                    owners,
                                    selected.feature,
                                    handle,
                                    operation.operation,
                                    u64::from(operation.label),
                                )?;
                                let seed = feature_ids
                                    .get(&output.source_feature)
                                    .and_then(|id| doc.feature(*id))
                                    .ok_or_else(|| invalid("missing pattern seed feature"))?;
                                let [seed_body] = seed.body_names.as_slice() else {
                                    return Err(invalid("ambiguous pattern seed body"));
                                };
                                for tag in &output.contributors {
                                    let modifier = owners
                                        .get(tag)
                                        .and_then(|native| feature_ids.get(native))
                                        .and_then(|id| doc.feature(*id))
                                        .filter(|modifier| {
                                            !modifier.suppressed
                                                && doc.features.iter().position(|f| f.id == modifier.id)
                                                    < doc.features.iter().position(|f| f.id == source.id)
                                        })
                                        .ok_or_else(|| invalid("pattern contributor is missing, suppressed or follows the pattern"))?;
                                    if !matches!(&modifier.kind, FeatureKind::Fillet { body: Some(body), .. } | FeatureKind::Chamfer { body: Some(body), .. } if body == seed_body)
                                    {
                                        return Err(invalid("pattern contributor does not modify the selected seed body"));
                                    }
                                }
                                let Some(index) = output.index else {
                                    super::body_labels::validate_aliases(
                                        &records,
                                        &classes,
                                        selected.feature,
                                        u64::from(operation.label),
                                        &operation.aliases,
                                    )?;
                                    return Ok(None);
                                };
                                source.body_names.get(index).ok_or_else(|| invalid("missing generated pattern body"))?
                            } else {
                                let [body] = source.body_names.as_slice() else { return Err(invalid("Combine source requires one produced body")) };
                                body
                            };
                            let handle = handle.ok_or_else(|| invalid("missing live Combine body handle"))?;
                            if !active_bodies.contains(body) {
                                return Err(invalid("Combine source is no longer available"));
                            }
                            for (i, operation) in selected.operations.iter().enumerate() {
                                let native = *owners.get(&operation.operation).ok_or_else(|| invalid("missing Combine contributor owner"))?;
                                let contributor = feature_ids
                                    .get(&native)
                                    .and_then(|id| doc.feature(*id))
                                    .filter(|f| !f.suppressed)
                                    .ok_or_else(|| invalid("Combine contributor follows its selector or is suppressed"))?;
                                if i > 0
                                    && !(operation.flags == 4
                                        && matches!(&contributor.kind,
                                FeatureKind::Extrude { operation: Operation::Join, targets, .. } if targets.as_slice() == [body.clone()]))
                                {
                                    return Err(invalid("Combine contributor belongs to another body or operation"));
                                }
                                super::body_labels::validate_aliases(&records, &classes, native, u64::from(operation.label), &operation.aliases)?;
                                if operation.flags == 3 && patterned {
                                    // Source, instance and handle were certified when selecting this body.
                                } else if operation.flags == 3 {
                                    let groups = super::mirrors::output_operations(&records, &classes, owners, native, handle, operation.operation)?;
                                    super::body_labels::validate_mirror(&records, &classes, native, u64::from(operation.label), handle, &groups)?;
                                } else {
                                    super::body_labels::validate(&records, &classes, native, operation.operation, u64::from(operation.label))?;
                                }
                            }
                            Ok(Some((body.clone(), handle)))
                        };
                        let (target, _) = body(*target_object)?.ok_or_else(|| invalid("Combine target is retired"))?;
                        let mut tools = Vec::new();
                        let mut tool_handles = BTreeSet::new();
                        for container in tool_containers {
                            for object in super::combines::objects(&records, &classes, container, f.id, Some(operation_tag))? {
                                let Some((tool, handle)) = body(object)? else { continue };
                                if tools.len() >= 64 || tools.contains(&tool) || tool == target {
                                    return Err(invalid("duplicate or excessive Combine tools"));
                                }
                                tools.push(tool);
                                tool_handles.insert(handle);
                            }
                        }
                        if !keep_tools && tool_handles != consumed_handles.into_iter().collect() {
                            return Err(invalid("Combine consumed handles disagree with selected tools"));
                        }
                        if !f.suppressed && !keep_tools {
                            active_bodies.retain(|body| !tools.contains(body));
                        }
                        FeatureKind::Combine { target: (target).into(), tools: tools.into_iter().map(Into::into).collect(), operation, keep_tools }
                    }
                    HistoryKind::CircularPattern(pattern) => {
                        let axis = super::circular_patterns::origin_axis(&records, &classes, pattern.axis, root)?;
                        let count = bindings
                            .get(&pattern.count)
                            .filter(|p| p.owner == Some(f.id) && p.label == "countU" && Kind::from_unit(&p.display_unit) == Kind::Unitless)
                            .ok_or_else(|| invalid("missing circular-pattern count binding"))?;
                        let angle = bindings
                            .get(&pattern.angle)
                            .filter(|p| p.owner == Some(f.id) && p.label == "TotalAngle" && Kind::from_unit(&p.display_unit) == Kind::Angle)
                            .ok_or_else(|| invalid("missing circular-pattern angle binding"))?;
                        if count.value != pattern.instances as f64 {
                            return Err(invalid("circular-pattern count disagrees with its instances"));
                        }
                        let mut targets = Vec::new();
                        for target in &pattern.targets {
                            let source = feature_ids
                                .get(target)
                                .and_then(|id| doc.feature(*id))
                                .filter(|f| !f.suppressed)
                                .ok_or_else(|| invalid("circular-pattern target is unavailable"))?;
                            let [body] = source.body_names.as_slice() else { return Err(invalid("circular-pattern target requires one body")) };
                            if !active_bodies.contains(body) || targets.contains(body) {
                                return Err(invalid("missing or duplicate circular-pattern target"));
                            }
                            targets.push(body.clone());
                        }
                        let mut features = Vec::new();
                        let mut bodies = Vec::new();
                        if pattern.features.is_empty() {
                            if operation_owners.is_none() {
                                operation_owners = Some(self.operation_owners()?);
                            }
                            let owners = operation_owners.as_ref().ok_or_else(|| invalid("missing circular-pattern operations"))?;
                            for object in super::loft_faces::objects(&records, &classes, pattern.source_container, f.id)? {
                                let target = super::loft_faces::target_object(&records, &classes, object, f.id)?;
                                let [operation] = target.operations.as_slice() else {
                                    return Err(invalid("composite circular-pattern source is not supported yet"));
                                };
                                if owners.get(&operation.operation) != Some(&target.feature) {
                                    return Err(invalid("circular-pattern recipe disagrees with its source"));
                                }
                                super::body_labels::validate(&records, &classes, target.feature, operation.operation, u64::from(operation.label))?;
                                let source = feature_ids
                                    .get(&target.feature)
                                    .and_then(|id| doc.feature(*id))
                                    .filter(|f| !f.suppressed)
                                    .ok_or_else(|| invalid("circular-pattern source follows its selector or is suppressed"))?;
                                let [body] = source.body_names.as_slice() else {
                                    return Err(invalid("circular-pattern source requires one produced body"));
                                };
                                if !active_bodies.contains(body) || bodies.contains(body) {
                                    return Err(invalid("missing or duplicate circular-pattern source body"));
                                }
                                bodies.push(body.clone());
                            }
                        } else {
                            for source in &pattern.features {
                                let source = feature_ids
                                    .get(source)
                                    .and_then(|id| doc.feature(*id))
                                    .filter(|f| !f.suppressed)
                                    .ok_or_else(|| invalid("circular-pattern feature follows its selector or is suppressed"))?;
                                let FeatureKind::Extrude { operation, targets: source_targets, .. } = &source.kind else {
                                    return Err(invalid("unsupported circular-pattern feature kind"));
                                };
                                if *operation != Operation::NewBody && *source_targets != targets {
                                    return Err(invalid("circular-pattern feature targets disagree with its operation"));
                                }
                                features.push(source.name.clone());
                            }
                        }
                        let mut outputs = Vec::new();
                        for _ in &pattern.outputs {
                            body_number += 1;
                            outputs.push(format!("Body{body_number}"));
                        }
                        if !f.suppressed {
                            active_bodies.extend(outputs.iter().cloned());
                        }
                        new_body = Some(outputs);
                        FeatureKind::Pattern {
                            features: features.into_iter().map(Into::into).collect(),
                            bodies: bodies.into_iter().map(Into::into).collect(),
                            pattern: solidcraft_doc::PatternKind::Circular {
                                origin: (Vec3::ZERO).into(),
                                axis: (axis).into(),
                                count: (count.name.clone()).into(),
                                angle: (angle.name.clone()).into(),
                                symmetric: pattern.symmetric,
                                alignment: Default::default(),
                                reversed: false,
                            },
                        }
                    }
                    HistoryKind::MirrorFeatures { plane, sources, targets, output_count } => {
                        let (plane, _) = plane_base(&records, &classes, &planes, plane, root)?;
                        let mut target_bodies = Vec::new();
                        for target in targets {
                            let source = feature_ids
                                .get(&target)
                                .and_then(|id| doc.feature(*id))
                                .filter(|f| !f.suppressed)
                                .ok_or_else(|| invalid("mirrored feature target is unavailable"))?;
                            let [body] = source.body_names.as_slice() else { return Err(invalid("mirrored feature target requires one body")) };
                            if !active_bodies.contains(body) || target_bodies.contains(body) {
                                return Err(invalid("missing or duplicate mirrored feature target"));
                            }
                            target_bodies.push(body.clone());
                        }
                        let mut features = Vec::new();
                        for source in sources {
                            let source = feature_ids
                                .get(&source)
                                .and_then(|id| doc.feature(*id))
                                .filter(|f| !f.suppressed)
                                .ok_or_else(|| invalid("mirrored feature must precede its mirror and be active"))?;
                            let FeatureKind::Extrude { operation, targets, .. } = &source.kind else {
                                return Err(invalid("unsupported mirrored feature kind"));
                            };
                            if *operation != Operation::NewBody && *targets != target_bodies {
                                return Err(invalid("mirrored feature target handles disagree with its operation"));
                            }
                            features.push(source.name.clone());
                        }
                        let mut outputs = Vec::new();
                        for _ in 0..output_count {
                            body_number += 1;
                            outputs.push(format!("Body{body_number}"));
                        }
                        if !f.suppressed {
                            active_bodies.extend(outputs.iter().cloned());
                        }
                        new_body = Some(outputs);
                        FeatureKind::Mirror { features: features.into_iter().map(Into::into).collect(), plane, bodies: vec![], combine: false }
                    }
                    HistoryKind::Mirror { plane, source_container, output_count } => {
                        let (plane, _) = plane_base(&records, &classes, &planes, plane, root)?;
                        let objects = super::loft_faces::objects(&records, &classes, source_container, f.id)?;
                        if objects.len() != output_count {
                            return Err(invalid("mirror output count disagrees with its sources"));
                        }
                        if operation_owners.is_none() {
                            operation_owners = Some(self.operation_owners()?);
                        }
                        let owners = operation_owners.as_ref().ok_or_else(|| invalid("missing mirror source operations"))?;
                        let mut bodies = Vec::new();
                        let mut outputs = Vec::new();
                        for object in objects {
                            let target = super::loft_faces::target_object(&records, &classes, object, f.id)?;
                            let [operation] = target.operations.as_slice() else {
                                return Err(invalid("mirror sources modified by later operations are not supported yet"));
                            };
                            if owners.get(&operation.operation) != Some(&target.feature) {
                                return Err(invalid("mirror source recipe disagrees with its body producer"));
                            }
                            super::body_labels::validate(&records, &classes, target.feature, operation.operation, u64::from(operation.label))?;
                            let source = feature_ids
                                .get(&target.feature)
                                .and_then(|id| doc.feature(*id))
                                .filter(|f| !f.suppressed)
                                .ok_or_else(|| invalid("mirror source follows its selector or is suppressed"))?;
                            let [body] = source.body_names.as_slice() else {
                                return Err(invalid("mirror source must identify one produced body"));
                            };
                            if !active_bodies.contains(body) || bodies.contains(body) {
                                return Err(invalid("missing or duplicate mirror source body"));
                            }
                            bodies.push(body.clone());
                            body_number += 1;
                            outputs.push(format!("Body{body_number}"));
                        }
                        if !f.suppressed {
                            active_bodies.extend(outputs.iter().cloned());
                        }
                        new_body = Some(outputs);
                        FeatureKind::Mirror { features: vec![], plane, bodies: bodies.into_iter().map(Into::into).collect(), combine: false }
                    }
                    HistoryKind::OffsetPlane { geometry, definition, extra_offset } => {
                        let definition = super::planes::definition(&records, &classes, definition, f.id)?;
                        if definition.extra != extra_offset {
                            return Err(invalid("inconsistent internal plane offset binding"));
                        }
                        let p = length_parameter(&bindings, definition.distance.ok_or_else(|| invalid("missing plane distance"))?)?;
                        if p.owner != Some(f.id) || p.label != "AlongDistance" {
                            return Err(invalid("plane distance belongs to another feature"));
                        }
                        let (base, cached) = plane_base(&records, &classes, &planes, definition.base, root)?;
                        let cached = cached.offset(p.value * 10.);
                        if !super::planes::same_surface(cached, definition.plane) {
                            return Err(invalid("plane definition disagrees with its dependencies"));
                        }
                        new_plane = Some((geometry, cached));
                        FeatureKind::ConstructionPlane { plane: PlaneRef::Offset { base: Box::new(base), distance: (p.name.clone()).into() } }
                    }
                    HistoryKind::Sketch { geometry, placement } => {
                        // Legacy v17 can repeat the complete membership with default
                        // state 1. Controlled local import/edit/resave checks preserve
                        // the same geometry and constraints with this table present.
                        // Partial or repeated entries have not been established.
                        if !placement.legacy_entity_metadata.is_empty() {
                            let ids: BTreeSet<_> = placement.legacy_entity_metadata.iter().map(|(id, _)| *id).collect();
                            let members: BTreeSet<_> = placement.entities.iter().copied().collect();
                            if ids.len() != placement.legacy_entity_metadata.len()
                                || ids != members
                                || placement.legacy_entity_metadata.iter().any(|(_, flags)| *flags != 1)
                            {
                                return Err(invalid("unsupported legacy sketch entity metadata membership or state"));
                            }
                        }
                        if placement.owner != root {
                            return Err(invalid("non-root sketch placement is not supported yet"));
                        }
                        let plane = plane(&placement.matrix)?;
                        let graph =
                            sketch_graphs.get(&geometry).filter(|g| g.feature == f.id).ok_or_else(|| invalid("missing sketch attachment graph"))?;
                        let plane_ref = if let Some(body) = graph.support {
                            let definition = super::planes::face_definition(&records, &classes, placement.support, f.id, body)?;
                            if !super::planes::same_support_plane(definition.plane, plane) {
                                return Err(invalid("sketch placement disagrees with its support definition"));
                            }
                            let (anchor, _) = import_face(definition.base)?;
                            super::planes::attach_sketch(&doc, &projection_model.state(), anchor, plane)?
                        } else if !has_offset_planes {
                            PlaneRef::Custom { plane }
                        } else {
                            let definition = super::planes::definition(&records, &classes, placement.support, f.id)?;
                            if definition.extra.is_some() || definition.distance.is_some() {
                                return Err(invalid("unsupported sketch support offset"));
                            }
                            let (base, cached) = plane_base(&records, &classes, &planes, definition.base, root)?;
                            if !super::planes::same_surface(cached, definition.plane) {
                                return Err(invalid("sketch support disagrees with its construction plane"));
                            }
                            sketch_plane_reference(base, cached, plane)?
                        };
                        let fallback = SketchConstraints { geometry, feature: f.id, constraints: Vec::new() };
                        let relations = match constraints.get(&geometry).filter(|c| c.feature == f.id) {
                            Some(relations) => relations,
                            None if self.recovering() => {
                                self.warn(format!("Sketch {}: constraints could not be recovered; retained decoded geometry", f.id));
                                &fallback
                            }
                            None => return Err(invalid("missing sketch relationships")),
                        };
                        let mut sketch = Sketch::new();
                        let mut point_indices = BTreeMap::new();
                        let mut curve_indices = BTreeMap::new();
                        let mut consumed = BTreeSet::new();
                        let native = projections::Geometry { points: &points, lines: &lines, circles: &circles, arcs: &arcs };
                        let mut projected_sources = BTreeMap::new();
                        let members: BTreeSet<_> = placement.entities.iter().copied().collect();
                        if members.len() != placement.entities.len() {
                            return Err(invalid("duplicate sketch entity"));
                        }
                        let projections =
                            placement.projections.iter().map(|id| projections::decode(&records, &classes, *id, f.id)).collect::<Result<Vec<_>>>()?;
                        for projection in projections.into_iter().flatten() {
                            let (entities, namespace) = match &projection.source {
                                projections::Source::Composite(boundary) => {
                                    if operation_owners.is_none() {
                                        operation_owners = Some(self.operation_owners()?);
                                    }
                                    projection_model.evaluate(&doc);
                                    if projection_model.results.iter().any(|r| r.error.is_some()) {
                                        return Err(invalid("cannot regenerate composite projection dependencies"));
                                    }
                                    let state = projection_model.state();
                                    let context = super::body_projections::Context {
                                        native: &native,
                                        profiles: &source_profiles,
                                        doc: &doc,
                                        model: &state,
                                        features: &feature_ids,
                                        operations: operation_owners.as_ref().ok_or_else(|| invalid("missing topology operation ownership"))?,
                                    };
                                    (boundary.attach(&context, &projection_model, geometry, plane, &plane_ref, &mut sketch)?, (3_u8, boundary.body))
                                }
                                projections::Source::Sketch { geometry: source_geometry, .. } => {
                                    let (source_feature, source_plane) =
                                        sketches.get(source_geometry).copied().ok_or_else(|| invalid("projection precedes its source sketch"))?;
                                    (
                                        native.attach(&projection, geometry, source_feature, (source_plane, plane), &mut sketch)?,
                                        (0_u8, *source_geometry),
                                    )
                                }
                                projections::Source::Body(edge) => {
                                    if operation_owners.is_none() {
                                        operation_owners = Some(self.operation_owners()?);
                                    }
                                    projection_model.evaluate(&doc);
                                    if projection_model.results.iter().any(|r| r.error.is_some()) {
                                        return Err(invalid("cannot regenerate body projection dependencies"));
                                    }
                                    let state = projection_model.state();
                                    let context = super::body_projections::Context {
                                        native: &native,
                                        profiles: &source_profiles,
                                        doc: &doc,
                                        model: &state,
                                        features: &feature_ids,
                                        operations: operation_owners.as_ref().ok_or_else(|| invalid("missing topology operation ownership"))?,
                                    };
                                    (
                                        context.attach(&projection, geometry, plane, &mut sketch)?,
                                        (if edge.recipe.end { 2_u8 } else { 1_u8 }, edge.body),
                                    )
                                }
                                projections::Source::CutBody(edge) => {
                                    if operation_owners.is_none() {
                                        operation_owners = Some(self.operation_owners()?);
                                    }
                                    projection_model.evaluate(&doc);
                                    if projection_model.results.iter().any(|r| r.error.is_some()) {
                                        return Err(invalid("cannot regenerate cut-face projection dependencies"));
                                    }
                                    let state = projection_model.state();
                                    let context = super::body_projections::Context {
                                        native: &native,
                                        profiles: &source_profiles,
                                        doc: &doc,
                                        model: &state,
                                        features: &feature_ids,
                                        operations: operation_owners.as_ref().ok_or_else(|| invalid("missing topology operation ownership"))?,
                                    };
                                    (
                                        edge.attach(&context, projection.target, geometry, plane, &mut sketch)?,
                                        (if edge.end { 2_u8 } else { 1_u8 }, edge.body),
                                    )
                                }
                                projections::Source::FaceCap(edge) => {
                                    if operation_owners.is_none() {
                                        operation_owners = Some(self.operation_owners()?);
                                    }
                                    projection_model.evaluate(&doc);
                                    if projection_model.results.iter().any(|r| r.error.is_some()) {
                                        return Err(invalid("cannot regenerate face-cap projection dependencies"));
                                    }
                                    let state = projection_model.state();
                                    let context = super::body_projections::Context {
                                        native: &native,
                                        profiles: &source_profiles,
                                        doc: &doc,
                                        model: &state,
                                        features: &feature_ids,
                                        operations: operation_owners.as_ref().ok_or_else(|| invalid("missing topology operation ownership"))?,
                                    };
                                    (edge.attach(&context, projection.target, geometry, plane, &mut sketch)?, (2_u8, edge.body))
                                }
                                projections::Source::ChamferCap(edge) => {
                                    if operation_owners.is_none() {
                                        operation_owners = Some(self.operation_owners()?);
                                    }
                                    projection_model.evaluate(&doc);
                                    if projection_model.results.iter().any(|r| r.error.is_some()) {
                                        return Err(invalid("cannot regenerate chamfer cap dependencies"));
                                    }
                                    let state = projection_model.state();
                                    let operations = operation_owners.as_ref().ok_or_else(|| invalid("missing chamfer operation ownership"))?;
                                    let context = super::body_projections::Context {
                                        native: &native,
                                        profiles: &source_profiles,
                                        doc: &doc,
                                        model: &state,
                                        features: &feature_ids,
                                        operations,
                                    };
                                    let topology = super::composite_blends::Context {
                                        records: &records,
                                        classes: &classes,
                                        operations,
                                        features: &feature_ids,
                                        profiles: &source_profiles,
                                        lines: &lines,
                                        circles: &circles,
                                        doc: &doc,
                                        state: &state,
                                        model: &projection_model,
                                        before: None,
                                        depth: 0,
                                    };
                                    (edge.attach(&context, &topology, projection.target, geometry, plane, &mut sketch)?, (4_u8, edge.body))
                                }
                            };
                            for (id, index, source_point) in entities.points {
                                if !members.contains(&id) {
                                    return Err(invalid("foreign projected point"));
                                }
                                let source = (namespace, source_point);
                                if let Some(previous) = projected_sources.insert(id, source) {
                                    if previous != source {
                                        return Err(invalid("shared projected point has conflicting sources"));
                                    }
                                    // A link owns its points. Adjacent one-curve links use
                                    // aliases of the same source vertex; constraints bind
                                    // the canonical point and every alias follows that source.
                                    sketch.points.get_mut(index).ok_or_else(|| invalid("missing projected alias"))?.id =
                                        format!("p{id}:projection{}", projection.target);
                                } else {
                                    consumed.insert(id);
                                    point_indices.insert(id, index);
                                }
                            }
                            for (id, index) in entities.curves {
                                if !members.contains(&id) || !consumed.insert(id) {
                                    return Err(invalid("duplicate or foreign projected curve"));
                                }
                                curve_indices.insert(id, index);
                            }
                        }
                        let projected = consumed.clone();
                        let mut has_origin = false;
                        for id in &placement.entities {
                            if projected.contains(id) {
                                continue;
                            }
                            if let Some(p) = points.get(id) {
                                if p.linked {
                                    return Err(invalid("linked sketch source dependencies are not supported yet"));
                                }
                                if p.sketch != geometry {
                                    return Err(invalid("point belongs to another sketch"));
                                }
                                let [x, y, z] = p.position;
                                if z.abs() > 1e-10 {
                                    return Err(invalid("3D sketch points are not supported yet"));
                                }
                                let i = if p.origin {
                                    if has_origin {
                                        return Err(invalid("duplicate sketch origin"));
                                    }
                                    has_origin = true;
                                    0
                                } else {
                                    sketch.add_point(Vec2::new(x * 10., y * 10.), Some(&format!("p{id}"))).map_err(|e| invalid(&e.to_string()))?
                                };
                                let point = sketch.points.get_mut(i).ok_or_else(|| invalid("missing imported point"))?;
                                point.fixed = p.fixed;
                                point_indices.insert(*id, i);
                                if !consumed.insert(*id) {
                                    return Err(invalid("duplicate sketch entity"));
                                }
                            }
                        }
                        for id in &placement.entities {
                            if projected.contains(id) {
                                continue;
                            }
                            if let Some(c) = circles.get(id) {
                                if c.reference {
                                    return Err(invalid("linked sketch source dependencies are not supported yet"));
                                }
                                if c.sketch != geometry || c.center.get(2).is_none_or(|z| z.abs() > 1e-10) || !xy_normal(&c.normal) {
                                    return Err(invalid("unsupported circle placement"));
                                }
                                let center = points.get(&c.center_point).ok_or_else(|| invalid("missing circle center"))?;
                                if c.center.iter().zip(center.position).any(|(a, b)| (a - b).abs() > 1e-10) {
                                    return Err(invalid("circle and center point disagree"));
                                }
                                let pi = point_indices.get(&c.center_point).copied().ok_or_else(|| invalid("circle center is outside its sketch"))?;
                                let [x, y, _] = c.center;
                                let i = sketch
                                    .add_circle(Vec2::new(x * 10., y * 10.), c.radius * 10., Some(pi), Some(&format!("c{id}")))
                                    .map_err(|e| invalid(&e.to_string()))?;
                                let curve = sketch.curves.get_mut(i).ok_or_else(|| invalid("missing imported curve"))?;
                                curve.construction = c.construction;
                                curve.fixed = c.fixed;
                                curve_indices.insert(*id, i);
                                if !consumed.insert(*id) {
                                    return Err(invalid("duplicate sketch entity"));
                                }
                            }
                        }
                        for id in &placement.entities {
                            if projected.contains(id) {
                                continue;
                            }
                            if let Some(arc) = arcs.get(id) {
                                let c = &arc.circle;
                                if c.reference {
                                    return Err(invalid("linked sketch source dependencies are not supported yet"));
                                }
                                if c.sketch != geometry || !xy_normal(&c.normal) {
                                    return Err(invalid("unsupported arc placement"));
                                }
                                let center = Vec3::new(c.center[0], c.center[1], c.center[2]);
                                let x = Vec3::new(c.x_axis[0], c.x_axis[1], c.x_axis[2]);
                                let y = Vec3::new(0., 0., 1.).cross(x);
                                let position = |angle: f64| center + (x * angle.cos() + y * angle.sin()) * c.radius;
                                let mut indices = [None; 3];
                                let mut coords = [Vec2::ZERO; 3];
                                for (slot, (id, expected)) in
                                    [(c.center_point, center), (arc.start_point, position(arc.start_angle)), (arc.end_point, position(arc.end_angle))]
                                        .into_iter()
                                        .enumerate()
                                {
                                    let point = points.get(&id).filter(|p| p.sketch == geometry).ok_or_else(|| invalid("missing arc point"))?;
                                    let [px, py, pz] = point.position;
                                    // Rotated public-API controls save an angle about 2e-8 rad
                                    // from its exact endpoint; retain the independently saved point.
                                    if (Vec3::new(px, py, pz) - expected).len() > 1e-7 * c.radius.max(1.) || pz.abs() > 1e-10 {
                                        return Err(invalid("arc and point geometry disagree"));
                                    }
                                    let index = point_indices.get(&id).copied().ok_or_else(|| invalid("arc point outside sketch"))?;
                                    if let Some(target) = indices.get_mut(slot) {
                                        *target = Some(index);
                                    }
                                    if let Some(target) = coords.get_mut(slot) {
                                        *target = Vec2::new(px * 10., py * 10.);
                                    }
                                }
                                let [center, start, end] = coords;
                                let i = sketch.add_arc(center, start, end, indices, Some(&format!("c{id}"))).map_err(|e| invalid(&e.to_string()))?;
                                let curve = sketch.curves.get_mut(i).ok_or_else(|| invalid("missing imported arc"))?;
                                curve.construction = c.construction;
                                if c.fixed {
                                    // Native fixation holds the supporting circle. Endpoint
                                    // fixation is independent; curve.fixed would freeze both.
                                    let [center, _, _] = indices;
                                    let p = center.ok_or_else(|| invalid("missing fixed arc center"))?;
                                    for kind in [ConstraintKind::Fix { p }, ConstraintKind::Radius { c: i, value: c.radius * 10. }] {
                                        sketch.add_constraint(kind, None).map_err(|e| invalid(&e.to_string()))?;
                                    }
                                }
                                curve_indices.insert(*id, i);
                                if !consumed.insert(*id) {
                                    return Err(invalid("duplicate sketch entity"));
                                }
                            }
                        }
                        for id in &placement.entities {
                            if projected.contains(id) {
                                continue;
                            }
                            if let Some(line) = lines.get(id) {
                                if line.reference {
                                    return Err(invalid("linked sketch source dependencies are not supported yet"));
                                }
                                if line.sketch != geometry {
                                    return Err(invalid("line belongs to another sketch"));
                                }
                                for (point, position) in [(line.start_point, line.start), (line.end_point, line.end)] {
                                    let p = points.get(&point).filter(|p| p.sketch == geometry).ok_or_else(|| invalid("missing line endpoint"))?;
                                    if p.position.iter().zip(position).any(|(a, b)| (a - b).abs() > 1e-10) {
                                        return Err(invalid("line and endpoint disagree"));
                                    }
                                }
                                let a = point_indices.get(&line.start_point).copied().ok_or_else(|| invalid("line start outside sketch"))?;
                                let b = point_indices.get(&line.end_point).copied().ok_or_else(|| invalid("line end outside sketch"))?;
                                let i = sketch.add_line_pts(a, b, Some(&format!("c{id}"))).map_err(|e| invalid(&e.to_string()))?;
                                let c = sketch.curves.get_mut(i).ok_or_else(|| invalid("missing imported line"))?;
                                c.construction = line.construction;
                                c.centerline = line.centerline;
                                if line.fixed {
                                    fix_line_support(&mut sketch, i, *id)?;
                                }
                                curve_indices.insert(*id, i);
                                if !consumed.insert(*id) {
                                    return Err(invalid("duplicate sketch entity"));
                                }
                            }
                        }
                        for constraint in &relations.constraints {
                            let (kind, param) = match constraint {
                                SketchConstraint::OffsetCurves(offset) => {
                                    let parameter = length_parameter(&bindings, offset.binding)?;
                                    super::offset_import::attach(&mut sketch, offset, parameter)?;
                                    consumed.insert(offset.dimension);
                                    continue;
                                }
                                SketchConstraint::LineMidpoints { id, lines: [a, b] } => {
                                    let a = curve_indices.get(a).copied().ok_or_else(|| invalid("missing midpoint line"))?;
                                    let b = curve_indices.get(b).copied().ok_or_else(|| invalid("missing midpoint line"))?;
                                    line_midpoints(&mut sketch, [a, b], *id)?;
                                    continue;
                                }
                                SketchConstraint::Symmetry { axis, entities, .. } => {
                                    for kind in symmetry(&sketch, &point_indices, &curve_indices, *axis, *entities)? {
                                        sketch.add_constraint(kind, None).map_err(|e| invalid(&e.to_string()))?;
                                    }
                                    continue;
                                }
                                SketchConstraint::Coincident { points: [a, b], .. } => {
                                    let p = point_indices.get(a).copied().ok_or_else(|| invalid("missing coincidence point"))?;
                                    let q = point_indices.get(b).copied().ok_or_else(|| invalid("missing coincidence point"))?;
                                    (ConstraintKind::Coincident { p, q }, None)
                                }
                                SketchConstraint::AlignedPoints { points: [a, b], vertical, .. } => {
                                    let p = point_indices.get(a).copied().ok_or_else(|| invalid("missing aligned point"))?;
                                    let q = point_indices.get(b).copied().ok_or_else(|| invalid("missing aligned point"))?;
                                    (
                                        if *vertical { ConstraintKind::VerticalPoints { p, q } } else { ConstraintKind::HorizontalPoints { p, q } },
                                        None,
                                    )
                                }
                                SketchConstraint::PointCurve { point, curve, midpoint, .. } => {
                                    let p = point_indices.get(point).copied().ok_or_else(|| invalid("missing constrained point"))?;
                                    let c = curve_indices.get(curve).copied().ok_or_else(|| invalid("missing constrained curve"))?;
                                    (if *midpoint { ConstraintKind::Midpoint { p, l: c } } else { ConstraintKind::PointOnCurve { p, c } }, None)
                                }
                                SketchConstraint::Curves { curves: [a, b], relation, .. } => {
                                    let a = curve_indices.get(a).copied().ok_or_else(|| invalid("missing constrained curve"))?;
                                    let b = curve_indices.get(b).copied().ok_or_else(|| invalid("missing constrained curve"))?;
                                    (
                                        match relation {
                                            CurveRelation::Collinear => ConstraintKind::Collinear { a, b },
                                            CurveRelation::Concentric => ConstraintKind::Concentric { a, b },
                                            CurveRelation::Parallel => ConstraintKind::Parallel { a, b },
                                            CurveRelation::Perpendicular => ConstraintKind::Perpendicular { a, b },
                                            CurveRelation::Tangent => ConstraintKind::Tangent { a, b },
                                            CurveRelation::Equal => ConstraintKind::Equal { a, b },
                                        },
                                        None,
                                    )
                                }
                                SketchConstraint::Diameter { id, circle, binding } | SketchConstraint::Radius { id, circle, binding } => {
                                    let c = curve_indices.get(circle).copied().ok_or_else(|| invalid("missing dimensioned curve"))?;
                                    let parameter = length_parameter(&bindings, *binding)?;
                                    consumed.insert(*id);
                                    let value = parameter.value * 10.;
                                    (
                                        if matches!(constraint, SketchConstraint::Radius { .. }) {
                                            ConstraintKind::Radius { c, value }
                                        } else {
                                            ConstraintKind::Diameter { c, value }
                                        },
                                        Some(parameter.name.clone()),
                                    )
                                }
                                SketchConstraint::Horizontal { line, .. } | SketchConstraint::Vertical { line, .. } => {
                                    let l = curve_indices.get(line).copied().ok_or_else(|| invalid("missing constrained line"))?;
                                    (
                                        if matches!(constraint, SketchConstraint::Horizontal { .. }) {
                                            ConstraintKind::Horizontal { l }
                                        } else {
                                            ConstraintKind::Vertical { l }
                                        },
                                        None,
                                    )
                                }
                                SketchConstraint::Linear { id, points: [a, b], orientation, binding } => {
                                    let p = point_indices.get(a).copied().ok_or_else(|| invalid("missing dimension point"))?;
                                    let q = point_indices.get(b).copied().ok_or_else(|| invalid("missing dimension point"))?;
                                    let parameter = length_parameter(&bindings, *binding)?;
                                    consumed.insert(*id);
                                    let value = parameter.value * 10.;
                                    (
                                        match orientation {
                                            LinearOrientation::Horizontal => ConstraintKind::DistanceX { p, q, value },
                                            LinearOrientation::Vertical => ConstraintKind::DistanceY { p, q, value },
                                            LinearOrientation::Aligned => ConstraintKind::Distance { p, q, value },
                                        },
                                        Some(parameter.name.clone()),
                                    )
                                }
                                SketchConstraint::Offset { id, line, entity, binding } => {
                                    let l = curve_indices.get(line).copied().ok_or_else(|| invalid("missing offset dimension line"))?;
                                    let p = if let Some(p) = point_indices.get(entity) {
                                        *p
                                    } else {
                                        let c = curve_indices.get(entity).copied().ok_or_else(|| invalid("missing offset dimension entity"))?;
                                        let Some(solidcraft_doc::sketch::CurveKind::Line { a, .. }) = sketch.curves.get(c).map(|c| &c.kind) else {
                                            return Err(invalid("offset dimension requires a line or point"));
                                        };
                                        let p = *a;
                                        sketch.add_constraint(ConstraintKind::Parallel { a: l, b: c }, None).map_err(|e| invalid(&e.to_string()))?;
                                        p
                                    };
                                    let parameter = length_parameter(&bindings, *binding)?;
                                    consumed.insert(*id);
                                    (ConstraintKind::PointLineDistance { p, l, value: parameter.value * 10. }, Some(parameter.name.clone()))
                                }
                                SketchConstraint::Angle { id, lines: [a, b], quadrants, binding, cached_radians } => {
                                    let a = curve_indices.get(a).copied().ok_or_else(|| invalid("missing angular dimension line"))?;
                                    let b = curve_indices.get(b).copied().ok_or_else(|| invalid("missing angular dimension line"))?;
                                    let p = bindings.get(binding).ok_or_else(|| invalid("missing angular dimension parameter"))?;
                                    if solidcraft_doc::expr::Kind::from_unit(&p.display_unit) != solidcraft_doc::expr::Kind::Angle
                                        || (p.value - cached_radians).abs() > 1e-10
                                    {
                                        return Err(invalid("angular dimension parameter disagrees with its cached value or units"));
                                    }
                                    let [quadrant, _] = *quadrants;
                                    consumed.insert(*id);
                                    (angle_constraint(&sketch, a, b, p.value, quadrant)?, Some(p.name.clone()))
                                }
                            };
                            sketch.add_constraint(kind, param).map_err(|e| invalid(&e.to_string()))?;
                        }
                        let missing: Vec<_> = placement.entities.iter().filter(|id| !consumed.contains(id)).collect();
                        if !missing.is_empty() {
                            if self.recovering() {
                                self.warn(format!(
                                    "Sketch {}: omitted {} unsupported entities or dimensions: {:?}",
                                    f.id,
                                    missing.len(),
                                    missing.iter().take(20).collect::<Vec<_>>()
                                ));
                            } else {
                                return Err(invalid("unsupported sketch entity; import would lose history"));
                            }
                        }
                        sketches.insert(geometry, (doc.next_id, plane));
                        FeatureKind::Sketch { plane: plane_ref, sketch }
                    }
                    HistoryKind::Loft { operation, section_containers, target_containers } => {
                        if !(2..=3).contains(&section_containers.len()) {
                            return Err(invalid("multi-section loft interpolation is not supported yet"));
                        }
                        if !matches!(operation, Operation::NewBody | Operation::Join) {
                            return Err(invalid("loft cut/intersect history is not supported yet"));
                        }
                        let three_sections = section_containers.len() == 3;
                        let mut sections = Vec::new();
                        for container in section_containers {
                            let objects = super::loft_faces::objects(&records, &classes, container, f.id)?;
                            let [object] = objects.as_slice() else {
                                return Err(invalid("loft section requires one source"));
                            };
                            if classes.get(object).is_some_and(|c| c.id == super::body_projections::SELECTOR) {
                                let (anchor, _) = import_face(*object)?;
                                sections.push(solidcraft_doc::LoftSection {
                                    sketch: (0).into(),
                                    profiles: ProfileSel::All,
                                    point: None,
                                    face: Some(Box::new(anchor)),
                                });
                                continue;
                            }
                            let chosen = selected(&records, &profiles, f.id, &[container])?;
                            let [profile] = chosen.as_slice() else {
                                return Err(invalid("loft section requires one selected sketch profile"));
                            };
                            let [loops] = profile.regions.as_slice() else {
                                return Err(invalid("loft section requires one region"));
                            };
                            if loops.len() != 1 {
                                return Err(invalid("loft sections with holes are not supported yet"));
                            }
                            if three_sections {
                                let [boundary] = loops.as_slice() else { return Err(invalid("missing circular loft boundary")) };
                                let [circle] = boundary.curves.as_slice() else {
                                    return Err(invalid("three-section native loft requires complete circular sketch profiles"));
                                };
                                if !boundary.outer
                                    || boundary.pieces != [(1, 1)]
                                    || !circles.get(circle).is_some_and(|c| c.sketch == profile.geometry)
                                {
                                    return Err(invalid("three-section native loft requires complete circular sketch profiles"));
                                }
                            }
                            let (sketch, _) = sketches.get(&profile.geometry).ok_or_else(|| invalid("loft section must precede its feature"))?;
                            pending_profiles.insert(sections.len(), *profile);
                            sections.push(solidcraft_doc::LoftSection {
                                sketch: (*sketch).into(),
                                profiles: profile_selection(profile, &circles)?,
                                point: None,
                                face: None,
                            });
                        }
                        let targets = if operation == Operation::NewBody {
                            if !target_containers.is_empty() {
                                return Err(invalid("new-body loft cannot have boolean targets"));
                            }
                            body_number += 1;
                            let name = format!("Body{body_number}");
                            if !f.suppressed {
                                active_bodies.push(name.clone());
                            }
                            new_body = Some(vec![name]);
                            vec![]
                        } else {
                            let [container] = target_containers.as_slice() else {
                                return Err(invalid("loft join requires one explicit target"));
                            };
                            let target = super::loft_faces::target(&records, &classes, *container, f.id)?;
                            if operation_owners.is_none() {
                                operation_owners = Some(self.operation_owners()?);
                            }
                            let owners = operation_owners.as_ref().ok_or_else(|| invalid("missing loft target operations"))?;
                            let source = feature_ids
                                .get(&target.feature)
                                .and_then(|id| doc.feature(*id))
                                .filter(|f| !f.suppressed)
                                .ok_or_else(|| invalid("loft target precedes its producer or the producer is suppressed"))?;
                            let [body] = source.body_names.as_slice() else {
                                return Err(invalid("loft target must identify one body"));
                            };
                            if !active_bodies.contains(body) {
                                return Err(invalid("loft target body is no longer active"));
                            }
                            let mut producers = BTreeSet::new();
                            for (i, operation) in target.operations.iter().enumerate() {
                                let producer = *owners.get(&operation.operation).ok_or_else(|| invalid("missing loft target operation owner"))?;
                                let feature = feature_ids
                                    .get(&producer)
                                    .and_then(|id| doc.feature(*id))
                                    .filter(|f| !f.suppressed)
                                    .ok_or_else(|| invalid("loft target operation follows its selector or is suppressed"))?;
                                if !producers.insert(producer)
                                    || (i == 0 && producer != target.feature)
                                    || (i != 0
                                        && !match (&feature.kind, operation.flags) {
                                            (FeatureKind::Extrude { operation: Operation::Join, targets, .. }, 4)
                                            | (FeatureKind::Loft { operation: Operation::Join, targets, .. }, 128 | 129) => {
                                                targets.as_slice() == [body.clone()]
                                            }
                                            _ => false,
                                        })
                                {
                                    return Err(invalid("loft target operations disagree with their body ownership"));
                                }
                                super::body_labels::validate(&records, &classes, producer, operation.operation, u64::from(operation.label))?;
                            }
                            vec![body.clone()]
                        };
                        let interpolation = if sections.len() == 3 {
                            if sections.iter().any(|s| s.face.is_some()) {
                                return Err(invalid("three-section native loft requires circular sketch profiles"));
                            }
                            // The kernel validates coaxial complete circles on every
                            // evaluation, including after parameter/plane edits.
                            solidcraft_doc::LoftInterpolation::CircularChord
                        } else {
                            solidcraft_doc::LoftInterpolation::Uniform
                        };
                        FeatureKind::Loft { sections, interpolation, operation, targets: targets.into_iter().map(Into::into).collect() }
                    }
                    HistoryKind::Extrude { operation, profile_containers, target_containers } => {
                        let objects = profile_containers
                            .iter()
                            .map(|container| super::loft_faces::objects(&records, &classes, *container, f.id))
                            .collect::<Result<Vec<_>>>()?
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>();
                        let has_face = objects.iter().any(|id| classes.get(id).is_some_and(|c| c.id == super::body_projections::SELECTOR));
                        let (profile, anchor, plane) = if has_face {
                            let [object] = objects.as_slice() else { return Err(invalid("face extrusion requires exactly one face")) };
                            let (anchor, plane) = import_face(*object)?;
                            (None, Some(anchor), plane)
                        } else {
                            let selected = selected(&records, &profiles, f.id, &profile_containers)?;
                            let [profile] = selected.as_slice() else {
                                return Err(invalid("multiple-profile extrusion reconstruction is not supported yet"));
                            };
                            let (_, plane) =
                                sketches.get(&profile.geometry).copied().ok_or_else(|| invalid("extrusion precedes its source sketch"))?;
                            (Some(*profile), None, plane)
                        };
                        let (mut extent, endpoint) = extent(&records, &classes, &bindings, f.id, &plane)?;
                        if let Some(container) = endpoint {
                            let objects = super::loft_faces::objects(&records, &classes, container, f.id)?;
                            let [object] = objects.as_slice() else {
                                return Err(invalid("extrusion endpoint requires one entity"));
                            };
                            let target = if classes.get(object).is_some_and(|c| c.id == super::body_projections::SELECTOR) {
                                let (anchor, plane) = import_face(*object)?;
                                PlaneRef::Face { plane, at: plane.origin, name: None, anchor: Some(Box::new(anchor)) }
                            } else {
                                let geometry = super::planes::tracked_plane(&records, &classes, *object, f.id)?;
                                if let Some((name, _)) = planes.get(&geometry) {
                                    PlaneRef::Construction { name: (name.clone()).into() }
                                } else {
                                    let (name, _) = super::planes::origin(&records, &classes, geometry, root)?;
                                    PlaneRef::Origin { name }
                                }
                            };
                            extent.to_plane = Some(Box::new(target));
                        }
                        let face_body = anchor.as_ref().map(|anchor| anchor.target.body.clone());
                        let (sketch, selection, region_count) = if let Some(profile) = profile {
                            source_profiles.insert(f.id, profile);
                            pending_profiles.insert(0, profile);
                            let (sketch, _) = sketches.get(&profile.geometry).copied().ok_or_else(|| invalid("missing extrusion sketch"))?;
                            (sketch, profile_selection(profile, &circles)?, profile.regions.len())
                        } else {
                            let anchor = anchor.ok_or_else(|| invalid("missing extrusion face anchor"))?;
                            let sketch = face_extrusion_sketch(&mut doc, &projection_model.state(), anchor, plane, &f.name)?;
                            (sketch, ProfileSel::All, 1)
                        };
                        let targets = if operation == Operation::NewBody {
                            let bodies: Vec<_> = (0..region_count)
                                .map(|_| {
                                    body_number += 1;
                                    format!("Body{body_number}")
                                })
                                .collect();
                            if !f.suppressed {
                                active_bodies.extend(bodies.iter().cloned());
                            }
                            new_body = Some(bodies);
                            vec![]
                        } else if !target_containers.is_empty() {
                            if operation_owners.is_none() {
                                operation_owners = Some(self.operation_owners()?);
                            }
                            let context = super::extrusion_targets::Context {
                                records: &records,
                                classes: &classes,
                                document: &doc,
                                feature_ids: &feature_ids,
                                owners: operation_owners.as_ref().ok_or_else(|| invalid("missing extrusion target operations"))?,
                                active: &active_bodies,
                            };
                            let body = context.single(&target_containers, f.id)?;
                            if face_body.as_ref().is_some_and(|source| source != &body) {
                                return Err(invalid("face extrusion profile and explicit target select different bodies"));
                            }
                            vec![body]
                        } else {
                            if let Some(body) = face_body {
                                if !active_bodies.contains(&body) {
                                    return Err(invalid("face extrusion target body is unavailable"));
                                }
                                vec![body]
                            } else {
                                if active_bodies.len() != 1 {
                                    return Err(invalid("multi-body boolean target reconstruction is not supported yet"));
                                }
                                active_bodies.clone()
                            }
                        };
                        FeatureKind::Extrude {
                            sketch: (sketch).into(),
                            profiles: selection,
                            extent,
                            operation,
                            targets: targets.into_iter().map(Into::into).collect(),
                            thin: None,
                        }
                    }
                };
                match &mut kind {
                    FeatureKind::Extrude { sketch, profiles, .. } => {
                        *profiles = resolved_profile_selection(
                            profiles.clone(),
                            pending_profiles.get(&0).copied(),
                            &circles,
                            &doc,
                            &mut projection_model,
                            sketch.0,
                        )?;
                    }
                    FeatureKind::Loft { sections, .. } => {
                        for (index, section) in sections.iter_mut().enumerate() {
                            section.profiles = resolved_profile_selection(
                                section.profiles.clone(),
                                pending_profiles.get(&index).copied(),
                                &circles,
                                &doc,
                                &mut projection_model,
                                (section.sketch).0,
                            )?;
                        }
                    }
                    _ => {}
                }
                let id = doc.add_feature(kind, Some(&f.name))?;
                feature_ids.insert(f.id, id);
                let feature = doc.features.iter_mut().find(|f| f.id == id).ok_or_else(|| invalid("missing imported feature"))?;
                feature.suppressed = f.suppressed;
                feature.edge_names = new_edge_names;
                if let Some((geometry, plane)) = new_plane
                    && planes.insert(geometry, (feature.name.clone(), plane)).is_some()
                {
                    return Err(invalid("duplicate construction-plane geometry"));
                }
                // Feature inputs own their native parameter identity. Keeping a second
                // stored parameter would leave downstream expressions stale after a
                // timeline edit. Reserve all names above, then transfer these inputs.
                let mut transferred = BTreeSet::new();
                for (name, (_, expression, kind)) in feature.param_names.iter_mut().zip(feature.kind.inputs_mut()) {
                    let Some(p) = named_parameters.get(expression.as_str()).filter(|p| p.owner == Some(f.id)) else { continue };
                    // SolidCraft feature inputs use mm/degrees. Preserve the existing
                    // unit-bearing parameter indirection for other display units until
                    // feature inputs support their own unit contexts.
                    if solidcraft_doc::expr::unit_info(&p.display_unit) != solidcraft_doc::expr::unit_info(kind.default_unit()) {
                        continue;
                    }
                    *name = p.name.clone();
                    *expression = (p.expression.clone()).into();
                    transferred.insert(p.name.clone());
                    if !p.comment.is_empty() {
                        doc.param_comments.insert(p.name.clone(), p.comment.clone());
                    }
                }
                if let Some(body) = new_body {
                    feature.body_names = body;
                }
                doc.params.retain(|p| !transferred.contains(&p.name));
                if let Some(expected) = new_point {
                    projection_model.evaluate(&doc);
                    if projection_model.results.iter().any(|r| r.error.is_some()) {
                        return Err(invalid("cannot regenerate imported construction point"));
                    }
                    let state = projection_model.state();
                    let actual = state.construct.iter().find(|c| c.feature == id).map(|c| &c.geom);
                    if !matches!(actual, Some(solidcraft_doc::construct::ConstructGeom::Point(p)) if p.dist(expected) <= 1e-6) {
                        return Err(invalid("construction point changed position during import"));
                    }
                }
                if self.recovering() {
                    projection_model.evaluate(&doc);
                    if let Some(error) = projection_model.results.iter().find_map(|r| r.error.as_ref()) {
                        return Err(invalid(&format!("regeneration failed: {error}")));
                    }
                }
                Ok(())
            })();
            if let Err(error) = outcome {
                if let Some(snapshot) = snapshot {
                    (doc, sketches, planes, active_bodies, body_number, feature_ids, source_profiles) = snapshot;
                    projection_model = solidcraft_doc::Model::new();
                    omitted.insert(native_id);
                    self.warn(format!("Omitted feature {native_id} ({native_name}): {error}"));
                } else {
                    return Err(error);
                }
            }
        }
        // Preserve expressions and reject a translation that changes their value.
        // A cached native value is a check, never a substitute for an expression.
        if self.recovering() {
            return Ok(doc);
        }
        let (values, errors) = doc.param_values();
        if !errors.is_empty() {
            return Err(invalid(&format!("parameter expressions cannot be reconstructed: {errors:?}")));
        }
        for p in &parameters {
            if internal_offsets.contains(&p.id) {
                continue;
            }
            let kind = solidcraft_doc::expr::Kind::from_unit(&p.display_unit);
            let value = values.get(&p.name).ok_or_else(|| invalid("missing imported parameter"))?.to_kind(kind)?;
            let expected = p.value * if kind == solidcraft_doc::expr::Kind::Length { 10. } else { 1. };
            if (value - expected).abs() > 1e-8 * expected.abs().max(1.) {
                return Err(invalid(&format!("parameter {} changed value during import", p.name)));
            }
        }
        Ok(doc)
    }
}

fn length_parameter<'a>(bindings: &BTreeMap<u64, &'a Parameter>, binding: u64) -> Result<&'a Parameter> {
    let p = bindings.get(&binding).copied().ok_or_else(|| invalid("missing dimension parameter"))?;
    if solidcraft_doc::expr::Kind::from_unit(&p.display_unit) != solidcraft_doc::expr::Kind::Length {
        return Err(invalid("dimension requires a length parameter"));
    }
    Ok(p)
}

pub(super) fn angle_constraint(sketch: &Sketch, a: usize, b: usize, value: f64, quadrant: u32) -> Result<ConstraintKind> {
    let direction = |i: usize| {
        let Some(solidcraft_doc::sketch::CurveKind::Line { a, b }) = sketch.curves.get(i).map(|c| &c.kind) else {
            return Err(invalid("angular dimension requires finite lines"));
        };
        let start = sketch.point(*a).ok_or_else(|| invalid("missing angular line endpoint"))?;
        let end = sketch.point(*b).ok_or_else(|| invalid("missing angular line endpoint"))?;
        (end - start).normalized().ok_or_else(|| invalid("degenerate angular dimension line"))
    };
    let da = direction(a)?;
    let db = direction(b)?;
    let cross = da.cross(db);
    let direct = cross.abs().atan2(da.dot(db));
    let flip = quadrant % 2 == 1;
    let expected = if flip { std::f64::consts::PI - value } else { value };
    if !direct.is_finite() || (direct - expected).abs() > 1e-7 {
        return Err(invalid("angular dimension quadrant disagrees with native line geometry"));
    }
    // Odd quadrants measure the supplement. Swapping the directed lines retains
    // clockwise dimensions, including at 90 degrees where cached angles coincide.
    let (a, b) = if (cross < 0.) != flip { (b, a) } else { (a, b) };
    Ok(ConstraintKind::Angle { a, b, value, flip })
}

fn fix_line_support(sketch: &mut Sketch, line: usize, id: u64) -> Result<()> {
    let Some(solidcraft_doc::sketch::CurveKind::Line { a, b }) = sketch.curves.get(line).map(|c| &c.kind) else {
        return Err(invalid("fixed support requires a line"));
    };
    let start = sketch.point(*a).ok_or_else(|| invalid("missing fixed line endpoint"))?;
    let end = sketch.point(*b).ok_or_else(|| invalid("missing fixed line endpoint"))?;
    // Native curve fixation holds the infinite support, independently of endpoint
    // fixation. SolidCraft's curve.fixed holds both endpoints and would lose DOFs.
    let anchor = sketch.add_point(start, Some(&format!("line{id}_support"))).map_err(|e| invalid(&e.to_string()))?;
    sketch.points.get_mut(anchor).ok_or_else(|| invalid("missing fixed support anchor"))?.fixed = true;
    for kind in [ConstraintKind::PointOnCurve { p: anchor, c: line }, ConstraintKind::LineAngle { l: line, value: (end - start).angle() }] {
        sketch.add_constraint(kind, None).map_err(|e| invalid(&e.to_string()))?;
    }
    Ok(())
}

pub(super) fn line_midpoints(sketch: &mut Sketch, lines: [usize; 2], id: u64) -> Result<()> {
    let center = |line: usize| {
        let Some(solidcraft_doc::sketch::CurveKind::Line { a, b }) = sketch.curves.get(line).map(|c| &c.kind) else {
            return Err(invalid("common midpoint requires two lines"));
        };
        let a = sketch.point(*a).ok_or_else(|| invalid("missing midpoint endpoint"))?;
        let b = sketch.point(*b).ok_or_else(|| invalid("missing midpoint endpoint"))?;
        Ok((a + b) * 0.5)
    };
    let [a, b] = lines;
    let (ma, mb) = (center(a)?, center(b)?);
    if !ma.is_finite() || !mb.is_finite() || (ma - mb).len() > 1e-7 * ma.len().max(mb.len()).max(1.) {
        return Err(invalid("line midpoints disagree with native geometry"));
    }
    // A shared, unfixed point equates the centers without constraining line
    // angle or length. Native line-to-line midpoint allows independent rotation.
    let p = sketch.add_point(ma, Some(&format!("midpoint{id}"))).map_err(|e| invalid(&e.to_string()))?;
    for l in lines {
        sketch.add_constraint(ConstraintKind::Midpoint { p, l }, None).map_err(|e| invalid(&e.to_string()))?;
    }
    Ok(())
}

fn xy_normal(normal: &[f64; 3]) -> bool {
    normal.iter().zip([0., 0., 1.]).all(|(value, expected)| value.is_finite() && (value - expected).abs() <= 1e-10)
}

fn resolved_profile_selection(
    selection: ProfileSel,
    native: Option<&SketchProfile>,
    circles: &BTreeMap<u64, SketchCircle>,
    doc: &Document,
    model: &mut solidcraft_doc::Model,
    sketch: u64,
) -> Result<ProfileSel> {
    let grouped = native.filter(|profile| profile.region_members.iter().any(|members| !members.is_empty()));
    if native.is_none() && !matches!(selection, ProfileSel::PieceBoundaries { .. }) {
        return Ok(selection);
    }
    model.evaluate(doc);
    if model.results.iter().any(|r| r.error.is_some()) {
        return Err(invalid("cannot regenerate selected profile dependencies"));
    }
    let state = model.state();
    let solved = state.sketch(sketch).ok_or_else(|| invalid("missing selected profile sketch"))?;
    if let Some(profile) = grouped {
        return super::profile_groups::resolve(profile, circles, solved);
    }
    let selection =
        if !matches!(selection, ProfileSel::PieceBoundaries { .. }) && solidcraft_doc::profile_selection_indices(solved, &selection).is_err() {
            match native {
                Some(profile) => selection_for_regions(&profile.regions, circles, true)?,
                None => selection,
            }
        } else {
            selection
        };
    super::profile_selection::resolve(selection, solved)
}

fn profile_selection(profile: &SketchProfile, circles: &BTreeMap<u64, SketchCircle>) -> Result<ProfileSel> {
    selection_for_regions(&profile.regions, circles, false)
}

pub(super) fn selection_for_regions(regions: &[Vec<ProfileLoop>], circles: &BTreeMap<u64, SketchCircle>, directed: bool) -> Result<ProfileSel> {
    if directed || regions.iter().flatten().flat_map(|boundary| &boundary.pieces).any(|(_, count)| *count != 1) {
        let boundary = |boundary: &ProfileLoop| -> Result<Vec<solidcraft_doc::sketch::ProfilePiece>> {
            let mut pieces = Vec::new();
            for ((id, (piece, count)), reversed) in boundary.curves.iter().zip(&boundary.pieces).zip(&boundary.reversed) {
                pieces.push(solidcraft_doc::sketch::ProfilePiece { curve: format!("c{id}"), piece: *piece, count: *count, reversed: *reversed });
            }
            pieces.sort();
            Ok(pieces)
        };
        let mut profiles = Vec::new();
        for loops in regions {
            let outer = loops.iter().find(|l| l.outer).ok_or_else(|| invalid("missing outer profile boundary"))?;
            let mut holes = loops.iter().filter(|l| !l.outer).map(boundary).collect::<Result<Vec<_>>>()?;
            holes.sort();
            profiles.push(solidcraft_doc::PieceProfileBoundary { outer: boundary(outer)?, holes });
        }
        let mut circle_seams = BTreeMap::new();
        for id in regions.iter().flatten().flat_map(|boundary| &boundary.curves) {
            if let Some(circle) = circles.get(id) {
                let angle = circle.x_axis[1].atan2(circle.x_axis[0]).rem_euclid(std::f64::consts::TAU);
                if angle > 1e-12 && angle < std::f64::consts::TAU - 1e-12 {
                    circle_seams.insert(format!("c{id}"), angle);
                }
            }
        }
        return Ok(ProfileSel::PieceBoundaries { profiles, circle_seams });
    }
    let ids = |boundary: &ProfileLoop| {
        let mut ids: Vec<_> = boundary.curves.iter().map(|id| format!("c{id}")).collect();
        ids.sort();
        ids
    };
    let mut profiles = Vec::new();
    for loops in regions {
        let outer = loops.iter().find(|l| l.outer).ok_or_else(|| invalid("missing outer profile boundary"))?;
        let mut holes: Vec<_> = loops.iter().filter(|l| !l.outer).map(ids).collect();
        holes.sort();
        profiles.push(solidcraft_doc::WholeProfileBoundary { outer: ids(outer), holes });
    }
    Ok(ProfileSel::WholeBoundaries { profiles })
}

pub(super) fn plane(matrix: &[f64; 16]) -> Result<Plane> {
    if matrix.iter().any(|n| !n.is_finite()) {
        return Err(invalid("non-finite plane placement"));
    }
    let [xx, yx, nx, ox, xy, yy, ny, oy, xz, yz, nz, oz, a, b, c, d] = *matrix;
    if [a, b, c, d] != [0., 0., 0., 1.] {
        return Err(invalid("non-affine sketch placement"));
    }
    let x = Vec3::new(xx, xy, xz);
    let y = Vec3::new(yx, yy, yz);
    let n = Vec3::new(nx, ny, nz);
    if (x.len() - 1.).abs() > 1e-9 || (y.len() - 1.).abs() > 1e-9 || x.dot(y).abs() > 1e-9 || (x.cross(y) - n).len() > 1e-9 {
        return Err(invalid("non-rigid sketch placement"));
    }
    Plane::new(Vec3::new(ox, oy, oz) * 10., x, y).ok_or_else(|| invalid("invalid sketch placement"))
}

/// Offset-only origin-plane chains keep a fixed normal. An in-plane rotation
/// about that normal preserves their editable position and native sketch basis.
pub(super) fn sketch_plane_reference(base: PlaneRef, cached: Plane, sketch: Plane) -> Result<PlaneRef> {
    if !super::planes::same_surface(cached, sketch) {
        return Err(invalid("unsupported sketch frame relative to its construction plane"));
    }
    if (cached.x - sketch.x).len() <= 1e-9 && (cached.y - sketch.y).len() <= 1e-9 {
        return Ok(base);
    }
    let normal = cached.normal();
    if cached.origin.cross(normal).len() > 1e-7 {
        return Err(invalid("rotated sketch requires an origin-plane offset chain"));
    }
    let angle = sketch.x.dot(cached.y).atan2(sketch.x.dot(cached.x));
    let (sin, cos) = angle.sin_cos();
    if (cached.x * cos + cached.y * sin - sketch.x).len() > 1e-9 || (cached.y * cos - cached.x * sin - sketch.y).len() > 1e-9 {
        return Err(invalid("unsupported sketch basis rotation"));
    }
    Ok(PlaneRef::AtAngle {
        base: Box::new(base),
        axis_origin: Vec3::default(),
        axis_dir: normal,
        angle: format!("{} deg", angle.to_degrees()).into(),
    })
}

fn plane_base(
    records: &Records<'_>,
    classes: &BTreeMap<u64, &Class>,
    planes: &BTreeMap<u64, (String, Plane)>,
    id: u64,
    root: u64,
) -> Result<(PlaneRef, Plane)> {
    if let Some((name, plane)) = planes.get(&id) {
        return Ok((PlaneRef::Construction { name: (name.clone()).into() }, *plane));
    }
    let (name, plane) = super::planes::origin(records, classes, id, root)?;
    Ok((PlaneRef::Origin { name }, plane))
}

fn selected<'a>(
    records: &Records<'_>,
    profiles: &'a BTreeMap<u64, SketchProfile>,
    feature: u64,
    containers: &[u64],
) -> Result<Vec<&'a SketchProfile>> {
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for id in containers {
        let bytes = records.primary(*id)?;
        let mut r = payload(bytes, *id)?;
        empty(&mut r)?;
        zeros(&mut r, 2)?;
        for id in references(&mut r)? {
            if !seen.insert(id) {
                return Err(invalid("duplicate selected profile"));
            }
            selected.push(profiles.get(&id).filter(|p| p.feature == feature).ok_or_else(|| invalid("missing selected profile or wrong owner"))?);
        }
        let tail = bytes.len().checked_sub(11).filter(|at| *at >= r.at).ok_or_else(|| invalid("truncated profile container"))?;
        r.at = tail;
        if r.reference()? != feature {
            return Err(invalid("inconsistent profile container owner"));
        }
        finish(&r)?;
    }
    Ok(selected)
}

/// A live face projection supplies the boundary; cached native tessellation does
/// not participate in selection or geometry reconstruction.
fn face_extrusion_sketch(
    doc: &mut Document,
    state: &solidcraft_doc::ModelState,
    anchor: solidcraft_doc::FaceAnchor,
    plane: Plane,
    name: &str,
) -> Result<u64> {
    use solidcraft_doc::sketch::{LinkGeom, LinkKind};
    let resolved = solidcraft_doc::project::resolve_anchored_face(doc, state, &plane, LinkKind::Project, &anchor)?;
    let circular = match resolved.geom.as_slice() {
        [LinkGeom::Circle(_, _)] => true,
        [LinkGeom::Arc { c, a, b }, LinkGeom::Arc { c: d, a: p, b: q }] => {
            let tolerance = 1e-6 * a.dist(*c).max(1.);
            c.dist(*d) < tolerance && a.dist(*q) < tolerance && b.dist(*p) < tolerance && a.dist(*b) > tolerance
        }
        _ => false,
    };
    if !circular {
        return Err(invalid("face extrusion requires a verified complete circular cap"));
    }
    let mut sketch = Sketch::new();
    let link =
        sketch.add_link_with_wires(LinkKind::Project, resolved.source, &resolved.geom, &resolved.wires).map_err(|e| invalid(&e.to_string()))?;
    let entry = sketch.links.iter_mut().find(|entry| entry.id == link).ok_or_else(|| invalid("missing face extrusion projection"))?;
    entry.face_name = Some(anchor.target.name.clone());
    entry.auto_face = true;
    let plane = PlaneRef::Face { plane, at: plane.origin, name: None, anchor: Some(Box::new(anchor)) };
    Ok(doc.add_feature(FeatureKind::Sketch { plane, sketch }, Some(&format!("{name} profile")))?)
}

fn extent(
    records: &Records<'_>,
    classes: &BTreeMap<u64, &Class>,
    parameters: &BTreeMap<u64, &Parameter>,
    id: u64,
    plane: &Plane,
) -> Result<(Extent, Option<u64>)> {
    let mut r = payload(records.primary(id)?, id)?;
    let input = super::history::extrude_inputs(&mut r, classes)?;
    if (input.axis - plane.normal()).len() > 1e-9 {
        return Err(invalid("extrusion axis differs from its sketch normal"));
    }
    let start_offset = input
        .start_offset
        .map(|binding| {
            let p = parameters
                .get(&binding)
                .filter(|p| p.owner == Some(id) && p.label == "ProfileOffset")
                .ok_or_else(|| invalid("missing extrusion start-offset binding"))?;
            if solidcraft_doc::expr::Kind::from_unit(&p.display_unit) != solidcraft_doc::expr::Kind::Length {
                return Err(invalid("extrusion start offset requires a length parameter"));
            }
            Ok(p.name.clone())
        })
        .transpose()?;
    let (binding, label, target, symmetric, full_length) = match input.limit {
        super::history::ExtrudeLimit::Distance(binding) => (binding, "AlongDistance", None, false, false),
        super::history::ExtrudeLimit::SymmetricDistance { distance, full_length } => (distance, "AlongDistance", None, true, full_length),
        super::history::ExtrudeLimit::ToEntity { offset, container } => (offset, "Side1Offset", Some(container), false, false),
    };
    let distance =
        parameters.get(&binding).filter(|p| p.owner == Some(id) && p.label == label).ok_or_else(|| invalid("missing extrusion distance binding"))?;
    let taper = parameters
        .get(&input.taper)
        .filter(|p| p.owner == Some(id) && p.label == "TaperAngle")
        .ok_or_else(|| invalid("missing extrusion taper binding"))?;
    if solidcraft_doc::expr::Kind::from_unit(&distance.display_unit) != solidcraft_doc::expr::Kind::Length
        || solidcraft_doc::expr::Kind::from_unit(&taper.display_unit) != solidcraft_doc::expr::Kind::Angle
    {
        return Err(invalid("extrusion parameter units do not match their input roles"));
    }
    // Controlled volume measurements establish the same positive taper sign.
    Ok((
        Extent {
            // Preserve the native parameter unchanged and express its full/half
            // length interpretation through the document's extent convention.
            distance: if target.is_some() { "0 mm".into() } else { (distance.name.clone()).into() },
            convention: if full_length { solidcraft_doc::ExtrudeConvention::TotalDistance } else { solidcraft_doc::ExtrudeConvention::LegacyPerSide },
            direction: if symmetric { Direction::Symmetric } else { Direction::Positive },
            distance2: None,
            start_offset: start_offset.map(Into::into),
            through_all: false,
            taper: Some((taper.name.clone()).into()),
            to: None,
            to_plane: None,
            to_offset: target.map(|_| distance.name.clone()).map(Into::into),
            from: None,
        },
        target,
    ))
}

pub(super) fn symmetry(
    sketch: &Sketch,
    points: &BTreeMap<u64, usize>,
    curves: &BTreeMap<u64, usize>,
    axis: u64,
    entities: [u64; 2],
) -> Result<Vec<ConstraintKind>> {
    use solidcraft_doc::sketch::CurveKind;
    let [a, b] = entities;
    let l = curves.get(&axis).copied().ok_or_else(|| invalid("missing symmetry axis"))?;
    if let (Some(p), Some(q)) = (points.get(&a), points.get(&b)) {
        return Ok(vec![ConstraintKind::Symmetric { p: *p, q: *q, l }]);
    }
    let ca = curves.get(&a).copied().ok_or_else(|| invalid("missing symmetric curve"))?;
    let cb = curves.get(&b).copied().ok_or_else(|| invalid("missing symmetric curve"))?;
    let kind = |i| sketch.curves.get(i).map(|c: &solidcraft_doc::sketch::Curve| &c.kind).ok_or_else(|| invalid("missing symmetry curve"));
    match (kind(ca)?, kind(cb)?) {
        (CurveKind::Circle { c: p, .. }, CurveKind::Circle { c: q, .. }) => {
            Ok(vec![ConstraintKind::Symmetric { p: *p, q: *q, l }, ConstraintKind::Equal { a: ca, b: cb }])
        }
        (CurveKind::Line { a: p0, b: p1 }, CurveKind::Line { a: q0, b: q1 }) => {
            let CurveKind::Line { a: la, b: lb } = kind(l)? else { return Err(invalid("symmetry axis is not a line")) };
            let point = |i| sketch.points.get(i).map(|p: &solidcraft_doc::sketch::SPoint| p.pos).ok_or_else(|| invalid("missing symmetry point"));
            let start = point(*la)?;
            let direction = point(*lb)? - start;
            let length2 = direction.len2();
            if !length2.is_finite() || length2 < 1e-20 {
                return Err(invalid("degenerate symmetry axis"));
            }
            let reflect = |p: Vec2| start + direction * (2. * (p - start).dot(direction) / length2) - (p - start);
            let [r0, r1] = [reflect(point(*p0)?), reflect(point(*p1)?)];
            let close = |p: Vec2, q: Vec2| (p - q).len() <= 1e-7 * p.len().max(q.len()).max(1.);
            let (q0, q1) = if close(r0, point(*q0)?) && close(r1, point(*q1)?) {
                (*q0, *q1)
            } else if close(r0, point(*q1)?) && close(r1, point(*q0)?) {
                (*q1, *q0)
            } else {
                return Err(invalid("symmetric line endpoints disagree with native geometry"));
            };
            Ok(vec![ConstraintKind::Symmetric { p: *p0, q: q0, l }, ConstraintKind::Symmetric { p: *p1, q: q1, l }])
        }
        _ => Err(invalid("unsupported symmetric entity types")),
    }
}

#[cfg(test)]
#[path = "face_extrusion_tests.rs"]
mod face_extrusion_tests;
