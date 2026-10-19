//! Observed analytic sketch profile boundaries. Other profile schemas
//! fail explicitly; selection is never guessed from curve or object order.
use super::history::{empty, finish, payload, references, require_class, zeros};
use super::*;

#[derive(Debug, PartialEq)]
pub struct CircleProfile {
    pub id: u64,
    pub feature: u64,
    pub geometry: u64,
    pub circle: u64,
}

#[derive(Debug, PartialEq, serde::Serialize)]
pub struct SketchProfile {
    pub id: u64,
    pub feature: u64,
    pub geometry: u64,
    pub regions: Vec<Vec<ProfileLoop>>,
    /// Atomic selected regions contributing to each combined boundary. An empty
    /// list denotes an ordinary, single-region selection.
    pub region_members: Vec<Vec<Vec<ProfileLoop>>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ProfileLoop {
    pub curves: Vec<u64>,
    pub reversed: Vec<bool>,
    pub pieces: Vec<(u32, u32)>,
    pub outer: bool,
}

impl Design {
    pub fn sketch_profiles(&self) -> Result<Vec<SketchProfile>> {
        if !matches!(self.layout, 4 | 5 | 8) {
            return Err(invalid("unsupported profile metadata layout"));
        }
        let Some(class) = self.classes.iter().find(|c| c.id == "4BD53E5A-0B3E-45E5-AE8B-02044306485A") else {
            return Ok(Vec::new());
        };
        if class.version != 0 {
            return Err(invalid("unsupported profile class version"));
        }
        let records = Records::new(self);
        let classes: BTreeMap<_, _> = self.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let circles = self.sketch_circles()?;
        let mut circle_ids = BTreeMap::new();
        for c in &circles {
            if let Some(identity) = c.profile_id
                && circle_ids.insert((c.sketch, identity), c.id).is_some()
            {
                return Err(invalid("ambiguous selected profile circle"));
            }
        }
        for line in self.sketch_lines()? {
            if let Some(identity) = line.profile_id
                && circle_ids.insert((line.sketch, identity), line.id).is_some()
            {
                return Err(invalid("ambiguous selected profile curve"));
            }
        }
        for arc in self.sketch_arcs()? {
            let c = arc.circle;
            if let Some(identity) = c.profile_id
                && circle_ids.insert((c.sketch, identity), c.id).is_some()
            {
                return Err(invalid("ambiguous selected profile arc"));
            }
        }
        self.recover_records(
            "profile",
            class.objects.iter().map(|id| {
                let bytes = records.primary(*id)?;
                let mut r = payload(bytes, *id)?;
                empty(&mut r)?;
                zeros(&mut r, 2)?;
                let tracking = r.reference()?;
                require_class(&classes, tracking, "0D57BD2F-D09B-43FC-AD57-1E89A118C453", 1)?;
                if r.u32()? != 1 {
                    return Err(invalid("unsupported profile source count"));
                }
                r.wide_string()?; // persistent source identity; no inferred semantics
                let geometry = r.wide_string()?.parse::<u64>().map_err(|_| invalid("unsupported profile sketch identity"))?;
                let owner = classes.get(&geometry).ok_or_else(|| invalid("missing profile sketch"))?;
                if owner.id != "44A64366-4BD3-4B24-881A-F94C206E8F2D" || !matches!(owner.version, 16..=18) {
                    return Err(invalid("unsupported profile sketch version"));
                }
                // The remaining presentation/cache fields are opaque, but the final
                // owning-feature reference is independently validated.
                let tail = bytes.len().checked_sub(11).filter(|at| *at >= r.at).ok_or_else(|| invalid("truncated profile tail"))?;
                let mut end = Reader { bytes: bytes.get(tail..).ok_or_else(|| invalid("profile tail"))?, at: 0 };
                let feature = end.reference()?;
                // A sketch owns the profiles it selects for projection; no extrusion consumes them.
                if !classes.get(&feature).is_some_and(|c| {
                    (c.id == "DD405BC2-D673-44F0-8833-5CB2A1C186C7" && c.version == 8)
                        || (c.id == super::lofts::CLASS && c.version == 9)
                        || (c.id == super::history::SKETCH && matches!(c.version, 3..=5))
                }) {
                    return Err(invalid("unsupported profile owning feature"));
                }
                let mut r = payload(records.primary(tracking)?, tracking)?;
                empty(&mut r)?;
                zeros(&mut r, 2)?;
                if r.reference()? != *id {
                    return Err(invalid("inconsistent profile tracking owner"));
                }
                let region_count = r.count(49)?;
                if region_count == 0 || region_count > 128 {
                    return Err(invalid("invalid profile region count"));
                }
                let mut regions = Vec::new();
                let mut total_curves = 0usize;
                let mut selected_regions = 0usize;
                let mut region_members = Vec::with_capacity(region_count);
                let mut region_keys = BTreeSet::new();
                let mut missing = None;
                for region_index in 0..region_count {
                    let context =
                        |error| invalid(&format!("feature {feature}, profile {id}, tracking {tracking}, region {}: {error}", region_index + 1));
                    let (loops, unresolved) = read_loops(&mut r, geometry, &circle_ids, &mut total_curves).map_err(context)?;
                    if missing.is_none() {
                        missing = unresolved.map(context);
                    }
                    let member_count = r.count(49)?;
                    selected_regions += member_count.max(1);
                    if selected_regions > 128 {
                        return Err(invalid("excessive selected profile members"));
                    }
                    let mut members = Vec::with_capacity(member_count);
                    for member_index in 0..member_count {
                        let member_context = |error| context(invalid(&format!("member {}: {error}", member_index + 1)));
                        let (member, unresolved) = read_loops(&mut r, geometry, &circle_ids, &mut total_curves).map_err(member_context)?;
                        if missing.is_none() {
                            missing = unresolved.map(member_context);
                        }
                        members.push(member);
                    }
                    region_members.push(members);
                    let mut key: Vec<_> = loops
                        .iter()
                        .map(|l| {
                            let mut curves: Vec<_> = l.curves.iter().copied().zip(l.pieces.iter().copied()).collect();
                            curves.sort();
                            (l.outer, curves)
                        })
                        .collect();
                    key.sort();
                    // Missing curves use an internal placeholder. Only resolved
                    // boundaries can be compared here; the verified snapshot
                    // replaces the complete selection when tracking is stale.
                    if loops.iter().all(|l| !l.curves.contains(&0)) && !region_keys.insert(key) {
                        return Err(invalid("duplicate selected profile region"));
                    }
                    regions.push(loops);
                }
                finish(&r)?;
                if let Some(error) = missing {
                    #[cfg(not(target_arch = "wasm32"))]
                    if self.archive.is_some() {
                        regions = super::profile_cache::resolve(self, *id, geometry, &circle_ids)
                            .map_err(|cache| invalid(&format!("{error}; current profile snapshot: {cache}")))?;
                        region_members = vec![Vec::new(); regions.len()];
                    } else {
                        return Err(error);
                    }
                    #[cfg(target_arch = "wasm32")]
                    return Err(error);
                }
                Ok(SketchProfile { id: *id, feature, geometry, regions, region_members })
            }),
        )
    }

    /// Compatibility view for callers specifically inspecting circular profiles.
    pub fn circle_profiles(&self) -> Result<Vec<CircleProfile>> {
        let circles: BTreeSet<_> = self.sketch_circles()?.into_iter().map(|c| c.id).collect();
        self.sketch_profiles()?
            .into_iter()
            .map(|p| {
                let [loops] = p.regions.as_slice() else {
                    return Err(invalid("profile contains multiple regions"));
                };
                let [boundary] = loops.as_slice() else {
                    return Err(invalid("profile contains holes"));
                };
                let [circle] = boundary.curves.as_slice() else {
                    return Err(invalid("profile is not a single circle"));
                };
                if !circles.contains(circle) || boundary.pieces.as_slice() != [(1, 1)] {
                    return Err(invalid("profile is not a circle"));
                }
                Ok(CircleProfile { id: p.id, feature: p.feature, geometry: p.geometry, circle: *circle })
            })
            .collect()
    }

    /// Resolve an extrusion's container through explicit profile references.
    pub fn selected_circle_profiles(&self, feature: u64, containers: &[u64]) -> Result<Vec<CircleProfile>> {
        let records = Records::new(self);
        let profiles: BTreeMap<_, _> = self.circle_profiles()?.into_iter().map(|p| (p.id, p)).collect();
        let classes: BTreeMap<_, _> = self.classes.iter().flat_map(|c| c.objects.iter().map(move |id| (*id, c))).collect();
        let mut selected = Vec::new();
        let mut seen = BTreeSet::new();
        for id in containers {
            require_class(&classes, *id, "2CA5A1CD-C99B-4C9B-91AF-57989148E841", 1)?;
            let bytes = records.primary(*id)?;
            let mut r = payload(bytes, *id)?;
            empty(&mut r)?;
            zeros(&mut r, 2)?;
            let ids = references(&mut r)?;
            let tail = bytes.len().checked_sub(11).filter(|at| *at >= r.at).ok_or_else(|| invalid("truncated profile container"))?;
            let mut r = Reader { bytes: bytes.get(tail..).ok_or_else(|| invalid("profile container tail"))?, at: 0 };
            if r.reference()? != feature {
                return Err(invalid("inconsistent profile container owner"));
            }
            for id in ids {
                if !seen.insert(id) {
                    return Err(invalid("duplicate selected profile"));
                }
                let p = profiles.get(&id).filter(|p| p.feature == feature).ok_or_else(|| invalid("missing selected profile or wrong owner"))?;
                selected.push(CircleProfile { id: p.id, feature: p.feature, geometry: p.geometry, circle: p.circle });
            }
        }
        Ok(selected)
    }
}

fn read_loops(
    r: &mut Reader<'_>,
    geometry: u64,
    circle_ids: &BTreeMap<(u64, (u64, u64)), u64>,
    total_curves: &mut usize,
) -> Result<(Vec<ProfileLoop>, Option<IoError>)> {
    let loop_count = r.count(45)?;
    if loop_count == 0 || loop_count > 129 {
        return Err(invalid("invalid profile loop count"));
    }
    let mut loops = Vec::new();
    let mut seen = BTreeSet::new();
    let mut missing = None;
    for _ in 0..loop_count {
        let count = r.count(40)?;
        *total_curves += count;
        if count == 0 || count > 512 || *total_curves > 20_000 {
            return Err(invalid("empty or excessive profile loop"));
        }
        let mut curves = Vec::new();
        let mut reversed = Vec::new();
        let mut pieces = Vec::new();
        for _ in 0..count {
            if r.u32()? != 3 {
                return Err(invalid("unsupported profile curve tracking"));
            }
            let identity = (r.u64()?, r.u64()?);
            reversed.push(match r.u32()? {
                0 => false,
                1 => true,
                _ => return Err(invalid("unsupported profile curve orientation")),
            });
            let piece = r.u32()?;
            let total = r.u32()?;
            if piece == 0 || piece > total || total > 50_000 {
                return Err(invalid("invalid profile piece index or count"));
            }
            pieces.push((piece, total));
            zeros(r, 8)?;
            let curve = circle_ids.get(&(geometry, identity)).copied();
            if curve.is_none() && missing.is_none() {
                missing = Some(invalid(&format!(
                    "missing selected profile curve ({}, {}) in sketch geometry {geometry}, piece {piece}/{total}",
                    identity.0, identity.1
                )));
            }
            if !seen.insert((identity, piece)) {
                return Err(invalid("repeated profile curve"));
            }
            // Never exposed: unresolved selections require a verified snapshot.
            curves.push(curve.unwrap_or(0));
        }
        let outer = match r.take(1)? {
            [0] => false,
            [1] => true,
            _ => return Err(invalid("invalid profile outer flag")),
        };
        loops.push(ProfileLoop { curves, reversed, pieces, outer });
    }
    if loops.iter().filter(|l| l.outer).count() != 1 {
        return Err(invalid("profile must have exactly one outer loop"));
    }
    Ok((loops, missing))
}
