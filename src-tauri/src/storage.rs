use crate::audio::part_path_for;
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;

pub const INTERRUPTED_ERROR: &str = "Previous dictation was interrupted before completion.";
pub const INTERRUPTED_BEFORE_FINALIZE_ERROR: &str =
    "Previous dictation was interrupted before audio finalization.";

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("filesystem failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid status: {0}")]
    InvalidStatus(String),
    #[error("storage lock poisoned")]
    Poisoned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingStatus {
    Recording,
    Processing,
    Completed,
    Failed,
    Cancelled,
}

impl RecordingStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: String) -> Result<Self, StorageError> {
        match value.as_str() {
            "recording" => Ok(Self::Recording),
            "processing" => Ok(Self::Processing),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(StorageError::InvalidStatus(value)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub id: String,
    pub created_at: i64,
    pub duration_ms: u64,
    pub status: RecordingStatus,
    pub text: Option<String>,
    pub model: Option<String>,
    pub audio_path: Option<String>,
    pub source_app: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQuery {
    pub search: Option<String>,
    pub status: Option<RecordingStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyEntry {
    pub id: i64,
    pub heard: String,
    pub replacement: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mode {
    pub id: String,
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    Days(u32),
    Forever,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CleanupReport {
    pub deleted_audio: usize,
}

pub struct Storage {
    connection: Mutex<Connection>,
    recordings_dir: PathBuf,
}

impl Storage {
    pub fn open(
        path: impl AsRef<Path>,
        recordings_dir: impl Into<PathBuf>,
    ) -> Result<Self, StorageError> {
        let connection = Connection::open(path)?;
        Self::from_connection(connection, recordings_dir.into())
    }

    #[cfg(test)]
    pub fn open_in_memory(recordings_dir: impl Into<PathBuf>) -> Result<Self, StorageError> {
        Self::from_connection(Connection::open_in_memory()?, recordings_dir.into())
    }

    fn from_connection(
        connection: Connection,
        recordings_dir: PathBuf,
    ) -> Result<Self, StorageError> {
        let storage = Self {
            connection: Mutex::new(connection),
            recordings_dir,
        };
        storage.migrate()?;
        storage.reconcile_interrupted()?;
        Ok(storage)
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StorageError> {
        self.connection.lock().map_err(|_| StorageError::Poisoned)
    }

    fn migrate(&self) -> Result<(), StorageError> {
        self.connection()?.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS schema_version (
               version INTEGER NOT NULL
             );
             INSERT INTO schema_version(version)
               SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM schema_version);
             CREATE TABLE IF NOT EXISTS settings (
               key TEXT PRIMARY KEY,
               value_json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS history (
               id TEXT PRIMARY KEY,
               created_at INTEGER NOT NULL,
               duration_ms INTEGER NOT NULL DEFAULT 0,
               status TEXT NOT NULL CHECK(status IN ('recording','processing','completed','failed','cancelled')),
               text TEXT,
               model TEXT,
               audio_path TEXT,
               source_app TEXT,
               error TEXT
             );
             CREATE INDEX IF NOT EXISTS history_created_at ON history(created_at DESC);
             CREATE INDEX IF NOT EXISTS history_status ON history(status);
             CREATE TABLE IF NOT EXISTS vocabulary (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               heard TEXT NOT NULL COLLATE NOCASE UNIQUE,
               replacement TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS modes (
               id TEXT PRIMARY KEY,
               config_json TEXT NOT NULL
             );",
        )?;
        let version: i64 =
            self.connection()?
                .query_row("SELECT version FROM schema_version", [], |row| row.get(0))?;
        if version < 2 {
            self.connection()?.execute_batch(
                "ALTER TABLE modes ADD COLUMN name TEXT NOT NULL DEFAULT '';
                 ALTER TABLE modes ADD COLUMN description TEXT NOT NULL DEFAULT '';
                 ALTER TABLE modes ADD COLUMN prompt TEXT NOT NULL DEFAULT '';
                 ALTER TABLE modes ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;
                 ALTER TABLE modes ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE modes ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        self.connection()?.execute_batch(
            "INSERT OR IGNORE INTO modes
               (id,config_json,name,description,prompt,enabled,is_default,created_at)
             VALUES
               ('clean','{}','Czysty','Lekka normalizacja tekstu','',1,1,0),
               ('message','{}','Wiadomość','Naturalny styl wiadomości','',1,0,1),
               ('code','{}','Kod','Dyktowanie terminów technicznych','',1,0,2);
             UPDATE schema_version SET version = 2 WHERE version < 2;",
        )?;
        Ok(())
    }

    pub fn recordings_dir(&self) -> &Path {
        &self.recordings_dir
    }

    pub fn schema_version(&self) -> Result<i64, StorageError> {
        Ok(self
            .connection()?
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))?)
    }

    #[cfg(test)]
    fn table_names(&self) -> Result<Vec<String>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name IN ('settings','history','vocabulary','modes')
             ORDER BY name",
        )?;
        let names = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(names)
    }

    pub fn insert_recording(&self, recording: &Recording) -> Result<(), StorageError> {
        self.connection()?.execute(
            "INSERT INTO history
             (id, created_at, duration_ms, status, text, model, audio_path, source_app, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                recording.id,
                recording.created_at,
                recording.duration_ms,
                recording.status.as_str(),
                recording.text,
                recording.model,
                recording.audio_path,
                recording.source_app,
                recording.error,
            ],
        )?;
        Ok(())
    }

    pub fn update_recording(&self, recording: &Recording) -> Result<bool, StorageError> {
        Ok(self.connection()?.execute(
            "UPDATE history SET created_at=?2, duration_ms=?3, status=?4, text=?5,
             model=?6, audio_path=?7, source_app=?8, error=?9 WHERE id=?1",
            params![
                recording.id,
                recording.created_at,
                recording.duration_ms,
                recording.status.as_str(),
                recording.text,
                recording.model,
                recording.audio_path,
                recording.source_app,
                recording.error,
            ],
        )? > 0)
    }

    pub fn update_status(
        &self,
        id: &str,
        status: RecordingStatus,
        text: Option<&str>,
        model: Option<&str>,
        duration_ms: Option<u64>,
        error: Option<&str>,
    ) -> Result<bool, StorageError> {
        Ok(self.connection()?.execute(
            "UPDATE history SET status=?2, text=COALESCE(?3,text), model=COALESCE(?4,model),
             duration_ms=COALESCE(?5,duration_ms), error=?6 WHERE id=?1",
            params![id, status.as_str(), text, model, duration_ms, error],
        )? > 0)
    }

    pub fn mark_retrying(&self, id: &str) -> Result<bool, StorageError> {
        self.update_status(id, RecordingStatus::Processing, None, None, None, None)
    }

    pub fn get_recording(&self, id: &str) -> Result<Option<Recording>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, created_at, duration_ms, status, text, model, audio_path, source_app, error
             FROM history WHERE id=?1",
        )?;
        let raw = statement.query_row([id], raw_recording).optional()?;
        raw.map(RawRecording::try_into).transpose()
    }

    pub fn list_history(&self, query: &HistoryQuery) -> Result<Vec<Recording>, StorageError> {
        let search = query
            .search
            .as_ref()
            .map(|value| format!("%{}%", value.trim()))
            .unwrap_or_else(|| "%".into());
        let status = query.status.map(RecordingStatus::as_str);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, created_at, duration_ms, status, text, model, audio_path, source_app, error
             FROM history
             WHERE (?1 IS NULL OR status=?1) AND COALESCE(text,'') LIKE ?2
             ORDER BY created_at DESC, id DESC",
        )?;
        let raw = statement
            .query_map(params![status, search], raw_recording)?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter().map(TryInto::try_into).collect()
    }

    pub fn delete_recording(&self, id: &str) -> Result<bool, StorageError> {
        Ok(self
            .connection()?
            .execute("DELETE FROM history WHERE id=?1", [id])?
            > 0)
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<(), StorageError> {
        let json = serde_json::to_string(value)?;
        self.connection()?.execute(
            "INSERT INTO settings(key,value_json) VALUES (?1,?2)
             ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json",
            params![key, json],
        )?;
        Ok(())
    }

    pub fn get_setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, StorageError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT value_json FROM settings WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(StorageError::from)
    }

    pub fn list_settings(
        &self,
    ) -> Result<serde_json::Map<String, serde_json::Value>, StorageError> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT key,value_json FROM settings ORDER BY key")?;
        let pairs = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        pairs
            .into_iter()
            .map(|(key, json)| Ok((key, serde_json::from_str(&json)?)))
            .collect()
    }

    pub fn add_vocabulary(
        &self,
        heard: &str,
        replacement: &str,
    ) -> Result<VocabularyEntry, StorageError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO vocabulary(heard,replacement) VALUES (?1,?2)",
            params![heard.trim(), replacement.trim()],
        )?;
        Ok(VocabularyEntry {
            id: connection.last_insert_rowid(),
            heard: heard.trim().to_owned(),
            replacement: replacement.trim().to_owned(),
        })
    }

    pub fn list_vocabulary(&self) -> Result<Vec<VocabularyEntry>, StorageError> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT id,heard,replacement FROM vocabulary ORDER BY id")?;
        Ok(statement
            .query_map([], |row| {
                Ok(VocabularyEntry {
                    id: row.get(0)?,
                    heard: row.get(1)?,
                    replacement: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_vocabulary(&self, id: i64) -> Result<bool, StorageError> {
        Ok(self
            .connection()?
            .execute("DELETE FROM vocabulary WHERE id=?1", [id])?
            > 0)
    }

    pub fn list_modes(&self) -> Result<Vec<Mode>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,name,description,prompt,enabled,is_default,created_at
             FROM modes ORDER BY created_at,id",
        )?;
        Ok(statement
            .query_map([], mode_from_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_mode(&self, id: &str) -> Result<Option<Mode>, StorageError> {
        self.connection()?
            .query_row(
                "SELECT id,name,description,prompt,enabled,is_default,created_at
                 FROM modes WHERE id=?1",
                [id],
                mode_from_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn upsert_mode(&self, mode: &Mode) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        if mode.is_default {
            transaction.execute("UPDATE modes SET is_default=0", [])?;
        }
        transaction.execute(
            "INSERT INTO modes
               (id,config_json,name,description,prompt,enabled,is_default,created_at)
             VALUES (?1,'{}',?2,?3,?4,?5,?6,?7)
             ON CONFLICT(id) DO UPDATE SET
               name=excluded.name,description=excluded.description,prompt=excluded.prompt,
               enabled=excluded.enabled,is_default=excluded.is_default,
               created_at=excluded.created_at",
            params![
                mode.id,
                mode.name,
                mode.description,
                mode.prompt,
                mode.enabled,
                mode.is_default,
                mode.created_at,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_mode(&self, id: &str) -> Result<bool, StorageError> {
        if matches!(id, "clean" | "message" | "code") {
            return Ok(false);
        }
        Ok(self
            .connection()?
            .execute("DELETE FROM modes WHERE id=?1", [id])?
            > 0)
    }

    pub fn reconcile_interrupted(&self) -> Result<usize, StorageError> {
        fs::create_dir_all(&self.recordings_dir)?;
        let managed_root = self.recordings_dir.canonicalize()?;
        let interrupted: Vec<(String, RecordingStatus, Option<String>)> = {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT id,status,audio_path FROM history
                 WHERE status IN ('recording','processing')",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .map(|row| {
                    let (id, status, audio_path) = row?;
                    Ok((id, RecordingStatus::parse(status)?, audio_path))
                })
                .collect::<Result<Vec<_>, StorageError>>()?
        };

        let mut retryable_ids = Vec::new();
        let mut interrupted_before_finalize_ids = Vec::new();
        for (id, _status, audio_path) in interrupted {
            let final_path = audio_path.as_deref().map(Path::new);
            let has_managed_final = final_path.is_some_and(|path| {
                path.canonicalize().is_ok_and(|canonical| {
                    canonical.is_file()
                        && canonical
                            .parent()
                            .is_some_and(|parent| parent.starts_with(&managed_root))
                        && canonical
                            .extension()
                            .is_some_and(|extension| extension == "wav")
                })
            });
            if has_managed_final {
                retryable_ids.push(id);
                continue;
            }

            if let Some(final_path) = final_path
                && is_safe_managed_wav_candidate(final_path, &managed_root)
            {
                let partial_path = part_path_for(final_path);
                if partial_path.is_file() {
                    fs::remove_file(partial_path)?;
                }
            }
            interrupted_before_finalize_ids.push(id);
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for id in &retryable_ids {
            transaction.execute(
                "UPDATE history SET status='failed', error=?2 WHERE id=?1",
                params![id, INTERRUPTED_ERROR],
            )?;
        }
        for id in &interrupted_before_finalize_ids {
            transaction.execute(
                "UPDATE history SET status='failed', audio_path=NULL, error=?2 WHERE id=?1",
                params![id, INTERRUPTED_BEFORE_FINALIZE_ERROR],
            )?;
        }
        transaction.commit()?;
        Ok(retryable_ids.len() + interrupted_before_finalize_ids.len())
    }

    pub fn cleanup_retention(
        &self,
        retention: Retention,
        now_ms: i64,
    ) -> Result<CleanupReport, StorageError> {
        let Retention::Days(days @ (1 | 7 | 30)) = retention else {
            return Ok(CleanupReport { deleted_audio: 0 });
        };
        let cutoff = now_ms.saturating_sub(i64::from(days) * 86_400_000);
        fs::create_dir_all(&self.recordings_dir)?;
        let managed_root = self.recordings_dir.canonicalize()?;
        let candidates: Vec<(String, String)> = {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT id,audio_path FROM history
                 WHERE created_at < ?1 AND audio_path IS NOT NULL
                   AND status IN ('completed','failed','cancelled')",
            )?;
            statement
                .query_map([cutoff], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut cleared_ids = Vec::new();
        for (id, raw_path) in candidates {
            let path = PathBuf::from(raw_path);
            let Ok(canonical) = path.canonicalize() else {
                continue;
            };
            if canonical
                .parent()
                .is_some_and(|parent| parent.starts_with(&managed_root))
                && canonical
                    .extension()
                    .is_some_and(|extension| extension == "wav")
            {
                fs::remove_file(&canonical)?;
                cleared_ids.push(id);
            }
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for id in &cleared_ids {
            transaction.execute("UPDATE history SET audio_path=NULL WHERE id=?1", [id])?;
        }
        transaction.commit()?;
        Ok(CleanupReport {
            deleted_audio: cleared_ids.len(),
        })
    }
}

fn is_safe_managed_wav_candidate(path: &Path, managed_root: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "wav")
        && path
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .is_some_and(|parent| parent.starts_with(managed_root))
}

fn mode_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Mode> {
    Ok(Mode {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        prompt: row.get(3)?,
        enabled: row.get(4)?,
        is_default: row.get(5)?,
        created_at: row.get(6)?,
    })
}

struct RawRecording {
    id: String,
    created_at: i64,
    duration_ms: u64,
    status: String,
    text: Option<String>,
    model: Option<String>,
    audio_path: Option<String>,
    source_app: Option<String>,
    error: Option<String>,
}

fn raw_recording(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRecording> {
    Ok(RawRecording {
        id: row.get(0)?,
        created_at: row.get(1)?,
        duration_ms: row.get(2)?,
        status: row.get(3)?,
        text: row.get(4)?,
        model: row.get(5)?,
        audio_path: row.get(6)?,
        source_app: row.get(7)?,
        error: row.get(8)?,
    })
}

impl TryFrom<RawRecording> for Recording {
    type Error = StorageError;

    fn try_from(raw: RawRecording) -> Result<Self, Self::Error> {
        Ok(Self {
            id: raw.id,
            created_at: raw.created_at,
            duration_ms: raw.duration_ms,
            status: RecordingStatus::parse(raw.status)?,
            text: raw.text,
            model: raw.model,
            audio_path: raw.audio_path,
            source_app: raw.source_app,
            error: raw.error,
        })
    }
}

pub fn postprocess(text: &str, vocabulary: &[VocabularyEntry]) -> String {
    let whitespace = Regex::new(r"\s+").expect("static whitespace regex is valid");
    let punctuation = Regex::new(r"\s+([,.;:!?])").expect("static punctuation regex is valid");
    let mut processed = whitespace.replace_all(text.trim(), " ").into_owned();
    let mut entries = vocabulary.to_vec();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.heard.chars().count()));
    for entry in entries {
        if entry.heard.trim().is_empty() {
            continue;
        }
        let pattern = format!(r"(?iu)\b{}\b", regex::escape(entry.heard.trim()));
        if let Ok(regex) = Regex::new(&pattern) {
            processed = regex
                .replace_all(&processed, regex::NoExpand(&entry.replacement))
                .into_owned();
        }
    }
    punctuation.replace_all(&processed, "$1").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn storage() -> (tempfile::TempDir, Storage) {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open_in_memory(temp.path().join("recordings")).unwrap();
        (temp, storage)
    }

    fn recording(id: &str, status: RecordingStatus, text: Option<&str>) -> Recording {
        Recording {
            id: id.to_owned(),
            created_at: 1_722_268_800_000,
            duration_ms: 1_250,
            status,
            text: text.map(str::to_owned),
            model: Some("parakeet".to_owned()),
            audio_path: Some(format!("{id}.wav")),
            source_app: Some("Notatnik".to_owned()),
            error: None,
        }
    }

    #[test]
    fn migrates_all_tables_and_records_schema_version() {
        let (_temp, storage) = storage();

        assert_eq!(storage.schema_version().unwrap(), 2);
        assert_eq!(
            storage.table_names().unwrap(),
            vec!["history", "modes", "settings", "vocabulary"]
        );
    }

    #[test]
    fn seeds_and_cruds_default_modes() {
        let (_temp, storage) = storage();
        let defaults = storage.list_modes().unwrap();
        assert_eq!(
            defaults
                .iter()
                .map(|mode| mode.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Czysty", "Wiadomość", "Kod"]
        );
        assert_eq!(defaults.iter().filter(|mode| mode.is_default).count(), 1);

        let custom = Mode {
            id: "custom".into(),
            name: "Własny".into(),
            description: "Firmowy styl".into(),
            prompt: "Zachowaj terminologię".into(),
            enabled: true,
            is_default: false,
            created_at: 123,
        };
        storage.upsert_mode(&custom).unwrap();
        assert_eq!(storage.get_mode("custom").unwrap(), Some(custom.clone()));
        assert!(storage.delete_mode("custom").unwrap());
        assert!(storage.get_mode("custom").unwrap().is_none());
    }

    #[test]
    fn built_in_modes_cannot_be_deleted_and_custom_deletion_persists() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("mow.sqlite3");
        let recordings = temp.path().join("recordings");
        let storage = Storage::open(&database, &recordings).unwrap();
        assert!(!storage.delete_mode("clean").unwrap());
        assert!(storage.get_mode("clean").unwrap().is_some());
        storage
            .upsert_mode(&Mode {
                id: "custom".into(),
                name: "Własny".into(),
                description: String::new(),
                prompt: String::new(),
                enabled: true,
                is_default: false,
                created_at: 99,
            })
            .unwrap();
        drop(storage);

        let reopened = Storage::open(&database, &recordings).unwrap();
        assert!(reopened.get_mode("custom").unwrap().is_some());
        assert!(reopened.delete_mode("custom").unwrap());
        drop(reopened);

        let reopened = Storage::open(&database, &recordings).unwrap();
        assert!(reopened.get_mode("custom").unwrap().is_none());
        assert!(reopened.get_mode("clean").unwrap().is_some());
    }

    #[test]
    fn reopening_reconciles_interrupted_rows_using_real_final_and_partial_audio() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("mow.sqlite3");
        let recordings = temp.path().join("recordings");
        fs::create_dir_all(&recordings).unwrap();
        let storage = Storage::open(&database, &recordings).unwrap();
        let final_audio = recordings.join("processing-final.wav");
        fs::write(&final_audio, b"wav").unwrap();
        let missing_final = recordings.join("recording-part.wav");
        let partial_audio = crate::audio::part_path_for(&missing_final);
        fs::write(&partial_audio, b"partial").unwrap();
        let missing_processing_final = recordings.join("processing-part.wav");
        let processing_partial_audio = crate::audio::part_path_for(&missing_processing_final);
        fs::write(&processing_partial_audio, b"partial").unwrap();
        let outside_dir = temp.path().join("outside");
        fs::create_dir_all(&outside_dir).unwrap();
        let malicious_final = outside_dir.join("outside.wav");
        let malicious_partial = crate::audio::part_path_for(&malicious_final);
        fs::write(&malicious_partial, b"must remain").unwrap();

        let mut final_row = recording("processing-final", RecordingStatus::Processing, None);
        final_row.audio_path = Some(final_audio.to_string_lossy().into_owned());
        storage.insert_recording(&final_row).unwrap();
        let mut partial_row = recording("recording-part", RecordingStatus::Recording, None);
        partial_row.audio_path = Some(missing_final.to_string_lossy().into_owned());
        storage.insert_recording(&partial_row).unwrap();
        let mut processing_partial_row =
            recording("processing-part", RecordingStatus::Processing, None);
        processing_partial_row.audio_path =
            Some(missing_processing_final.to_string_lossy().into_owned());
        storage.insert_recording(&processing_partial_row).unwrap();
        let mut malicious_row = recording("outside", RecordingStatus::Processing, None);
        malicious_row.audio_path = Some(malicious_final.to_string_lossy().into_owned());
        storage.insert_recording(&malicious_row).unwrap();
        drop(storage);

        let reopened = Storage::open(&database, &recordings).unwrap();

        let final_row = reopened.get_recording("processing-final").unwrap().unwrap();
        assert_eq!(final_row.status, RecordingStatus::Failed);
        assert_eq!(final_row.error.as_deref(), Some(INTERRUPTED_ERROR));
        assert_eq!(
            final_row.audio_path.as_deref(),
            Some(final_audio.to_string_lossy().as_ref())
        );
        assert!(final_audio.is_file());

        let partial_row = reopened.get_recording("recording-part").unwrap().unwrap();
        assert_eq!(partial_row.status, RecordingStatus::Failed);
        assert_eq!(
            partial_row.error.as_deref(),
            Some(INTERRUPTED_BEFORE_FINALIZE_ERROR)
        );
        assert!(partial_row.audio_path.is_none());
        assert!(!partial_audio.exists());

        let processing_partial_row = reopened.get_recording("processing-part").unwrap().unwrap();
        assert_eq!(processing_partial_row.status, RecordingStatus::Failed);
        assert_eq!(
            processing_partial_row.error.as_deref(),
            Some(INTERRUPTED_BEFORE_FINALIZE_ERROR)
        );
        assert!(processing_partial_row.audio_path.is_none());
        assert!(!processing_partial_audio.exists());

        let malicious_row = reopened.get_recording("outside").unwrap().unwrap();
        assert_eq!(malicious_row.status, RecordingStatus::Failed);
        assert_eq!(
            malicious_row.error.as_deref(),
            Some(INTERRUPTED_BEFORE_FINALIZE_ERROR)
        );
        assert!(malicious_row.audio_path.is_none());
        assert!(malicious_partial.is_file());
    }

    #[test]
    fn reopening_preserves_finalized_audio_from_recording_stop_crash_window() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("mow.sqlite3");
        let recordings = temp.path().join("recordings");
        fs::create_dir_all(&recordings).unwrap();
        let final_audio = recordings.join("recording-final.wav");
        fs::write(&final_audio, b"wav").unwrap();
        let storage = Storage::open(&database, &recordings).unwrap();
        let mut row = recording("recording-final", RecordingStatus::Recording, None);
        row.audio_path = Some(final_audio.to_string_lossy().into_owned());
        storage.insert_recording(&row).unwrap();
        drop(storage);

        let reopened = Storage::open(&database, &recordings).unwrap();

        let recovered = reopened.get_recording("recording-final").unwrap().unwrap();
        assert_eq!(recovered.status, RecordingStatus::Failed);
        assert_eq!(recovered.error.as_deref(), Some(INTERRUPTED_ERROR));
        assert_eq!(
            recovered.audio_path.as_deref(),
            Some(final_audio.to_string_lossy().as_ref())
        );
        assert!(final_audio.is_file());
    }

    #[test]
    fn history_crud_search_filter_retry_and_delete() {
        let (_temp, storage) = storage();
        storage
            .insert_recording(&recording(
                "a",
                RecordingStatus::Completed,
                Some("Zażółć gęślą jaźń"),
            ))
            .unwrap();
        storage
            .insert_recording(&recording(
                "b",
                RecordingStatus::Failed,
                Some("Hello world"),
            ))
            .unwrap();

        assert_eq!(
            storage
                .list_history(&HistoryQuery {
                    search: Some("gęślą".into()),
                    status: None,
                })
                .unwrap()
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
        assert_eq!(
            storage
                .list_history(&HistoryQuery {
                    search: None,
                    status: Some(RecordingStatus::Failed),
                })
                .unwrap()[0]
                .id,
            "b"
        );
        storage.mark_retrying("b").unwrap();
        assert_eq!(
            storage.get_recording("b").unwrap().unwrap().status,
            RecordingStatus::Processing
        );
        storage.delete_recording("a").unwrap();
        assert!(storage.get_recording("a").unwrap().is_none());
    }

    #[test]
    fn typed_settings_and_vocabulary_round_trip_json() {
        let (_temp, storage) = storage();
        storage.set_setting("autopaste", &true).unwrap();
        storage
            .set_setting("retention", &Retention::Days(7))
            .unwrap();
        let entry = storage.add_vocabulary("żółw", "Żółw™").unwrap();

        assert_eq!(
            storage.get_setting::<bool>("autopaste").unwrap(),
            Some(true)
        );
        assert_eq!(
            storage.get_setting::<Retention>("retention").unwrap(),
            Some(Retention::Days(7))
        );
        assert_eq!(storage.list_vocabulary().unwrap(), vec![entry.clone()]);
        storage.delete_vocabulary(entry.id).unwrap();
        assert!(storage.list_vocabulary().unwrap().is_empty());
    }

    #[test]
    fn postprocessing_handles_polish_diacritics_phrases_and_punctuation() {
        let vocabulary = vec![
            VocabularyEntry {
                id: 1,
                heard: "zolw".into(),
                replacement: "żółw".into(),
            },
            VocabularyEntry {
                id: 2,
                heard: "nowy sącz".into(),
                replacement: "Nowy Sącz".into(),
            },
        ];

        assert_eq!(
            postprocess("  ZOLW   jest w nowy SĄCZ  , naprawdę ?  ", &vocabulary),
            "żółw jest w Nowy Sącz, naprawdę?"
        );
        assert_eq!(postprocess("przyzolwowy", &vocabulary), "przyzolwowy");
    }

    #[test]
    fn retention_only_removes_old_files_inside_managed_directory() {
        let (temp, storage) = storage();
        let managed = storage.recordings_dir().join("old.wav");
        fs::create_dir_all(storage.recordings_dir()).unwrap();
        fs::write(&managed, b"audio").unwrap();
        let outside = temp.path().join("outside.wav");
        fs::write(&outside, b"do not delete").unwrap();
        let mut safe = recording("safe", RecordingStatus::Completed, Some("old"));
        safe.created_at = 0;
        safe.audio_path = Some(managed.to_string_lossy().into_owned());
        storage.insert_recording(&safe).unwrap();
        let mut traversal = recording("traversal", RecordingStatus::Completed, Some("old"));
        traversal.created_at = 0;
        traversal.audio_path = Some(
            storage
                .recordings_dir()
                .join("..")
                .join("outside.wav")
                .to_string_lossy()
                .into_owned(),
        );
        storage.insert_recording(&traversal).unwrap();

        let report = storage
            .cleanup_retention(Retention::Days(1), 172_800_000)
            .unwrap();

        assert_eq!(report.deleted_audio, 1);
        assert!(!managed.exists());
        assert!(outside.exists());
        let safe = storage.get_recording("safe").unwrap().unwrap();
        assert_eq!(safe.status, RecordingStatus::Completed);
        assert_eq!(safe.text.as_deref(), Some("old"));
        assert!(safe.audio_path.is_none());
        assert!(
            storage
                .get_recording("traversal")
                .unwrap()
                .unwrap()
                .audio_path
                .is_some()
        );
    }
}
