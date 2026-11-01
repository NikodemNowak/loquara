use std::path::{Path, PathBuf};

/// The ONNX Runtime and sherpa-onnx libraries the engine links against.
///
/// They are produced by the `sherpa-rs-sys` build into the profile's output
/// directory, which is not a path the bundler can be pointed at: it differs
/// between debug and release, and naming one profile in `tauri.conf.json`
/// makes a build of the other copy artefacts it never produced. Collecting
/// them into `lib/` first gives the bundler one stable place to look.
#[cfg(windows)]
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
#[cfg(windows)]
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
#[cfg(windows)]
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

/// Locates one engine library on Windows.
///
/// Cargo orders build scripts against build dependencies only, so this script
/// can run before `sherpa-rs-sys` has compiled anything. `beforeBuildCommand`
/// builds that crate first for exactly this reason. The search still looks in
/// two places: the profile root, where sherpa-rs-sys copies its libraries as
/// a convenience, and the build tree they came from, which survives a
/// restored cache when the loose copies do not.
#[cfg(windows)]
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

#[cfg(windows)]
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

/// Where `sherpa-rs-sys` unpacks the prebuilt archives it downloads.
///
/// The crate only copies the unversioned `*.so` entries into the target
/// directory, while the loader asks for the versioned name recorded in the
/// library's SONAME. The cache still holds every name, so it is searched too.
#[cfg(not(windows))]
fn sherpa_cache_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Caches/sherpa-rs"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .map(|cache| cache.join("sherpa-rs"))
    }
}

#[cfg(not(windows))]
fn is_engine_library(name: &str, extension: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let relevant = lower.starts_with("libsherpa-onnx") || lower.starts_with("libonnxruntime");
    let shared = lower.ends_with(&format!(".{extension}"))
        || lower.contains(&format!(".{extension}."));
    relevant && shared
}

#[cfg(not(windows))]
fn collect_engine_libraries(
    root: &Path,
    extension: &str,
    found: &mut std::collections::HashMap<String, PathBuf>,
    depth: usize,
) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_engine_libraries(&path, extension, found, depth - 1);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_engine_library(name, extension) {
            found.entry(name.to_owned()).or_insert(path);
        }
    }
}

/// Stages macOS/Linux shared libraries for the bundler.
///
/// macOS puts them in `Contents/Frameworks`, Linux in the resource directory
/// under `/usr/lib/loquara`. Both are wired up through the platform Tauri
/// configs; this function only has to make sure the files exist in `lib/`.
#[cfg(not(windows))]
fn stage_engine_libraries() {
    let Some(profile) = profile_dir() else { return };
    let destination = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("lib");
    if std::fs::create_dir_all(&destination).is_err() {
        return;
    }
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let extension = if target_os == "macos" { "dylib" } else { "so" };

    let mut found = std::collections::HashMap::new();
    // The sys crate copies the unversioned libraries next to the binary.
    collect_engine_libraries(&profile, extension, &mut found, 2);
    // Its build tree keeps the versioned files and symlinks.
    if let Ok(entries) = std::fs::read_dir(profile.join("build")) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("sherpa-rs-sys-")
            {
                collect_engine_libraries(&entry.path().join("out"), extension, &mut found, 8);
            }
        }
    }
    // The download cache is the complete archive, versioned names included.
    if let Some(cache) = sherpa_cache_dir() {
        collect_engine_libraries(&cache, extension, &mut found, 8);
    }

    let staged = found.len();
    for (name, source) in found {
        let target = destination.join(&name);
        let same = std::fs::metadata(&source)
            .ok()
            .zip(std::fs::metadata(&target).ok())
            .is_some_and(|(from, to)| from.len() == to.len());
        if !same {
            let _ = std::fs::copy(&source, &target);
        }
        println!("cargo:rerun-if-changed={}", source.display());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Some archives provide only the unversioned symlink. The loader still
        // asks for the SONAME, so give it the same bytes under that name.
        let base = destination.join("libonnxruntime.so");
        let soname = destination.join("libonnxruntime.so.1");
        if base.is_file() && !soname.exists() {
            let _ = std::fs::copy(&base, &soname);
        }
    }

    // Test binaries live in `<profile>/deps` and resolve their libraries from
    // `$ORIGIN`, while sherpa-rs-sys copies only the unversioned `*.so` there.
    // Mirror the staged names (including onnxruntime's versioned SONAME) next
    // to the tests and next to an unbundled binary as well.
    for mirror in [profile.join("deps"), profile] {
        if !mirror.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&destination) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_engine_library(&name, extension) {
                continue;
            }
            let target = mirror.join(&name);
            if target == entry.path() {
                continue;
            }
            let same = std::fs::metadata(entry.path())
                .ok()
                .zip(std::fs::metadata(&target).ok())
                .is_some_and(|(from, to)| from.len() == to.len());
            if !same {
                let _ = std::fs::copy(entry.path(), &target);
            }
        }
    }

    if staged == 0 {
        println!(
            "cargo:warning=no sherpa-onnx shared libraries were staged for bundling; \
             run the engine prebuild before packaging"
        );
    }
}

fn main() {
    stage_engine_libraries();

    // The engine's libraries are loaded from next to the binary in a dev run,
    // and from the bundle's own directories once installed. `DT_RPATH` (not
    // `DT_RUNPATH`) is used on Linux because it is inherited by transitive
    // dependencies, and `libsherpa-onnx-c-api` itself depends on onnxruntime.
    #[cfg(target_os = "macos")]
    {
        for path in [
            "@executable_path",
            "@executable_path/..",
            "@executable_path/../Frameworks",
        ] {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{path}");
            println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{path}");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for path in [
            "$ORIGIN",
            "$ORIGIN/..",
            "$ORIGIN/../lib/loquara",
            "$ORIGIN/../lib/Loquara",
        ] {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{path}");
            println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{path}");
        }
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
        println!("cargo:rustc-link-arg-tests=-Wl,--disable-new-dtags");
    }

    tauri_build::build();
}
