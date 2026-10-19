//! A design opened from the model its file keeps is the design evaluated again: the same
//! results, the same bodies (to the bit), the same face names and volumes after every feature,
//! and the same after an edit made to both.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use solidcraft_engine::Session;
use solidcraft_engine::doc::{ModelBody, ModelState};

fn corpus() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut v: Vec<PathBuf> =
        std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "solidcraft")).collect();
    v.sort();
    v
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("solidcraft-model-cache-{}-{name}.solidcraft", std::process::id()))
}

fn open(path: &Path) -> Session {
    let mut s = Session::default();
    s.execute("doc.open", &json!({"path": path.to_string_lossy()})).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    s
}

fn body(name: &str, b: &ModelBody) -> Value {
    let measured = solidcraft_engine::kernel::measure(&b.body).ok();
    json!({
        "name": b.name, "feature": b.feature, "component": b.component, "source": b.source_name, "transferred": b.transferred,
        "brep": solidcraft_engine::kernel::encode_native_body(&b.body).map(|bytes| format!("{:x}", sha(&bytes))).unwrap_or_else(|e| format!("{name}: {e}")),
        "faces": *solidcraft_engine::doc::naming::face_names(b),
        "volume": measured.as_ref().map(|m| m.volume.to_bits()), "area": measured.as_ref().map(|m| m.area.to_bits()),
        "look": b.imported.as_ref().map(|l| format!("{l:?}")),
    })
}

fn sha(bytes: &[u8]) -> u128 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let a = h.finish();
    bytes.len().hash(&mut h);
    (u128::from(a) << 64) | u128::from(h.finish())
}

fn state(name: &str, st: &Arc<ModelState>) -> Value {
    json!({
        "bodies": st.bodies.iter().map(|b| body(name, b)).collect::<Vec<_>>(),
        "sketches": st.sketches.iter().map(|s| json!([s.feature, s.name, s.plane, s.sketch, s.report, s.profiles.len()])).collect::<Vec<_>>(),
        "counter": st.body_counter, "sheets": st.sheets, "threads": st.threads, "construct": format!("{:?}", st.construct),
    })
}

/// Everything a model holds that the rest of the app reads.
fn model(name: &str, s: &Session) -> Value {
    let results: Vec<Value> = s
        .model
        .results
        .iter()
        .map(|r| json!({"id": r.id, "name": r.name, "error": r.error, "warning": r.warning, "skipped": r.skipped, "state": state(name, &r.state)}))
        .collect();
    json!({"results": results, "world": state(name, &s.world_state())})
}

/// The same edit on both: a parameter's value when the design has one, else a new body.
fn edit(s: &mut Session) {
    let param = s.doc.params.first().map(|p| (p.name.clone(), p.expr.clone()));
    match param {
        Some((name, expr)) => {
            s.execute("parameters.change", &json!({"name": name, "expression": format!("({expr}) * 1.05")})).unwrap();
        }
        None => {
            s.execute("solid.box", &json!({"length": 7, "width": 8, "height": 9, "operation": "new"})).unwrap();
        }
    }
}

fn check(name: &str, mut built: Session) {
    let path = temp(name);
    built.execute("file.save_as", &json!({"path": path.to_string_lossy()})).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(solidcraft_engine::io::read_design_geometry(&bytes).is_some(), "{name}: the save keeps the model");
    let mut cached = open(&path);
    assert_eq!(cached.model.last_recomputed, 0, "{name}: opened without evaluating");
    let mut rebuilt = Session::new(solidcraft_engine::io::read_design(&bytes).unwrap());
    assert!(rebuilt.model.last_recomputed > 0);
    assert_eq!(model(name, &cached), model(name, &rebuilt), "{name}: opened from the file's model");
    edit(&mut cached);
    edit(&mut rebuilt);
    assert_eq!(model(name, &cached), model(name, &rebuilt), "{name}: after the same edit");
    cached.execute("edit.undo", &json!({})).unwrap();
    rebuilt.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(model(name, &cached), model(name, &rebuilt), "{name}: after undoing it");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}.bak", path.display()));
}

#[test]
fn corpus_designs_open_from_their_model_as_evaluated() {
    for p in corpus() {
        let name = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let s = open(&p);
        if s.model.results.iter().any(|r| r.error.is_some()) || name == "naming-split-pieces" {
            // Not kept: a model with errors, or names saved before format 14.
            let path = temp(&name);
            let mut s = s;
            s.execute("file.save_as", &json!({"path": path.to_string_lossy()})).unwrap();
            assert!(solidcraft_engine::io::read_design_geometry(&std::fs::read(&path).unwrap()).is_none(), "{name}");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(format!("{}.bak", path.display()));
            continue;
        }
        check(&name, s);
    }
}

#[test]
fn sample_designs_open_from_their_model_as_evaluated() {
    for sample in solidcraft_engine::sample::samples() {
        let mut s = Session::default();
        for c in (sample.script)()["commands"].as_array().unwrap() {
            s.execute(c["command"].as_str().unwrap(), &c["params"]).unwrap_or_else(|e| panic!("{}: {e}", sample.name));
        }
        check(sample.name, s);
    }
}

#[test]
fn imported_assemblies_open_from_their_model_as_evaluated() {
    // A STEP assembly of the hinge sample: parts placed several times.
    let mut source = Session::default();
    let hinge = solidcraft_engine::sample::samples().into_iter().find(|s| s.name == "Hinge Assembly").unwrap();
    for c in (hinge.script)()["commands"].as_array().unwrap() {
        source.execute(c["command"].as_str().unwrap(), &c["params"]).unwrap();
    }
    let step = std::env::temp_dir().join(format!("solidcraft-model-cache-{}-hinge.step", std::process::id()));
    source.execute("file.export", &json!({"path": step.to_string_lossy()})).unwrap();
    let mut s = Session::default();
    s.execute("file.insert_step", &json!({"path": step.to_string_lossy()})).unwrap();
    s.execute("file.insert_step", &json!({"path": step.to_string_lossy(), "matrix": [1, 0, 0, 200, 0, 1, 0, 0, 0, 0, 1, 0]})).unwrap();
    let _ = std::fs::remove_file(&step);
    check("imported hinge", s);
}
