//! Bounded native geometry fallback via cadmpeg's Apache-2.0 codecs.
use super::*;
use cadmpeg_codec_f3d::F3dCodec;
use cadmpeg_codec_step::StepCodec;
use cadmpeg_ir::CadIr;
use cadmpeg_ir::codec::write::{EncodeInput, Encoder, target::TargetRequest};
use cadmpeg_ir::codec::{Codec, DecodeOptions};
use cadmpeg_ir::scalar::PositiveLength;
use solidcraft_doc::{Document, Model};

pub(super) fn recover(bytes: &[u8], name: &str) -> Result<(Document, Vec<String>)> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(bytes, name))).map_err(|_| invalid("native body decoder stopped unexpectedly"))?
}

fn decode(bytes: &[u8], name: &str) -> Result<(Document, Vec<String>)> {
    let mut options = DecodeOptions::default();
    options.policy.limits.max_input_bytes = MAX_F3D_BYTES as u64;
    options.policy.limits.max_decompressed_bytes_total = MAX_EXPANDED;
    options.policy.limits.max_retained_bytes = 2 << 30;
    let mut warnings = Vec::new();
    let decoded = F3dCodec.decode(&mut Cursor::new(bytes), &options).map_err(|e| invalid(&format!("native bodies: {e}")))?;
    warnings.extend(decoded.report().losses.iter().take(1000).map(|l| format!("Native decoder: {}", l.message)));
    let (mut ir, _, _) = decoded.into_parts();
    declare_cache_accuracy(&mut ir);
    let plan = StepCodec::default()
        .plan(EncodeInput::new(&ir, None), TargetRequest::Explicit("ap214"))
        .map_err(|e| invalid(&format!("native body conversion: {e}")))?;
    warnings.extend(plan.report().losses.iter().take(1000).map(|l| format!("Body conversion: {}", l.message)));
    let mut step = Vec::new();
    plan.write_to(&mut step).map_err(|e| invalid(&format!("native body conversion: {e}")))?;
    let imported = crate::step_import_feature(&step, &format!("{name}.step"))?;
    // Geometric defects must be visible before potentially long native-record
    // omission lists, especially when the recovered STEP contains open shells.
    warnings.splice(0..0, imported.warnings);
    let mut doc = Document::new(name);
    let id = doc.add_feature(imported.kind, Some("Recovered native bodies"))?;
    if let Some(feature) = doc.feature_mut(id) {
        feature.body_names = imported.body_names;
    }
    let mut model = Model::new();
    model.evaluate(&doc);
    if let Some(error) = model.results.iter().find_map(|r| r.error.as_ref()) {
        return Err(invalid(&format!("native body regeneration: {error}")));
    }
    if model.state().bodies.is_empty() {
        return Err(invalid("native decoder recovered no bodies"));
    }
    warnings.push("Recovered native bodies have no editable source history. Geometry conversion is incomplete and has not been verified against the original final shape.".into());
    Ok((doc, warnings))
}

/// Procedural curves and surfaces are written as their cached approximations, which are
/// only as accurate as their fit tolerance. Declare the model uncertainty accordingly so
/// the STEP importer does not reject vertices that lie within that tolerance.
fn declare_cache_accuracy(ir: &mut CadIr) {
    let curves = ir.model.procedural_curves.iter().filter_map(|c| c.cache_fit_tolerance());
    let surfaces = ir.model.procedural_surfaces.iter().filter_map(|s| s.cache_fit_tolerance());
    let fit = curves.chain(surfaces).map(|t| t.get()).filter(|t| t.is_finite()).fold(0., f64::max);
    if fit > ir.tolerances.linear.get()
        && let Some(linear) = PositiveLength::new(fit)
    {
        ir.tolerances.linear = linear;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadmpeg_ir::geometry::surface_payloads::LawSurfacePayload;
    use cadmpeg_ir::geometry::{LawFormula, LawSurfaceConstruction, LawSurfaceTail, LegacyCache, ProceduralSurface, ProceduralSurfaceDefinition};
    use cadmpeg_ir::ids::ProceduralSurfaceId;

    /// Original synthetic surface whose cached approximation states `fit` mm.
    fn cached_surface(name: &str, fit: f64) -> ProceduralSurface {
        let construction = LawSurfaceConstruction {
            parameter_ranges: None,
            primary: LawFormula::Null {},
            additional: Vec::new(),
            tail: LawSurfaceTail::Full { cache: LegacyCache::try_new(fit).unwrap() },
            discontinuities: std::array::from_fn(|_| Vec::new()),
        };
        let definition = ProceduralSurfaceDefinition::Law(LawSurfacePayload::try_new(Box::new(construction)).unwrap());
        ProceduralSurface::new(ProceduralSurfaceId::mint(name).unwrap(), definition, None)
    }

    #[test]
    fn declared_uncertainty_covers_cached_approximations() {
        let mut ir = CadIr::empty();
        let model = ir.tolerances.linear.get();
        declare_cache_accuracy(&mut ir);
        assert_eq!(ir.tolerances.linear.get(), model, "nothing cached keeps the model tolerance");
        ir.model.procedural_surfaces.push(cached_surface("synthetic:test:tight#0", model / 10.));
        declare_cache_accuracy(&mut ir);
        assert_eq!(ir.tolerances.linear.get(), model, "a tighter cache never lowers it");
        ir.model.procedural_surfaces.push(cached_surface("synthetic:test:loose#1", 0.0044));
        declare_cache_accuracy(&mut ir);
        assert_eq!(ir.tolerances.linear.get(), 0.0044);
    }
}
