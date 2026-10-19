//! Finite cap/side blend edges across joined extrusion histories. Resolve every
//! contributor through exact lineage, then certify both endpoint incidences.
use super::history::{finish, payload, references, zeros};
use super::recipe_incidence::{self, Term};
use super::*;
#[cfg(test)]
use solidcraft_doc::config::ConfigFaceTarget;
use solidcraft_doc::config::FaceCertificate;
use solidcraft_doc::{Document, FeatureKind, ModelBody, ModelState, Operation};
use solidcraft_geom::Vec3;

#[path = "blend_cache.rs"]
mod cache;
#[path = "composite_blend_edges.rs"]
mod edges;
#[path = "composite_blend_faces.rs"]
mod faces;
#[path = "blend_group.rs"]
mod group;

#[derive(Clone)]
pub(super) struct Selection {
    pub body: String,
    pub name: String,
    pub point: Vec3,
}

pub(super) struct Context<'a> {
    pub records: &'a Records<'a>,
    pub classes: &'a BTreeMap<u64, &'a Class>,
    pub operations: &'a BTreeMap<u64, u64>,
    pub features: &'a BTreeMap<u64, u64>,
    pub profiles: &'a BTreeMap<u64, &'a SketchProfile>,
    pub lines: &'a BTreeMap<u64, SketchLine>,
    pub circles: &'a BTreeMap<u64, SketchCircle>,
    pub doc: &'a Document,
    pub state: &'a ModelState,
    pub model: &'a solidcraft_doc::Model,
    pub before: Option<u64>,
    pub depth: usize,
}

fn decode(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, selector: u64, owner: u64) -> Result<(u64, recipe_incidence::Identity)> {
    decode_oriented(records, classes, selector, owner, false).map(|(creator, identity, _)| (creator, identity))
}

fn decode_oriented(
    records: &Records<'_>,
    classes: &BTreeMap<u64, &Class>,
    selector: u64,
    owner: u64,
    oriented: bool,
) -> Result<(u64, recipe_incidence::Identity, Option<Term>)> {
    let version = super::loft_faces::selector_version(classes, selector)?;
    let mut r = payload(records.primary(selector)?, selector)?;
    zeros(&mut r, if version == 2 { 7 } else { 8 })?;
    let id = r.reference()?;
    let (identity, primary) = if oriented {
        let (identity, primary) = recipe_incidence::identity_with_primary(records, classes, id)?;
        (identity, Some(primary))
    } else {
        (recipe_incidence::identity(records, classes, id, false)?, None)
    };
    if r.u32()? != 1 || r.wide_string()?.is_empty() || r.wide_string()?.is_empty() || r.u32()? != 2 {
        return Err(invalid("invalid joined blend body selection"));
    }
    super::history::selection_flag(&mut r)?;
    let cached = references(&mut r)?;
    if cached.len() > 64 || cached.iter().collect::<BTreeSet<_>>().len() != cached.len() {
        return Err(invalid("invalid joined blend cache count"));
    }
    for id in cached {
        // Cache coordinates do not choose an edge.
        cache::validate(records, classes, id)?;
    }
    let handle = super::loft_faces::selector_tail(&mut r, records, owner)?;
    finish(&r)?;
    let (_, creator) = super::body_projections::body_owner(records, classes, handle)?;
    Ok((creator, identity, primary))
}

impl Context<'_> {
    fn producer(&self, op: u64, creator: u64, body: &str) -> Result<(u64, u64, u64)> {
        let native = *self.operations.get(&op).ok_or_else(|| invalid("missing joined blend operation owner"))?;
        let id = *self.features.get(&native).ok_or_else(|| invalid("blend precedes a contributing feature"))?;
        let feature = self.doc.feature(id).filter(|f| !f.suppressed).ok_or_else(|| invalid("joined blend producer is unavailable"))?;
        let FeatureKind::Extrude { sketch, extent, operation, targets, .. } = &feature.kind else {
            return Err(invalid("joined blend contributor is not an extrusion"));
        };
        if extent.from.is_some()
            || extent.start_offset.is_some()
            || extent.taper.as_ref().is_some_and(|e| !self.doc.eval(e, solidcraft_doc::expr::Kind::Angle).is_ok_and(|v| v == 0.))
            || (id == creator && *operation != Operation::NewBody)
            || (id != creator && (!matches!(operation, Operation::Join | Operation::Cut) || targets.as_slice() != [body.to_string()]))
        {
            return Err(invalid("joined blend contributor has another target or unsupported extent"));
        }
        Ok((native, id, sketch.0))
    }

    pub fn resolve(&self, selector: u64, owner: u64) -> Result<Vec<Selection>> {
        let (native_creator, identity) = decode(self.records, self.classes, selector, owner)?;
        self.resolve_identity(native_creator, &identity)
    }

    pub fn resolve_asymmetric(&self, selector: u64, owner: u64, native_flip: bool) -> Result<(Vec<Selection>, bool)> {
        let (creator, identity, primary) = decode_oriented(self.records, self.classes, selector, owner, true)?;
        let primary = primary.ok_or_else(|| invalid("missing joined chamfer primary face"))?;
        let (body, edge, indices) = self.topology(creator, &identity)?;
        let selected = edges::resolve(body, edge, &indices)?;
        let flip = asymmetric_flip(body, edge, &indices, &primary, native_flip)?;
        Ok((selected, flip))
    }

    pub(super) fn resolve_identity(&self, native_creator: u64, identity: &recipe_incidence::Identity) -> Result<Vec<Selection>> {
        let (body, edge, indices) = self.topology(native_creator, identity)?;
        edges::resolve(body, edge, &indices)
    }

    pub(super) fn resolve_group(&self, selectors: &[u64], owner: u64) -> Result<Vec<Selection>> {
        if !(2..=64).contains(&selectors.len()) || selectors.iter().collect::<BTreeSet<_>>().len() != selectors.len() {
            return Err(invalid("invalid joined blend selection group"));
        }
        let mut inputs = Vec::new();
        let mut creator = None;
        let mut name = None;
        for selector in selectors {
            let (native, identity) = decode(self.records, self.classes, *selector, owner)?;
            if creator.is_some_and(|previous| previous != native) {
                return Err(invalid("blend group has multiple body creators"));
            }
            creator = Some(native);
            let (body, edge, indices) = self.topology(native, &identity)?;
            name = Some(body.name.clone());
            inputs.push((edge.clone(), edges::candidates(body, edge, &indices).map_err(|e| invalid(&format!("blend selector {selector}: {e}")))?));
        }
        let body = name.and_then(|name| self.state.body(&name)).ok_or_else(|| invalid("missing group body"))?;
        group::resolve(body, &inputs)
    }

    fn topology<'b>(
        &'b self,
        native_creator: u64,
        identity: &'b recipe_incidence::Identity,
    ) -> Result<(&'b ModelBody, &'b recipe_incidence::Edge, BTreeMap<Term, BTreeSet<usize>>)> {
        let creator = *self.features.get(&native_creator).ok_or_else(|| invalid("joined blend precedes its body creator"))?;
        let feature = self.doc.feature(creator).ok_or_else(|| invalid("missing joined blend body creator"))?;
        let [name] = feature.body_names.as_slice() else { return Err(invalid("joined blend requires one created body")) };
        let body = self.state.body(name).ok_or_else(|| invalid("joined blend body no longer exists"))?;
        let [edge] = identity.edges.as_slice() else { return Err(invalid("joined blend requires one finite edge")) };
        let mut indices = BTreeMap::new();
        for term in &identity.terms {
            if term.iter().all(|(_, label)| *label < 0) {
                for (op, _) in term {
                    self.producer(*op, creator, name)?;
                    if !edge.ends.iter().any(|v| v.seams.contains(term) && v.faces.iter().any(|f| f.iter().any(|(owner, _)| owner == op))) {
                        return Err(invalid("blend seam is not incident to its producer"));
                    }
                }
                continue;
            }
            let candidates = self.face_indices(term, creator, body)?;
            if candidates.is_empty() || candidates.len() > 128 {
                return Err(invalid("blend face family is empty or excessive"));
            }
            indices.insert(term.clone(), candidates);
        }
        Ok((body, edge, indices))
    }
}

// Use the same deterministic planar-face ranking as the kernel. Never infer
// the native distance assignment from a sorted label or from a cached point.
fn asymmetric_flip(
    body: &ModelBody,
    edge: &recipe_incidence::Edge,
    indices: &BTreeMap<Term, BTreeSet<usize>>,
    primary: &Term,
    native_flip: bool,
) -> Result<bool> {
    if !edge.faces.contains(primary) {
        return Err(invalid("joined chamfer primary face is not incident"));
    }
    let mut normals = Vec::new();
    for term in &edge.faces {
        let faces = indices.get(term).ok_or_else(|| invalid("missing joined chamfer side"))?;
        if faces.len() != 1 {
            return Err(invalid("asymmetric joined chamfer requires single planar side faces"));
        }
        let index = *faces.first().ok_or_else(|| invalid("empty joined chamfer side"))?;
        normals.push(body.body.planar_face(index).ok_or_else(|| invalid("nonplanar asymmetric joined chamfer face"))?.normal());
    }
    let [a, b] = normals.as_slice() else { return Err(invalid("missing joined chamfer planes")) };
    if a.dot(*b).abs() > 1e-8 {
        return Err(invalid("nonorthogonal asymmetric joined chamfer faces"));
    }
    let score = (*a - *b).dot(Vec3::new(1e-6, 1e-3, 1.));
    if !score.is_finite() || score.abs() < 1e-12 {
        return Err(invalid("ambiguous asymmetric joined chamfer face ranking"));
    }
    Ok(((primary == &edge.faces[0]) != (score > 0.)) ^ native_flip)
}

#[cfg(test)]
fn resolve_edge(body: &ModelBody, edge: &recipe_incidence::Edge, indices: &BTreeMap<Term, usize>) -> Result<Selection> {
    let families = indices.iter().map(|(term, index)| (term.clone(), BTreeSet::from([*index]))).collect();
    let mut selected = edges::resolve(body, edge, &families)?;
    if selected.len() != 1 {
        return Err(invalid("expected one finite blend span"));
    }
    selected.pop().ok_or_else(|| invalid("missing finite blend span"))
}

#[cfg(test)]
#[path = "composite_blends_tests.rs"]
mod tests;
