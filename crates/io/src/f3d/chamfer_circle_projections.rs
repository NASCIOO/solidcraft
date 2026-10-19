//! A circular cap bounded by a live chamfer face. Selector, cache and mapped
//! tracker must name the same closed edge. The chamfer's output label resolves
//! through its own input tracker, including when one group changes both ends.
use super::history::{payload, references, require_class, zeros};
use super::recipe_incidence::{self, Identity};
use super::*;

pub(super) struct Projection {
    pub body: u64,
    creator: u64,
    identity: Identity,
    end: bool,
}

pub(super) fn decode(
    records: &Records<'_>,
    classes: &BTreeMap<u64, &Class>,
    selector: u64,
    owner: u64,
    mappings: &[(u64, u64)],
) -> Result<Vec<super::projections::Projection>> {
    let [(tracking, target)] = mappings else { return Err(invalid("chamfer cap requires one circular mapping")) };
    require_class(classes, *target, "F0130424-8B7E-4092-93C9-1CA807482534", 0)?;
    let version = super::loft_faces::selector_version(classes, selector)?;
    let mut r = payload(records.primary(selector)?, selector)?;
    zeros(&mut r, if version == 2 { 7 } else { 8 })?;
    let identity = recipe_incidence::identity(records, classes, r.reference()?, true)?;
    let [cap, boundary] = identity.terms.as_slice() else { return Err(invalid("chamfer cap requires two face terms")) };
    let cap = cap.iter().copied().collect::<Vec<_>>();
    let boundary = boundary.iter().copied().collect::<Vec<_>>();
    let ([(operation, role)], [(modifier, label)]) = (cap.as_slice(), boundary.as_slice()) else {
        return Err(invalid("chamfer cap requires unambiguous producers"));
    };
    if !matches!(role, 1 | 2) || *label <= 0 || operation == modifier {
        return Err(invalid("invalid chamfer cap roles"));
    }
    let [edge] = identity.edges.as_slice() else { return Err(invalid("chamfer cap requires one closed boundary")) };
    if edge.ends.len() != 1 || r.u32()? != 1 {
        return Err(invalid("chamfer cap requires one closed carrier and body"));
    }
    r.wide_string()?;
    r.wide_string()?;
    if r.u32()? != 2 {
        return Err(invalid("unsupported chamfer cap selection mode"));
    }
    super::history::selection_flag(&mut r)?;
    let cached = references(&mut r)?;
    let [cached] = cached.as_slice() else { return Err(invalid("chamfer cap requires one cached circle")) };
    require_class(classes, *cached, "B0B3FA59-C374-444F-B424-24AA5DE41645", 0)?;
    if super::composite_projections::cached_identity(records, classes, *cached)? != *edge {
        return Err(invalid("chamfer cap cache disagrees with selected incidence"));
    }
    let tracked = super::projections::tracking_identity(records, classes, *tracking, "4B31DA1B-FCD6-4527-BC3E-BE1FBAF867FE", 1)?;
    if recipe_incidence::identity(records, classes, tracked, false)?.edges != identity.edges {
        return Err(invalid("chamfer cap mapping disagrees with selected incidence"));
    }
    let handle = super::loft_faces::selector_tail(&mut r, records, owner)?;
    let (body, creator) = super::body_projections::body_owner(records, classes, handle)?;
    let owners = records.design.operation_owners()?;
    if owners.get(operation) != Some(&creator)
        || owners.get(modifier).and_then(|id| classes.get(id)).is_none_or(|c| c.id != super::chamfers::CLASS || !matches!(c.version, 2 | 3))
    {
        return Err(invalid("chamfer cap operation ownership disagrees with its producers"));
    }
    let end = *role == 2;
    Ok(vec![super::projections::Projection {
        target: *target,
        source: super::projections::Source::ChamferCap(Projection { body, creator, identity, end }),
    }])
}

impl Projection {
    pub fn attach(
        &self,
        context: &super::body_projections::Context<'_>,
        topology: &super::composite_blends::Context<'_>,
        target: u64,
        geometry: u64,
        plane: solidcraft_geom::Plane,
        sketch: &mut solidcraft_doc::sketch::Sketch,
    ) -> Result<super::projections::Entities> {
        let selected = topology.resolve_identity(self.creator, &self.identity)?;
        let body = selected.first().ok_or_else(|| invalid("missing chamfer cap boundary"))?.body.clone();
        if selected.iter().any(|edge| edge.body != body) {
            return Err(invalid("chamfer cap boundary spans multiple bodies"));
        }
        let names = selected.into_iter().map(|edge| edge.name).collect::<Vec<_>>();
        context.attach_circle(target, geometry, plane, sketch, (&body, &names, self.end))
    }
}

#[cfg(test)]
#[path = "chamfer_circle_projection_tests.rs"]
mod original_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the local Style 6 archive"]
    fn native_style6_chamfer_caps_keep_editable_history() {
        let root = std::path::PathBuf::from(solidcraft_geom::env::var_os("F3D_ADDITIONAL_CORPUS").unwrap());
        let bytes = std::fs::read(root.join("knobs/Style 6.f3d")).unwrap();
        let outcome = import_f3d_with_preference(&bytes, "Style 6", ImportPreference::Editability).unwrap();
        for feature in &outcome.document.features {
            println!("FEATURE {} {}", feature.id, feature.name);
        }
        for warning in &outcome.warnings {
            println!("WARNING {warning}");
        }
        assert!(outcome.complete_history && outcome.warnings.is_empty(), "Style 6 must import without omissions");
        let reopened = crate::read_design(&crate::write_design_container(&outcome.document).unwrap()).unwrap();
        assert_eq!(reopened, outcome.document);
        let mut model = solidcraft_doc::Model::new();
        model.evaluate(&reopened);
        assert!(model.results.iter().all(|r| r.error.is_none()), "{:?}", model.results);
        println!("Complete Style 6 history: {} features", reopened.features.len());
    }
}
