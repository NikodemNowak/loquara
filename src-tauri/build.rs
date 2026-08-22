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

/// Libraries without which the application cannot transcribe a word.
///
/// The rest of the list is optional: which of them a build produces depends on
/// how sherpa-onnx was configured. Missing one of these, though, means an
/// installer that starts and then fails on the first dictation — so the build
/// stops here instead.
const REQUIRED_LIBRARIES: &[&str] = &[
    "onnxruntime.dll",
    "sherpa-onnx-c-api.dll",
    "sherpa-onnx-cxx-api.dll",
    "DirectML.dll",
];

/// Walks up from `OUT_DIR` to the profile directory that holds build outputs.
fn profile_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR")?);
    // .../target/<profile>/build/<crate>-<hash>/out
    out_dir.ancestors().nth(3).map(Path::to_path_buf)
}

/// Finds `name` anywhere under `root`, up to `depth` levels down.
fn find_within(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let candidate = root.join(name);
    if candidate.is_file() {
        return Some(candidate);
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .find_map(|entry| find_within(&entry.path(), name, depth - 1))
}

/// Locates one engine library.
///
/// `sherpa-rs-sys` copies its libraries to the profile root as a convenience,
/// but that is a copy rather than the original: a restored build cache brings
/// back the compiled crate without those loose files, and the search then has
/// to go to the build tree they came from.
fn locate(profile: &Path, name: &str) -> Option<PathBuf> {
    let direct = profile.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(profile.join("build"))
        .ok()?
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("sherpa-rs-sys-")
        })
        .find_map(|entry| find_within(&entry.path().join("out"), name, 8))
}

fn stage_engine_libraries() {
    let Some(profile) = profile_dir() else { return };
    let destination = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("lib");
    if std::fs::create_dir_all(&destination).is_err() {
        return;
    }
    for name in ENGINE_LIBRARIES {
        let Some(source) = locate(&profile, name) else {
            assert!(
                !REQUIRED_LIBRARIES.contains(name),
                "{name} is missing: the engine cannot run without it, and an \
                 installer built now would fail on the first dictation. Look \
                 under {}",
                profile.display()
            );
            continue;
        };
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
