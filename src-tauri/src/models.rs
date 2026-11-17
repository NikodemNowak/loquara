//! The models Loquara can transcribe with, and how to fetch them.
//!
//! Every model is a handful of ordinary files on a public host (the project's
//! own GitHub release): no account, no access token, no licence to accept. That is a deliberate constraint
//! rather than a coincidence — a model the app cannot fetch on its own is a
//! model the user has to go and find, which is where the old setup went
//! wrong.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use sha2::{Digest, Sha256};

/// One file belonging to a model.
///
/// `remote` and `local` differ because publishers name files to suit their
/// own repository, while the engine looks for fixed names.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    pub remote: &'static str,
    pub local: &'static str,
    pub bytes: u64,
    /// Lower-case hex SHA-256 of the published file. A download is only
    /// accepted, and a file already on disk only reused, when it matches.
    pub sha256: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub key: &'static str,
    pub display: &'static str,
    pub provider: &'static str,
    pub languages: &'static str,
    /// Stable identifier shown to the interface.
    pub id: &'static str,
    /// Public, ungated location the files are fetched from; each file is
    /// `<base_url>/<remote>`.
    pub base_url: &'static str,
    pub files: &'static [ModelFile],
}

impl ModelSpec {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }

    /// Where one file is fetched from.
    pub fn url(&self, file: &ModelFile) -> String {
        format!("{}/{}", self.base_url, file.remote)
    }
}

/// Sizes are the published file sizes, used to show progress before the
/// server has answered and to estimate the download before it starts.
///
/// NVIDIA Parakeet TDT 0.6B v3 (CC-BY-4.0), exported to ONNX by istupakov and
/// quantized by Loquara with ONNX Runtime's weight-only block quantizer
/// (8-bit, block 32), with the encoder's weights in a separate, memory-mappable
/// file since release r2 (same weights). On the user's own takes it matched the fp32 model on
/// 99.3% of words, where the sherpa-onnx int8 export it replaces differed on
/// 11.8%; that export is where "Cloud Code" became "Clad Cod".
pub const CATALOGUE: &[ModelSpec] = &[ModelSpec {
    key: "parakeet",
    display: "Parakeet TDT 0.6B v3",
    provider: "NVIDIA",
    languages: "25 języków",
    id: "nvidia/parakeet-tdt-0.6b-v3 (8-bit, block 32)",
    base_url: "https://github.com/NikodemNowak/loquara/releases/download/model-parakeet-tdt-0.6b-v3-q8b32-r2",
    // The weights come first and the graph that points at them last: a
    // download interrupted in between leaves no encoder graph, which reads
    // as "not installed" rather than as a model that fails to load.
    files: &[
        ModelFile {
            remote: "encoder-model.onnx.data",
            local: "encoder-model.onnx.data",
            bytes: 965_013_504,
            sha256: "5075348d125cbf8fdd0f5ef230b34900b08c5675768cb034d8c04b5a4ad97a87",
        },
        ModelFile {
            remote: "encoder-model.onnx",
            local: "encoder-model.onnx",
            bytes: 873_009,
            sha256: "2ed0629253d0ecbc9082a6a2e16d9e124c167993d10a5ab1b7e2c11052c958a5",
        },
        ModelFile {
            remote: "decoder_joint-model.onnx",
            local: "decoder_joint-model.onnx",
            bytes: 54_375_348,
            sha256: "dcec3a9b35cb1504b050921f9c3cfe33b53e18a6f114d19b724d05bf05dd5811",
        },
        ModelFile {
            remote: "vocab.txt",
            local: "vocab.txt",
            bytes: 93_939,
            sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
        },
    ],
}];

/// Files of the sherpa-onnx int8 model earlier versions downloaded into the
/// same directory. The engine cannot read them, so an upgraded installation
/// reports the model as not installed (and offers the download); once the new
/// files are in place these are deleted to give the space back.
pub const LEGACY_FILES: &[&str] = &["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt"];

pub fn spec(key: &str) -> Option<&'static ModelSpec> {
    CATALOGUE.iter().find(|model| model.key == key)
}

pub fn default_model() -> &'static str {
    CATALOGUE[0].key
}

/// Maps a stored model key onto one that still exists.
///
/// Settings outlive the catalogue: an installation that was configured for a
/// model Loquara no longer offers would otherwise sit there warming a model
/// that cannot be found, reporting nothing and transcribing nothing.
pub fn resolve(key: &str) -> &'static str {
    spec(key).map(|model| model.key).unwrap_or_else(default_model)
}

/// Progress of a download in flight.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub model: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("could not reach the model host: {0}")]
    Network(String),
    #[error("could not write the model to disk: {0}")]
    Disk(String),
    #[error("download cancelled")]
    Cancelled,
    #[error("{0} did not match its published checksum")]
    Checksum(String),
}

/// SHA-256 of a file on disk, as lower-case hex.
fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Whether `path` holds exactly the published file.
fn is_intact(path: &Path, file: &ModelFile) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.len() == file.bytes)
        && file_sha256(path).is_ok_and(|hash| hash == file.sha256)
}

/// Deletes the previous engine's model files, returning the bytes freed.
pub fn remove_legacy(directory: &Path) -> u64 {
    let mut freed = 0;
    for name in LEGACY_FILES {
        let path = directory.join(name);
        let size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        if fs::remove_file(&path).is_ok() {
            freed += size;
        }
    }
    freed
}

/// How many connections one file is fetched over.
///
/// Measured against the host: one connection is held to roughly a quarter of
/// the speed four of them reach together, so the limit is per-connection
/// rather than the line. Four is also what the publisher's own client uses,
/// which is a reasonable place to stop asking for more.
const STREAMS: u64 = 4;

/// How much has to arrive before the caller hears about it again.
///
/// Progress was reported after every 64 KB, which is around ten thousand
/// messages for one model — none of which the eye can tell apart, all of
/// which the download thread pays for.
const PROGRESS_STEP: u64 = 4 << 20;

/// Downloads every file a model needs into `directory`.
///
/// Each file lands under a `.part` name and is renamed once complete, so an
/// interrupted download can never leave a half-written file that later looks
/// like an installed model. `should_continue` is polled during the transfer
/// so the caller can abort.
pub fn download(
    key: &str,
    directory: &Path,
    on_progress: impl Fn(u64, u64) + Sync,
    should_continue: impl Fn() -> bool + Sync,
) -> Result<(), DownloadError> {
    let spec = spec(key).ok_or_else(|| DownloadError::UnknownModel(key.to_owned()))?;
    fs::create_dir_all(directory).map_err(|error| DownloadError::Disk(error.to_string()))?;

    let total = spec.total_bytes();
    let done = AtomicU64::new(0);
    let reported = AtomicU64::new(0);
    // Called from every stream, so it reports the shared total rather than
    // its own share, and only when the number has moved enough to matter.
    let advance = |bytes: u64| {
        let now = done.fetch_add(bytes, Ordering::Relaxed) + bytes;
        let last = reported.load(Ordering::Relaxed);
        if now >= last + PROGRESS_STEP
            && reported
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            on_progress(now.min(total), total);
        }
    };
    on_progress(0, total);

    for file in spec.files {
        let final_path = directory.join(file.local);
        if final_path.is_file() {
            if is_intact(&final_path, file) {
                // Already fetched by an earlier attempt, or placed there by
                // hand: either way it is byte for byte the published file.
                advance(file.bytes);
                on_progress(done.load(Ordering::Relaxed).min(total), total);
                continue;
            }
            let _ = fs::remove_file(&final_path);
        }
        let part_path = final_path.with_extension("part");
        fetch(&spec.url(file), &part_path, file.bytes, &advance, &should_continue).inspect_err(
            |_| {
                let _ = fs::remove_file(&part_path);
            },
        )?;
        if !is_intact(&part_path, file) {
            let _ = fs::remove_file(&part_path);
            return Err(DownloadError::Checksum(file.remote.to_owned()));
        }
        fs::rename(&part_path, &final_path)
            .map_err(|error| DownloadError::Disk(error.to_string()))?;
    }
    // The new model is complete; the old engine's files are dead weight now.
    remove_legacy(directory);
    on_progress(total, total);
    Ok(())
}

/// Fetches one file, over several connections at once.
///
/// The file is sized up front and each stream writes straight into its own
/// span of it, so there is nothing to join at the end. A host that ignores
/// the range header answers with the whole file; that is not an error, it
/// just means one stream does the work.
fn fetch(
    url: &str,
    path: &Path,
    size: u64,
    advance: &(impl Fn(u64) + Sync),
    should_continue: &(impl Fn() -> bool + Sync),
) -> Result<(), DownloadError> {
    let file = fs::File::create(path).map_err(|error| DownloadError::Disk(error.to_string()))?;
    file.set_len(size)
        .map_err(|error| DownloadError::Disk(error.to_string()))?;
    drop(file);

    let span = size.div_ceil(STREAMS);
    let outcome = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..STREAMS)
            .map(|index| {
                let from = index * span;
                let to = ((index + 1) * span).min(size) - 1;
                scope.spawn(move || fetch_span(url, path, from, to, advance, should_continue))
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap_or_else(|_| {
                Err(DownloadError::Network("a download thread stopped".into()))
            }))
            .collect::<Result<Vec<bool>, DownloadError>>()
    })?;

    // A host that ignored the ranges sent the whole file to every stream. The
    // first one wrote it correctly; the rest wrote it over itself from zero,
    // so the bytes are right and only the count is wrong.
    if outcome.iter().any(|ranged| !ranged) {
        return Ok(());
    }
    Ok(())
}

/// Fetches `from..=to` of `url` into the matching span of `path`.
///
/// Returns whether the host honoured the range. Each stream opens its own
/// handle, so no two of them share a file position.
fn fetch_span(
    url: &str,
    path: &Path,
    from: u64,
    to: u64,
    advance: &(impl Fn(u64) + Sync),
    should_continue: &(impl Fn() -> bool + Sync),
) -> Result<bool, DownloadError> {
    if to < from {
        return Ok(true);
    }
    let response = ureq::get(url)
        .set("Range", &format!("bytes={from}-{to}"))
        .call()
        .map_err(|error| DownloadError::Network(error.to_string()))?;
    let ranged = response.status() == 206;
    let mut reader = response.into_reader();

    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| DownloadError::Disk(error.to_string()))?;
    let mut offset = if ranged { from } else { 0 };
    let mut buffer = vec![0_u8; 1 << 16];
    loop {
        if !should_continue() {
            return Err(DownloadError::Cancelled);
        }
        let read = std::io::Read::read(&mut reader, &mut buffer)
            .map_err(|error| DownloadError::Network(error.to_string()))?;
        if read == 0 {
            return Ok(ranged);
        }
        let mut written = 0;
        while written < read {
            let count = write_at(&file, &buffer[written..read], offset + written as u64)
                .map_err(|error| DownloadError::Disk(error.to_string()))?;
            if count == 0 {
                return Err(DownloadError::Disk("the disk accepted no bytes".into()));
            }
            written += count;
        }
        offset += read as u64;
        if ranged {
            advance(read as u64);
        }
    }
}

/// Writes at an absolute offset, leaving the handle's own position alone.
#[cfg(windows)]
fn write_at(file: &fs::File, buffer: &[u8], offset: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(file, buffer, offset)
}

#[cfg(unix)]
fn write_at(file: &fs::File, buffer: &[u8], offset: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(file, buffer, offset)
}

/// Clears half-written files left by a download that never finished.
///
/// Closing the app mid-transfer leaves `.part` files behind. They are never
/// mistaken for a model — only the final names count — but they hold the disk
/// space of a download nobody is waiting for any more.
pub fn sweep_parts(root: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    let mut freed = 0;
    for model in entries.flatten() {
        let Ok(files) = fs::read_dir(model.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_some_and(|extension| extension == "part") {
                let size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
                if fs::remove_file(&path).is_ok() {
                    freed += size;
                }
            }
        }
    }
    freed
}

/// Removes a downloaded model, including any interrupted parts.
pub fn remove(key: &str, directory: &Path) -> Result<(), DownloadError> {
    let spec = spec(key).ok_or_else(|| DownloadError::UnknownModel(key.to_owned()))?;
    remove_legacy(directory);
    for file in spec.files {
        for path in [
            directory.join(file.local),
            directory.join(file.local).with_extension("part"),
        ] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(DownloadError::Disk(error.to_string())),
            }
        }
    }
    let _ = fs::remove_dir(directory);
    Ok(())
}

/// Bytes a model already occupies on disk.
pub fn installed_bytes(key: &str, directory: &Path) -> u64 {
    spec(key)
        .map(|spec| {
            spec.files
                .iter()
                .filter_map(|file| fs::metadata(directory.join(file.local)).ok())
                .map(|meta| meta.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Path a model's files live under, given the models root.
pub fn model_dir(root: &Path, key: &str) -> PathBuf {
    root.join(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_that_no_longer_exists_falls_back_to_the_default() {
        // Upgrades carry old settings forward; "cohere" was offered by an
        // earlier version and would otherwise leave the app with nothing.
        assert_eq!(resolve("cohere"), default_model());
        assert_eq!(resolve(""), default_model());
    }

    #[test]
    fn a_known_model_is_left_alone() {
        assert_eq!(resolve("parakeet"), "parakeet");
    }

    #[test]
    fn the_declared_size_matches_what_the_host_serves() {
        // Taken from the published Content-Length; a number copied from a
        // half-finished download would make progress run past 100%.
        let model = spec("parakeet").unwrap();
        assert_eq!(model.total_bytes(), 1_020_355_800);
    }

    #[test]
    fn the_default_model_is_in_the_catalogue() {
        assert!(spec(default_model()).is_some());
    }

    #[test]
    fn every_model_lists_the_files_the_engine_looks_for() {
        // The engine loads fixed names; a spec that fetches something else
        // would download successfully and then fail to load.
        for model in CATALOGUE {
            let locals: Vec<_> = model.files.iter().map(|file| file.local).collect();
            for required in crate::engine::MODEL_FILES.iter().chain([&crate::engine::ENCODER_DATA_FILE]) {
                assert!(locals.contains(required), "{} lacks {required}", model.key);
            }
        }
    }

    #[test]
    fn urls_point_at_a_public_release_without_a_token() {
        let model = spec("parakeet").unwrap();
        let url = model.url(&model.files[0]);

        assert_eq!(
            url,
            "https://github.com/NikodemNowak/loquara/releases/download/model-parakeet-tdt-0.6b-v3-q8b32-r2/encoder-model.onnx.data"
        );
    }

    #[test]
    fn every_file_carries_a_sha256() {
        for model in CATALOGUE {
            for file in model.files {
                assert_eq!(file.sha256.len(), 64, "{}", file.remote);
                assert!(file.sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
            }
        }
    }

    #[test]
    fn a_file_is_intact_only_with_the_published_size_and_hash() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("vocab.txt");
        fs::write(&path, b"abc").unwrap();
        // SHA-256 of "abc".
        let good = ModelFile {
            remote: "vocab.txt",
            local: "vocab.txt",
            bytes: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        assert!(is_intact(&path, &good));
        assert!(!is_intact(&path, &ModelFile { bytes: 4, ..good }));
        assert!(!is_intact(&path, &ModelFile { sha256: "00", ..good }));
        assert!(!is_intact(&temp.path().join("missing"), &good));
    }

    #[test]
    fn the_previous_engines_files_are_removed_and_the_new_ones_kept() {
        let temp = tempfile::tempdir().unwrap();
        for name in LEGACY_FILES {
            fs::write(temp.path().join(name), vec![0_u8; 10]).unwrap();
        }
        fs::write(temp.path().join("vocab.txt"), b"x").unwrap();

        assert_eq!(remove_legacy(temp.path()), 40);
        assert!(LEGACY_FILES.iter().all(|name| !temp.path().join(name).exists()));
        assert!(temp.path().join("vocab.txt").exists());
    }

    #[test]
    fn the_advertised_size_is_the_sum_of_the_files() {
        let model = spec("parakeet").unwrap();

        assert_eq!(
            model.total_bytes(),
            model.files.iter().map(|file| file.bytes).sum::<u64>()
        );
        // Sanity: the encoder dominates, so the total must be in that region.
        assert!(model.total_bytes() > 500_000_000);
    }

    /// Reaches the real host, so it is not part of the ordinary run.
    /// `cargo test --release -- --ignored ranged` exercises it.
    #[ignore]
    #[test]
    fn four_streams_reassemble_a_file_byte_for_byte() {
        use std::sync::atomic::AtomicU64;

        let model = spec("parakeet").unwrap();
        let tokens = model.files.iter().find(|file| file.local == "vocab.txt").unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tokens.part");
        let counted = AtomicU64::new(0);

        fetch(
            &model.url(tokens),
            &path,
            tokens.bytes,
            &|bytes| { counted.fetch_add(bytes, Ordering::Relaxed); },
            &|| true,
        )
        .unwrap();

        let written = fs::read(&path).unwrap();
        assert_eq!(written.len() as u64, tokens.bytes, "size");
        // Every span landed where it belongs: a file stitched wrongly still
        // has the right length, so the content is what has to be checked.
        let single = ureq::get(&model.url(tokens)).call().unwrap();
        let mut expected = Vec::new();
        std::io::Read::read_to_end(&mut single.into_reader(), &mut expected).unwrap();
        assert_eq!(written, expected, "content");
        assert_eq!(counted.load(Ordering::Relaxed), tokens.bytes, "reported bytes");
    }

    #[test]
    fn an_unknown_model_is_refused_rather_than_guessed() {
        let temp = tempfile::tempdir().unwrap();

        let error = download("nope", temp.path(), |_, _| {}, || true).unwrap_err();

        assert!(matches!(error, DownloadError::UnknownModel(_)), "{error:?}");
    }

    #[test]
    fn removing_a_model_clears_finished_and_interrupted_files() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("parakeet");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("vocab.txt"), b"x").unwrap();
        fs::write(directory.join("encoder-model.part"), b"x").unwrap();
        fs::write(directory.join("tokens.txt"), b"x").unwrap();

        remove("parakeet", &directory).unwrap();

        assert!(!directory.join("vocab.txt").exists());
        assert!(!directory.join("encoder-model.part").exists());
        assert!(!directory.join("tokens.txt").exists(), "the old engine's files go too");
    }

    #[test]
    fn interrupted_parts_are_swept_and_finished_files_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("parakeet");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("tokens.txt"), vec![0_u8; 10]).unwrap();
        fs::write(directory.join("encoder.int8.part"), vec![0_u8; 250]).unwrap();

        let freed = sweep_parts(temp.path());

        assert_eq!(freed, 250);
        assert!(!directory.join("encoder.int8.part").exists());
        assert!(directory.join("tokens.txt").exists(), "a finished file is not a leftover");
    }

    #[test]
    fn installed_bytes_counts_only_what_is_actually_there() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("parakeet");
        fs::create_dir_all(&directory).unwrap();
        assert_eq!(installed_bytes("parakeet", &directory), 0);

        fs::write(directory.join("vocab.txt"), vec![0_u8; 100]).unwrap();

        assert_eq!(installed_bytes("parakeet", &directory), 100);
    }
}
