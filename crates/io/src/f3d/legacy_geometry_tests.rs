//! Original field-authored geometry records, never captured Fusion payloads.
use super::import_tests::Fixture;
use super::polygon_tests::rectangle;
use super::relationship_tests::{reference, refs};
use super::*;

fn legacy(mut f: Fixture, geometry: u32) -> Design {
    for (guid, version, bytes, _) in f.0.values_mut() {
        match guid.as_str() {
            entity::LINE | "F0130424-8B7E-4092-93C9-1CA807482534" => {
                // Curve base v8 lacks one byte preceding the construction flag.
                let n = bytes.len();
                bytes.remove(n - 48);
            }
            "44A64366-4BD3-4B24-881A-F94C206E8F2D" => *version = geometry,
            _ => continue,
        }
    }
    if geometry == 17 {
        for (guid, _, bytes, _) in f.0.values_mut() {
            if matches!(guid.as_str(), entity::POINT | entity::LINE | "F0130424-8B7E-4092-93C9-1CA807482534") {
                let n = bytes.len();
                bytes.splice(n - 11..n - 11, [0; 4]);
            }
        }
    }
    f.put(999, "9FF71F46-205F-484B-A2B5-A245D0D8FE33", 8, vec![], None);
    let mut d = f.design();
    d.layout = if geometry == 16 { 4 } else { 5 };
    d
}

fn legacy_membership(entries: &[(u64, u32)]) -> Design {
    legacy_membership_with(entries, false)
}

/// `context`: later version-17 streams keep the v18 reference to the design's
/// sketch context object in the placement instead of dropping it.
fn legacy_membership_with(entries: &[(u64, u32)], context: bool) -> Design {
    let mut f = super::import_tests::fixture(false);
    if context {
        f.put(1002, "5275CBA4-3D7D-40DC-A651-1DFF3FC8AFBF", 4, vec![], None);
    }
    let sketch = f.0.get_mut(&8).unwrap();
    sketch.1 = 4;
    sketch.2.drain(49..53);
    let secondary = f.0.get_mut(&70).unwrap().3.as_mut().unwrap();
    if context {
        let mut shared = Vec::new();
        reference(&mut shared, 1002);
        secondary.splice(21..32, shared);
    } else {
        secondary.drain(21..32);
    }
    let mut table = Vec::new();
    super::relationship_tests::u32(&mut table, entries.len() as u32);
    for (id, flags) in entries {
        reference(&mut table, *id);
        super::relationship_tests::u32(&mut table, *flags);
    }
    let at = secondary.len() - 11;
    secondary.splice(at..at, table);
    let graph = &mut f.0.get_mut(&70).unwrap().2;
    let mut r = Reader { bytes: graph, at: 5 };
    attributes::read(&mut r).unwrap();
    let start = r.at;
    r.wide_string().unwrap();
    let end = r.at;
    // The empty external identity follows the owner in v17.
    graph.drain(start..end);
    let at = graph.len() - 3;
    graph.splice(at..at, [0; 4]);
    f.put(1000, "A0A15D26-1F3B-4120-A3F1-9CDDA189AB74", 2, vec![], None);
    f.put(1001, "2EDC2536-D95B-4469-8ACA-DC07124D9C43", 2, vec![], None);
    for (guid, version, bytes, _) in f.0.values_mut() {
        if guid == "7A6A3D31-BE74-4E19-9115-20642944C2E4" {
            // Parameter v7 omits four inherited bytes and one expression byte.
            let mut r = Reader { bytes, at: 16 };
            r.wide_string().unwrap();
            r.u32().unwrap();
            r.reference().unwrap();
            r.wide_string().unwrap();
            let at = r.at;
            bytes.remove(at);
            bytes.drain(12..16);
            *version = 7;
        }
    }
    legacy(f, 17)
}

#[test]
fn complete_legacy_membership_preserves_driving_dimensions_and_saved_replay() {
    let d = legacy_membership(&[(56, 1), (31, 1), (41, 1), (42, 1)]);
    let mut doc = d.to_document("Legacy membership").unwrap();
    super::import_tests::volume(&doc, std::f64::consts::PI * 30. * 30. * 17.);
    doc.change_param("d7", "40 mm", None, None).unwrap();
    doc.change_param("height", "d7 / 2", None, None).unwrap();
    super::import_tests::volume(&doc, std::f64::consts::PI * 20. * 20. * 20.);
    let reopened = crate::read_design(&crate::write_design(&doc)).unwrap();
    assert_eq!(doc, reopened);
    super::import_tests::volume(&reopened, std::f64::consts::PI * 20. * 20. * 20.);
}

#[test]
fn later_legacy_placements_keep_their_sketch_context_reference() {
    let entries = [(56, 1), (31, 1), (41, 1), (42, 1)];
    let expected = legacy_membership(&entries).to_document("Legacy membership").unwrap();
    let doc = legacy_membership_with(&entries, true).to_document("Legacy membership").unwrap();
    assert_eq!(doc, expected);
}

#[test]
fn ambiguous_legacy_membership_is_not_silently_discarded() {
    for entries in [
        vec![(31, 1)],
        vec![(56, 1), (31, 1), (41, 1), (42, 1), (42, 1)],
        vec![(56, 1), (31, 1), (41, 1), (43, 1)],
        vec![(56, 1), (31, 2), (41, 1), (42, 1)],
    ] {
        let error = legacy_membership(&entries).to_document("Ambiguous membership").unwrap_err();
        assert!(error.to_string().contains("legacy"), "{error}");
    }
}

#[test]
fn legacy_geometry_versions_preserve_construction_and_coordinates() {
    for polygon in [true, false] {
        let fixture = || if polygon { rectangle() } else { super::import_tests::fixture(false) };
        for geometry in [16, 17] {
            let mut original = fixture();
            let curve = &mut original.0.get_mut(&31).unwrap().2;
            let at = curve.len() - 46;
            curve[at] = 1;
            let modern = original.design();
            let mut old = fixture();
            let curve = &mut old.0.get_mut(&31).unwrap().2;
            let at = curve.len() - 46;
            curve[at] = 1;
            let old = legacy(old, geometry);
            assert_eq!(old.sketch_points().unwrap(), modern.sketch_points().unwrap());
            assert_eq!(old.sketch_lines().unwrap(), modern.sketch_lines().unwrap());
            assert_eq!(old.sketch_circles().unwrap(), modern.sketch_circles().unwrap());
        }
    }
}

#[test]
fn version_17_entities_can_end_in_an_owner_without_a_relationship_list() {
    for polygon in [false, true] {
        let fixture = || if polygon { rectangle() } else { super::import_tests::fixture(false) };
        let modern = fixture().design();
        let mut old = legacy(fixture(), 16); // same terminal-owner form, no metadata list
        old.layout = 5;
        old.classes.iter_mut().find(|c| c.id == "44A64366-4BD3-4B24-881A-F94C206E8F2D").unwrap().version = 17;
        assert_eq!(old.sketch_points().unwrap(), modern.sketch_points().unwrap());
        assert_eq!(old.sketch_lines().unwrap(), modern.sketch_lines().unwrap());
        assert_eq!(old.sketch_circles().unwrap(), modern.sketch_circles().unwrap());
    }
    // Metadata is still decoded when present; the short form is not a
    // fallback for a malformed list or a reference to another class.
    let schema = entity::Schema { geometry: 17, curve: 8 };
    let mut b = Vec::new();
    refs(&mut b, &[900]);
    reference(&mut b, 70);
    let mut metadata = EntityMetadata::default();
    let mut reader = Reader { bytes: &b, at: 0 };
    assert_eq!(schema.owner(&mut reader, &mut metadata).unwrap(), 70);
    assert_eq!(metadata.legacy, [900]);
    super::history::finish(&reader).unwrap();
    for keep in 0..11 {
        let mut metadata = EntityMetadata::default();
        let owner = &b[b.len() - 11..];
        assert!(schema.owner(&mut Reader { bytes: &owner[..keep], at: 0 }, &mut metadata).is_err());
    }
}

#[test]
#[ignore = "requires the local native knob corpus"]
fn native_knob_entities_decode_both_version_17_owner_envelopes() {
    let root = std::path::PathBuf::from(solidcraft_geom::env::var_os("F3D_ADDITIONAL_CORPUS").unwrap()).join("knobs");
    for name in ["Crescent", "Eye", "Narrow", "Square", "Style 3", "Style 4", "Style 5", "Style 6", "style-1"] {
        let designs = read_f3d(&std::fs::read(root.join(format!("{name}.f3d"))).unwrap()).unwrap();
        let d = &designs[0];
        assert!(!d.sketch_points().unwrap_or_else(|e| panic!("{name} points: {e}")).is_empty());
        d.sketch_lines().unwrap_or_else(|e| panic!("{name} lines: {e}"));
        d.sketch_circles().unwrap_or_else(|e| panic!("{name} circles: {e}"));
    }
}

fn specialized() -> Fixture {
    let mut f = rectangle();
    for id in [41, 31] {
        let record = f.0.get_mut(&id).unwrap();
        let mut r = Reader { bytes: &record.2, at: 4 };
        r.take(1).unwrap();
        attributes::read(&mut r).unwrap();
        let at = r.at;
        let mut primary = record.2[..at].to_vec();
        let mut secondary = vec![0; 4];
        secondary.extend(&record.2[at..]);
        if id == 31 {
            r.take(96).unwrap();
            let end = r.reference().unwrap();
            let start = r.reference().unwrap();
            reference(&mut primary, end);
            reference(&mut primary, 70); // opaque definition retained explicitly
            reference(&mut primary, start);
            record.0 = entity::SPECIAL_LINE.into();
        } else {
            record.0 = entity::SPECIAL_POINT.into();
        }
        record.1 = 0;
        record.2 = primary;
        record.3 = Some(secondary);
    }
    f
}

fn design(f: Fixture) -> Design {
    let mut d = f.design();
    for class in &mut d.classes {
        match class.id.as_str() {
            entity::SPECIAL_LINE => class.parent = entity::LINE.into(),
            entity::SPECIAL_POINT => class.parent = entity::POINT.into(),
            _ => {}
        }
    }
    d
}

#[test]
fn split_geometry_retains_definitions_and_rejects_truncated_or_inconsistent_records() {
    let ordinary = rectangle().design();
    let d = design(specialized());
    let points = d.sketch_points().unwrap();
    let p = points.iter().find(|p| p.id == 41).unwrap();
    assert!(p.metadata.specialized);
    assert_eq!(p.position, ordinary.sketch_points().unwrap().iter().find(|p| p.id == 41).unwrap().position);
    let lines = d.sketch_lines().unwrap();
    let l = lines.iter().find(|l| l.id == 31).unwrap();
    assert!(l.metadata.specialized);
    assert_eq!(l.metadata.definition, [70]);
    let base = ordinary.sketch_lines().unwrap();
    assert_eq!((l.start, l.end), (base[0].start, base[0].end));
    assert!(d.to_document("Keep dependencies").unwrap_err().to_string().contains("entity relationship"));
    for id in [31, 41] {
        for secondary in [false, true] {
            let f = specialized();
            let record = &f.0[&id];
            let length = if secondary { record.3.as_ref().unwrap().len() } else { record.2.len() };
            for n in 0..length {
                let mut f = specialized();
                let record = f.0.get_mut(&id).unwrap();
                if secondary {
                    record.3.as_mut().unwrap().truncate(n);
                } else {
                    record.2.truncate(n);
                }
                let d = design(f);
                assert!(if id == 31 { d.sketch_lines().is_err() } else { d.sketch_points().is_err() }, "{id}/{secondary}/{n}");
            }
        }
    }
    let mut f = specialized();
    let primary = &mut f.0.get_mut(&31).unwrap().2;
    let at = primary.len() - 10;
    primary[at..at + 8].copy_from_slice(&72_u64.to_le_bytes());
    assert!(design(f).sketch_lines().unwrap_err().to_string().contains("endpoint references disagree"));
}

#[test]
fn nonempty_curve_relationships_remain_visible_and_cannot_be_flattened() {
    let mut f = rectangle();
    let bytes = &mut f.0.get_mut(&31).unwrap().2;
    let mut related = Vec::new();
    refs(&mut related, &[70]);
    let at = bytes.len() - 41;
    bytes.splice(at..at + 4, related);
    let d = f.design();
    assert_eq!(d.sketch_lines().unwrap()[0].metadata.relationships, [70]);
    assert!(d.to_document("Preserve dependencies").unwrap_err().to_string().contains("entity relationship"));
}

#[test]
fn unknown_point_flags_are_retained_without_inventing_constraints() {
    let mut f = rectangle();
    let point = &mut f.0.get_mut(&41).unwrap().2;
    let at = point.len() - 81;
    point[at] = 1;
    point[at - 6] = 1;
    let d = f.design();
    assert_eq!(d.sketch_points().unwrap().iter().find(|p| p.id == 41).unwrap().metadata.point_prefix, [0, 1, 0]);
    assert!(d.sketch_points().unwrap().iter().find(|p| p.id == 41).unwrap().metadata.point_auxiliary);
    assert!(d.to_document("Keep point semantics").unwrap_err().to_string().contains("entity relationship"));
}

#[test]
fn reversed_support_line_axis_preserves_endpoint_order_and_editable_profile() {
    let mut f = rectangle();
    let bytes = &mut f.0.get_mut(&31).unwrap().2;
    let mut r = Reader { bytes, at: 5 };
    attributes::read(&mut r).unwrap();
    let at = r.at + 48;
    for i in 0..3 {
        let range = at + i * 8..at + (i + 1) * 8;
        let value = f64::from_le_bytes(bytes[range.clone()].try_into().unwrap());
        bytes[range].copy_from_slice(&(-value).to_le_bytes());
    }
    let mut doc = f.design().to_document("Reversed support axis").unwrap();
    super::import_tests::volume(&doc, 30. * 20. * 17.);
    doc.change_param("width", "45 mm", None, None).unwrap();
    super::import_tests::volume(&doc, 45. * 20. * 17.);
    let mut f = rectangle();
    let bytes = &mut f.0.get_mut(&31).unwrap().2;
    for (i, value) in [0_f64, 0., 1.].iter().enumerate() {
        bytes[at + i * 8..at + (i + 1) * 8].copy_from_slice(&value.to_le_bytes());
    }
    assert!(f.design().sketch_lines().is_err());
}

/// Sketch features keep version 4 when their geometry is upgraded to v18, and
/// v18 placement and entity records may carry the v17 tables, here empty.
#[test]
fn version_4_sketches_over_v18_geometry_accept_empty_relationship_tables() {
    let mut f = super::import_tests::fixture(false);
    let sketch = f.0.get_mut(&8).unwrap();
    sketch.1 = 4;
    sketch.2.drain(49..53);
    let placement = f.0.get_mut(&70).unwrap().3.as_mut().unwrap();
    let at = placement.len() - 11;
    placement.splice(at..at, [0; 4]);
    for (guid, _, bytes, _) in f.0.values_mut() {
        if matches!(guid.as_str(), entity::POINT | entity::LINE | "F0130424-8B7E-4092-93C9-1CA807482534") {
            let n = bytes.len();
            bytes.splice(n - 11..n - 11, [0; 4]);
        }
    }
    let d = f.design();
    assert_eq!(d.classes.iter().find(|c| c.id == "44A64366-4BD3-4B24-881A-F94C206E8F2D").unwrap().version, 18);
    let modern = super::import_tests::fixture(false).design();
    assert_eq!(d.sketch_points().unwrap(), modern.sketch_points().unwrap());
    assert_eq!(d.sketch_lines().unwrap(), modern.sketch_lines().unwrap());
    assert_eq!(d.sketch_circles().unwrap(), modern.sketch_circles().unwrap());
    let doc = d.to_document("Upgraded geometry").unwrap();
    super::import_tests::volume(&doc, std::f64::consts::PI * 30. * 30. * 17.);
}
