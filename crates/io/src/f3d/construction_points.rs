//! Two-edge construction points: explicit sources, never a frozen cached point.
use super::history::{empty, finish, payload, references, require_class, zeros};
use super::*;
use solidcraft_geom::Vec3;

pub(super) const CLASS: &str = "5F40832C-61AA-4A05-95B1-EFA0BA483138";
const GEOMETRY: &str = "0E18EEF2-0D6F-480C-997E-5EBC7369AE5F";
const DEFINITION: &str = "69EE2FA7-BCC7-449E-9CA9-976CEFDFED44";

pub(super) fn inputs(r: &mut Reader<'_>, records: &Records<'_>, classes: &BTreeMap<u64, &Class>, owner: u64) -> Result<(HistoryKind, String)> {
    empty(r)?;
    zeros(r, 1)?;
    if !super::attributes::read(r)?.is_empty() || r.take(1)? != [1] || r.u32()? != 1 {
        return Err(invalid("unsupported construction-point feature flags"));
    }
    zeros(r, 4)?;
    let geometry = r.reference()?;
    let definition = r.reference()?;
    if r.take(1)? != [1] {
        return Err(invalid("unsupported construction-point definition flags"));
    }
    require_class(classes, geometry, GEOMETRY, 0)?;
    let definition_version = classes
        .get(&definition)
        .filter(|c| c.id == DEFINITION && matches!(c.version, 2 | 3))
        .map(|c| c.version)
        .ok_or_else(|| invalid("unsupported construction-point definition schema"))?;
    let (position, name) = geometry_record(records, classes, geometry, owner)?;
    let mut d = payload(records.primary(definition)?, definition)?;
    empty(&mut d)?;
    zeros(&mut d, 1)?;
    if !super::attributes::read(&mut d)?.is_empty() {
        return Err(invalid("unsupported construction-point definition attributes"));
    }
    // cadmpeg's public WorkPoint grammar identifies this as boundingboxIndex.
    // It is unused (and can contain uninitialized bytes) for a two-edge point;
    // it is not an object reference or part of the point's construction.
    d.u32()?;
    zeros(&mut d, 17)?; // two unused parameters and an absent pick-point parameter
    if vector(&mut d)?.dist(position) > 1e-7 || d.u32()? != 7 {
        return Err(invalid("unsupported or inconsistent construction-point method"));
    }
    if definition_version >= 3 {
        for _ in 0..3 {
            if f64::from_bits(d.u64()?) != -1. {
                return Err(invalid("unsupported construction-point path parameters"));
            }
        }
    }
    let sources = references(&mut d)?;
    let [a, b] = sources.as_slice() else { return Err(invalid("construction point requires two edges")) };
    if a == b || !classes.contains_key(a) || !classes.contains_key(b) {
        return Err(invalid("invalid construction-point sources"));
    }
    zeros(&mut d, 2)?;
    for cached in references(&mut d)? {
        require_class(classes, cached, "2DF7DA30-A4E4-4260-AFD1-D9C7154CB44B", 0)?;
    }
    tail(&mut d, records, owner)?;
    Ok((HistoryKind::TwoEdgePoint { definition, edges: [*a, *b], position }, name))
}

fn vector(r: &mut Reader<'_>) -> Result<Vec3> {
    let point = Vec3::new(f64::from_bits(r.u64()?), f64::from_bits(r.u64()?), f64::from_bits(r.u64()?)) * 10.;
    if !point.is_finite() {
        return Err(invalid("non-finite construction point"));
    }
    Ok(point)
}

fn geometry_record(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<(Vec3, String)> {
    let inherited = records
        .design
        .classes
        .iter()
        .find(|c| c.id == "1E5F6CE7-7544-4DE1-8C7B-6BACF6DC8E40")
        .map(|c| c.version)
        .ok_or_else(|| invalid("missing construction-point geometry schema"))?;
    if !matches!(inherited, 3 | 5) {
        return Err(invalid("unsupported construction-point geometry schema"));
    }
    let mut r = payload(records.primary(id)?, id)?;
    empty(&mut r)?;
    zeros(&mut r, 1)?;
    let attributes = super::attributes::read(&mut r)?;
    if attributes.iter().any(|(name, value)| name != "EntityGenesis" || *value != 0) {
        return Err(invalid("unsupported construction-point attributes"));
    }
    let position = vector(&mut r)?;
    let parent = if inherited == 5 {
        if r.wide_string()?.is_empty() {
            return Err(invalid("missing construction-point identity"));
        }
        Some(r.reference()?)
    } else {
        None
    };
    if r.u32()? == 0 {
        return Err(invalid("unsupported construction-point ownership"));
    }
    zeros(&mut r, 5)?;
    if r.reference()? != owner {
        return Err(invalid("construction-point geometry has a foreign owner"));
    }
    if r.take(2)? != [0, u8::from(inherited == 5)] {
        return Err(invalid("unsupported construction-point naming flags"));
    }
    let name = r.wide_string()?;
    r.take(if inherited == 5 { 30 } else { 29 })?; // versioned presentation fields
    let trailing_parent = r.reference()?;
    require_class(classes, trailing_parent, "A8635B5F-1E2C-45FD-8BA9-0173B0FEB75E", if inherited == 5 { 3 } else { 1 })?;
    if parent.is_some_and(|p| p != trailing_parent) {
        return Err(invalid("inconsistent construction-point parent"));
    }
    finish(&r)?;
    Ok((position, name))
}

fn tail(r: &mut Reader<'_>, records: &Records<'_>, owner: u64) -> Result<()> {
    let version = records
        .design
        .classes
        .iter()
        .find(|c| c.id == "A0A15D26-1F3B-4120-A3F1-9CDDA189AB74")
        .map(|c| c.version)
        .unwrap_or(if records.design.layout == 8 { 2 } else { 0 });
    r.take(match version {
        1 => 59,
        2 => 60,
        _ => return Err(invalid("unsupported construction-point inherited schema")),
    })?;
    if r.reference()? != owner {
        return Err(invalid("construction-point source has a foreign owner"));
    }
    finish(r)
}

pub(super) fn sketch_edge(records: &Records<'_>, classes: &BTreeMap<u64, &Class>, id: u64, owner: u64) -> Result<(u64, u64)> {
    require_class(classes, id, "5A1BF548-241F-46FD-9FB5-E4B05126EB9D", 1)?;
    let mut r = payload(records.primary(id)?, id)?;
    zeros(&mut r, 6)?;
    let identity = r.reference()?;
    if r.u32()? != 1 || r.wide_string()?.is_empty() || r.wide_string()?.is_empty() || r.u32()? != 2 {
        return Err(invalid("unsupported construction-point sketch selection"));
    }
    super::history::selection_flag(&mut r)?;
    for cached in references(&mut r)? {
        require_class(classes, cached, "50AEE8B4-9456-4DD7-847D-6F6C909BBB9D", 0)?;
    }
    tail(&mut r, records, owner)?;
    super::projections::source_identity(records, classes, identity)
}

/// The native API requires intersecting lines. Do not silently import the
/// closest-point midpoint accepted by SolidCraft's more general TwoEdges feature.
pub(super) fn check_line(points: &[Vec3], intersection: Vec3) -> Result<()> {
    let a = points.first().ok_or_else(|| invalid("empty construction-point source"))?;
    let b = points.last().ok_or_else(|| invalid("empty construction-point source"))?;
    let direction = (*b - *a).normalized().ok_or_else(|| invalid("degenerate construction-point source"))?;
    if points.iter().chain(std::iter::once(&intersection)).any(|p| (*p - *a).cross(direction).len() > 1e-6) {
        return Err(invalid("construction-point sources do not intersect at their recorded position"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::f3d::import_tests::Fixture;
    use crate::f3d::relationship_tests::{reference, refs, u32, wide};

    fn authored() -> (Fixture, Vec<u8>) {
        let mut f = Fixture(BTreeMap::new());
        let mut geometry = vec![0; 6];
        for n in [1.25_f64, -2., 3.5] {
            geometry.extend(n.to_le_bytes());
        }
        wide(&mut geometry, "authored point");
        reference(&mut geometry, 105);
        u32(&mut geometry, 1);
        geometry.extend([0; 5]);
        reference(&mut geometry, 100);
        geometry.extend([0, 1]);
        wide(&mut geometry, "Independent datum");
        geometry.extend([0; 30]);
        reference(&mut geometry, 105);
        f.put(101, GEOMETRY, 0, geometry, None);
        f.put(106, "1E5F6CE7-7544-4DE1-8C7B-6BACF6DC8E40", 5, vec![], None);
        f.put(105, "A8635B5F-1E2C-45FD-8BA9-0173B0FEB75E", 3, vec![], None);
        for id in [103, 104] {
            f.put(id, "5A1BF548-241F-46FD-9FB5-E4B05126EB9D", 1, vec![], None);
        }
        let mut definition = vec![0; 27];
        for n in [1.25_f64, -2., 3.5] {
            definition.extend(n.to_le_bytes());
        }
        u32(&mut definition, 7);
        for _ in 0..3 {
            definition.extend((-1_f64).to_le_bytes());
        }
        refs(&mut definition, &[103, 104]);
        definition.extend([0; 2]);
        refs(&mut definition, &[]);
        definition.extend([0; 60]);
        reference(&mut definition, 100);
        f.put(102, DEFINITION, 3, definition, None);
        let mut feature = vec![0; 6];
        feature.push(1);
        u32(&mut feature, 1);
        feature.extend([0; 4]);
        reference(&mut feature, 101);
        reference(&mut feature, 102);
        feature.push(1);
        (f, feature)
    }

    fn decode(f: Fixture, feature: &[u8]) -> Result<(HistoryKind, String)> {
        let design = f.design();
        let records = Records::new(&design);
        let classes = design.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let mut r = Reader { bytes: feature, at: 0 };
        let result = inputs(&mut r, &records, &classes, 100)?;
        finish(&r)?;
        Ok(result)
    }

    #[test]
    fn authored_point_retains_sources_name_and_cached_position() {
        let (f, bytes) = authored();
        assert_eq!(
            decode(f, &bytes).unwrap(),
            (HistoryKind::TwoEdgePoint { definition: 102, edges: [103, 104], position: Vec3::new(12.5, -20., 35.) }, "Independent datum".into())
        );
        // A second point has its own nonzero sequence; it is not an ownership flag.
        let (mut f, bytes) = authored();
        let b = &mut f.0.get_mut(&101).unwrap().2;
        let mut r = Reader { bytes: b, at: 30 };
        r.wide_string().unwrap();
        r.reference().unwrap();
        let sequence = r.at;
        b[sequence..sequence + 4].copy_from_slice(&2u32.to_le_bytes());
        assert!(decode(f, &bytes).is_ok());
    }

    #[test]
    fn point_records_reject_truncation_wrong_method_owner_and_stale_cache() {
        let (f, bytes) = authored();
        for keep in 0..bytes.len() {
            assert!(decode(authored().0, &bytes[..keep]).is_err());
        }
        for id in [101, 102] {
            for keep in 0..f.0[&id].2.len() {
                let (mut f, bytes) = authored();
                f.0.get_mut(&id).unwrap().2.truncate(keep);
                assert!(decode(f, &bytes).is_err(), "{id}/{keep}");
            }
        }
        for (offset, value) in [(51, 8), (49, 1), (84, 104)] {
            let (mut f, bytes) = authored();
            f.0.get_mut(&102).unwrap().2[offset] = value;
            assert!(decode(f, &bytes).is_err(), "{offset}");
        }
        let (mut f, bytes) = authored();
        let b = &mut f.0.get_mut(&102).unwrap().2;
        let owner = b.len() - 10;
        b[owner] = 99;
        assert!(decode(f, &bytes).is_err());
    }

    #[test]
    fn legacy_point_keeps_its_sources_and_ignores_only_unused_bounding_box_bytes() {
        for legacy in [false, true] {
            for bounding_box in [0_u32, 0x211d0000, u32::MAX] {
                let (mut f, feature) = authored();
                f.0.get_mut(&102).unwrap().2[6..10].copy_from_slice(&bounding_box.to_le_bytes());
                if legacy {
                    let definition = f.0.get_mut(&102).unwrap();
                    definition.1 = 2;
                    definition.2.drain(55..79); // no tangent-point data in v2
                    // The inherited feature schema controls the shared tail.
                    let at = definition.2.len() - 11 - 60;
                    definition.2.remove(at);
                    f.put(107, "A0A15D26-1F3B-4120-A3F1-9CDDA189AB74", 1, vec![], None);
                    f.0.get_mut(&106).unwrap().1 = 3;
                    f.0.get_mut(&105).unwrap().1 = 1;
                    let geometry = &mut f.0.get_mut(&101).unwrap().2;
                    let mut r = Reader { bytes: geometry, at: 30 };
                    r.wide_string().unwrap();
                    r.reference().unwrap();
                    let end = r.at;
                    geometry.drain(30..end);
                    geometry[51] = 0;
                    geometry.remove(geometry.len() - 41); // legacy presentation omits one flag, after the variable-length name
                }
                let (kind, name) = decode(f, &feature).unwrap();
                assert_eq!(name, "Independent datum");
                assert_eq!(kind, HistoryKind::TwoEdgePoint { definition: 102, edges: [103, 104], position: Vec3::new(12.5, -20., 35.) });
            }
        }
        for offset in [10, 18, 26] {
            // construction parameters and pick-point reference remain strict
            let (mut f, feature) = authored();
            f.0.get_mut(&102).unwrap().2[offset] = 1;
            assert!(decode(f, &feature).is_err());
        }
        for version in [0, 1, 4, 6] {
            let (mut f, feature) = authored();
            f.0.get_mut(&106).unwrap().1 = version;
            assert!(decode(f, &feature).is_err());
        }
    }

    #[test]
    fn native_intersection_never_accepts_skew_curved_or_degenerate_sources() {
        let line = [Vec3::ZERO, Vec3::new(2., 0., 0.)];
        assert!(check_line(&line, Vec3::new(5., 0., 0.)).is_ok());
        assert!(check_line(&line, Vec3::new(0., 0., 0.1)).is_err());
        assert!(check_line(&[line[0], Vec3::new(1., 1., 0.), line[1]], Vec3::ZERO).is_err());
        assert!(check_line(&[Vec3::ZERO; 2], Vec3::ZERO).is_err());
        assert!(check_line(&[], Vec3::ZERO).is_err());
    }
}
