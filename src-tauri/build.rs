use std::path::{Path, PathBuf};

/// The ONNX Runtime and sherpa-onnx libraries the engine links against.
///
/// They are produced by the `sherpa-rs-sys` build into the profile's output
/// directory, which is not a path the bundler can be pointed at: it differs
/// between debug and release, and naming one profile in `tauri.conf.json`
/// makes a build of the other copy artefacts it never produced. Collecting
/// them into `lib/` first gives the bundler one stable place to look.
const ENGINE_LIBRARIES: &[&str] = &[
    "onnxruntime.dll",
    "onnxruntime_providers_shared.dll",
    "sherpa-onnx-c-api.dll",
    "sherpa-onnx-cxx-api.dll",
    "cargs.dll",
    // DirectML is what lets the engine use any DirectX 12 card, whoever made
    // it. Its debug twin is deliberately left behind.
    "DirectML.dll",
];

/// Walks up from `OUT_DIR` to the profile directory that holds build outputs.
fn profile_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR")?);
    // .../target/<profile>/build/<crate>-<hash>/out
    out_dir.ancestors().nth(3).map(Path::to_path_buf)
}

fn stage_engine_libraries() {
    let Some(profile) = profile_dir() else { return };
    let destination = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("lib");
    if std::fs::create_dir_all(&destination).is_err() {
        return;
    }
    for name in ENGINE_LIBRARIES {
        let source = profile.join(name);
        if !source.is_file() {
            continue;
        }
        let target = destination.join(name);
        // Copying over a library the running app has mapped fails on Windows,
        // and an identical file does not need copying anyway.
        let same = std::fs::metadata(&source)
            .ok()
            .zip(std::fs::metadata(&target).ok())
            .is_some_and(|(from, to)| from.len() == to.len());
        if !same {
            let _ = std::fs::copy(&source, &target);
        }
        println!("cargo:rerun-if-changed={}", source.display());
    }
}

fn main() {
    stage_engine_libraries();
    tauri_build::build();
}
