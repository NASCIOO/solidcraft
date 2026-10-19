//! Versioned sketch-entity metadata. Unknown relationships remain explicit.
use super::history::{references, require_class};
use super::*;

pub(super) const POINT: &str = "C2CEDAE7-1716-47C1-B7B1-07B70081D0FB";
pub(super) const SPECIAL_POINT: &str = "00D157C2-2332-4BD5-BB53-048C4081D1C3";
pub(super) const LINE: &str = "DCA267ED-D615-4934-B64F-AD805E8003E2";
pub(super) const SPECIAL_LINE: &str = "16DEFC4D-1816-4FB0-8E39-9BDA23954248";
const GEOMETRY: &str = "44A64366-4BD3-4B24-881A-F94C206E8F2D";

/// References whose reconstruction semantics have not yet been established.
/// Inspection retains them; importing them as independent geometry is forbidden.
#[derive(Debug, Default, PartialEq)]
pub struct EntityMetadata {
    pub specialized: bool,
    /// Boolean following the linked-point flag; its meaning remains unverified.
    pub point_auxiliary: bool,
    /// Versioned point flags before fixation, retained without inferred meaning.
    pub point_prefix: [u8; 3],
    pub definition: Vec<u64>,
    pub relationships: Vec<u64>,
    pub legacy: Vec<u64>,
}
impl EntityMetadata {
    pub(super) fn validate(&self, classes: &BTreeMap<u64, &Class>) -> Result<()> {
        for id in self.definition.iter().chain(&self.relationships).chain(&self.legacy) {
            if !classes.contains_key(id) {
                return Err(invalid("missing sketch entity relationship"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) struct Schema {
    pub geometry: u32,
    pub curve: u32,
}
impl Schema {
    pub fn read(d: &Design) -> Result<Self> {
        if !matches!(d.layout, 4 | 5 | 8) {
            return Err(invalid("unsupported sketch entity layout"));
        }
        if d.classes.iter().any(|c| c.id == "8E9D588C-28EF-43C4-B9A6-AA5430A68D4B" && c.version != 2) {
            return Err(invalid("unsupported inherited point schema"));
        }
        let geometry = d.classes.iter().find(|c| c.id == GEOMETRY).map(|c| c.version).ok_or_else(|| invalid("missing sketch geometry schema"))?;
        let curve =
            d.classes.iter().find(|c| c.id == "9FF71F46-205F-484B-A2B5-A245D0D8FE33").map(|c| c.version).unwrap_or(if d.layout == 8 { 9 } else { 0 });
        if !matches!(geometry, 16..=18) || !matches!(curve, 8 | 9) {
            return Err(invalid("unsupported inherited sketch entity schema"));
        }
        Ok(Self { geometry, curve })
    }
    pub fn owner(&self, r: &mut Reader<'_>, metadata: &mut EntityMetadata) -> Result<u64> {
        // Some v17 and v18 records end directly with the owning sketch reference.
        // Others retain a relationship list before it. Only the exact
        // terminal-reference envelope may omit that list.
        if matches!(self.geometry, 17 | 18) && r.bytes.len().saturating_sub(r.at) != 11 {
            metadata.legacy = references(r)?;
        }
        r.reference()
    }
    pub fn curve_tail(&self, r: &mut Reader<'_>, metadata: &mut EntityMetadata) -> Result<u64> {
        r.take(4)?;
        metadata.relationships = references(r)?;
        r.take(26)?; // presentation fields
        self.owner(r, metadata)
    }
    pub fn require_owner(&self, classes: &BTreeMap<u64, &Class>, owner: u64) -> Result<()> {
        require_class(classes, owner, GEOMETRY, self.geometry)
    }
}

pub(super) fn require_point(classes: &BTreeMap<u64, &Class>, id: u64) -> Result<()> {
    let class = classes.get(&id).ok_or_else(|| invalid("missing sketch point"))?;
    match (class.id.as_str(), class.version) {
        (POINT, 11) => Ok(()),
        (SPECIAL_POINT, 0) if class.parent == POINT => Ok(()),
        _ => Err(invalid("unsupported sketch point reference")),
    }
}
