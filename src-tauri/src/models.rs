//! The models Loquara can transcribe with, and how to fetch them.
//!
//! Every model is a handful of ordinary files on a public host: no account,
//! no access token, no licence to accept. That is a deliberate constraint
//! rather than a coincidence — a model the app cannot fetch on its own is a
//! model the user has to go and find, which is where the old setup went
//! wrong.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// One file belonging to a model.
///
/// `remote` and `local` differ because publishers name files to suit their
/// own repository, while the engine looks for fixed names.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    pub remote: &'static str,
    pub local: &'static str,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub key: &'static str,
    pub display: &'static str,
    pub provider: &'static str,
    pub languages: &'static str,
    /// Hugging Face repository holding the files. Public and ungated.
    pub repo: &'static str,
    pub files: &'static [ModelFile],
}

impl ModelSpec {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }

    /// Where one file is fetched from.
    pub fn url(&self, file: &ModelFile) -> String {
        format!(
            "https://huggingface.co/{}/resolve/main/{}",
            self.repo, file.remote
        )
    }
}

/// Sizes are the published file sizes, used to show progress before the
/// server has answered and to estimate the download before it starts.
pub const CATALOGUE: &[ModelSpec] = &[ModelSpec {
    key: "parakeet",
    display: "Parakeet TDT 0.6B v3",
    provider: "NVIDIA",
    languages: "25 języków",
    repo: "csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
    files: &[
        ModelFile { remote: "encoder.int8.onnx", local: "encoder.int8.onnx", bytes: 561_880_064 },
        ModelFile { remote: "decoder.int8.onnx", local: "decoder.int8.onnx", bytes: 11_845_275 },
        ModelFile { remote: "joiner.int8.onnx", local: "joiner.int8.onnx", bytes: 6_355_277 },
        ModelFile { remote: "tokens.txt", local: "tokens.txt", bytes: 93_939 },
    ],
}];

pub fn spec(key: &str) -> Option<&'static ModelSpec> {
    CATALOGUE.iter().find(|model| model.key == key)
}

pub fn default_model() -> &'static str {
    CATALOGUE[0].key
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
}

/// Downloads every file a model needs into `directory`.
///
/// Each file lands under a `.part` name and is renamed once complete, so an
/// interrupted download can never leave a half-written file that later looks
/// like an installed model. `should_continue` is polled during the transfer
/// so the caller can abort.
pub fn download(
    key: &str,
    directory: &Path,
    mut on_progress: impl FnMut(u64, u64),
    should_continue: impl Fn() -> bool,
) -> Result<(), DownloadError> {
    let spec = spec(key).ok_or_else(|| DownloadError::UnknownModel(key.to_owned()))?;
    fs::create_dir_all(directory).map_err(|error| DownloadError::Disk(error.to_string()))?;

    let total = spec.total_bytes();
    let mut done = 0_u64;
    on_progress(0, total);

    for file in spec.files {
        let final_path = directory.join(file.local);
        if final_path.is_file() {
            // Already fetched by an earlier attempt.
            done += file.bytes;
            on_progress(done.min(total), total);
            continue;
        }
        let part_path = final_path.with_extension("part");
        let response = ureq::get(&spec.url(file))
            .call()
            .map_err(|error| DownloadError::Network(error.to_string()))?;
        let mut reader = response.into_reader();
        let mut writer =
            fs::File::create(&part_path).map_err(|error| DownloadError::Disk(error.to_string()))?;
        let mut buffer = vec![0_u8; 1 << 16];
        loop {
            if !should_continue() {
                let _ = fs::remove_file(&part_path);
                return Err(DownloadError::Cancelled);
            }
            let read = reader
                .read(&mut buffer)
                .map_err(|error| DownloadError::Network(error.to_string()))?;
            if read == 0 {
                break;
            }
            std::io::Write::write_all(&mut writer, &buffer[..read])
                .map_err(|error| DownloadError::Disk(error.to_string()))?;
            done += read as u64;
            on_progress(done.min(total), total);
        }
        drop(writer);
        fs::rename(&part_path, &final_path).map_err(|error| DownloadError::Disk(error.to_string()))?;
    }
    Ok(())
}

/// Removes a downloaded model, including any interrupted parts.
pub fn remove(key: &str, directory: &Path) -> Result<(), DownloadError> {
    let spec = spec(key).ok_or_else(|| DownloadError::UnknownModel(key.to_owned()))?;
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
    fn the_default_model_is_in_the_catalogue() {
        assert!(spec(default_model()).is_some());
    }

    #[test]
    fn every_model_lists_the_files_the_engine_looks_for() {
        // The engine loads fixed names; a spec that fetches something else
        // would download successfully and then fail to load.
        for model in CATALOGUE {
            let locals: Vec<_> = model.files.iter().map(|file| file.local).collect();
            for required in ["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt"] {
                assert!(locals.contains(&required), "{} lacks {required}", model.key);
            }
        }
    }

    #[test]
    fn urls_point_at_a_public_repository_without_a_token() {
        let model = spec("parakeet").unwrap();
        let url = model.url(&model.files[0]);

        assert!(url.starts_with("https://huggingface.co/"), "{url}");
        assert!(url.ends_with("/resolve/main/encoder.int8.onnx"), "{url}");
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
        fs::write(directory.join("tokens.txt"), b"x").unwrap();
        fs::write(directory.join("encoder.int8.part"), b"x").unwrap();

        remove("parakeet", &directory).unwrap();

        assert!(!directory.join("tokens.txt").exists());
        assert!(!directory.join("encoder.int8.part").exists());
    }

    #[test]
    fn installed_bytes_counts_only_what_is_actually_there() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("parakeet");
        fs::create_dir_all(&directory).unwrap();
        assert_eq!(installed_bytes("parakeet", &directory), 0);

        fs::write(directory.join("tokens.txt"), vec![0_u8; 100]).unwrap();

        assert_eq!(installed_bytes("parakeet", &directory), 100);
    }
}
