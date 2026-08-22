use crate::audio::part_path_for;
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
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
    /// Amplitude envelope, one byte per bucket, `0..=255` mapping to `0.0..=1.0`.
    /// `None` for recordings captured before envelopes were stored, and while
    /// a capture is still running. It survives retention pruning: the envelope
    /// describes the recording, not the audio file, and stays useful in the
    /// history list after the WAV is gone.
    #[serde(default)]
    pub peaks: Option<Vec<u8>>,
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

struct RetentionReservation {
    id: String,
    original_path: String,
    canonical_path: PathBuf,
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
               error TEXT,
               peaks BLOB
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
            let columns: Vec<String> = self
                .connection()?
                .prepare("PRAGMA table_info(modes)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<_, _>>()?;
            let mut migrations = String::new();
            for (column, definition) in [
                ("name", "TEXT NOT NULL DEFAULT ''"),
                ("description", "TEXT NOT NULL DEFAULT ''"),
                ("prompt", "TEXT NOT NULL DEFAULT ''"),
                ("enabled", "INTEGER NOT NULL DEFAULT 1"),
                ("is_default", "INTEGER NOT NULL DEFAULT 0"),
                ("created_at", "INTEGER NOT NULL DEFAULT 0"),
            ] {
                if !columns.iter().any(|existing| existing == column) {
                    migrations.push_str(&format!(
                        "ALTER TABLE modes ADD COLUMN {column} {definition};"
                    ));
                }
            }
            if !migrations.is_empty() {
                self.connection()?.execute_batch(&migrations)?;
            }
        }
        if version < 3 {
            // Recordings captured before this column existed keep a NULL
            // envelope; the interface renders those rows without a waveform
            // rather than inventing one.
            let columns: Vec<String> = self
                .connection()?
                .prepare("PRAGMA table_info(history)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<_, _>>()?;
            if !columns.iter().any(|column| column == "peaks") {
                self.connection()?
                    .execute_batch("ALTER TABLE history ADD COLUMN peaks BLOB;")?;
            }
        }
        self.connection()?.execute_batch(
            "INSERT OR IGNORE INTO modes
               (id,config_json,name,description,prompt,enabled,is_default,created_at)
             VALUES
               ('clean','{}','Clean','Light text normalization','',1,1,0),
               ('message','{}','Message','Natural message style','',1,0,1),
               ('code','{}','Code','Dictating technical terms','',1,0,2);
             UPDATE schema_version SET version = 3 WHERE version < 3;",
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
             (id, created_at, duration_ms, status, text, model, audio_path, source_app, error, peaks)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                recording.peaks,
            ],
        )?;
        Ok(())
    }

    pub fn update_recording(&self, recording: &Recording) -> Result<bool, StorageError> {
        Ok(self.connection()?.execute(
            "UPDATE history SET created_at=?2, duration_ms=?3, status=?4, text=?5,
             model=?6, audio_path=?7, source_app=?8, error=?9, peaks=?10 WHERE id=?1",
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
                recording.peaks,
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

    /// Stores the amplitude envelope captured while the recording ran.
    pub fn store_peaks(&self, id: &str, peaks: &[u8]) -> Result<bool, StorageError> {
        Ok(self
            .connection()?
            .execute("UPDATE history SET peaks=?2 WHERE id=?1", params![id, peaks])?
            > 0)
    }

    pub fn mark_retrying(&self, id: &str) -> Result<bool, StorageError> {
        self.update_status(id, RecordingStatus::Processing, None, None, None, None)
    }

    pub fn get_recording(&self, id: &str) -> Result<Option<Recording>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, created_at, duration_ms, status, text, model, audio_path, source_app, error, peaks
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
            "SELECT id, created_at, duration_ms, status, text, model, audio_path, source_app, error, peaks
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
        self.cleanup_retention_with(retention, now_ms, |path| fs::remove_file(path))
    }

    fn cleanup_retention_with(
        &self,
        retention: Retention,
        now_ms: i64,
        mut remove: impl FnMut(&Path) -> std::io::Result<()>,
    ) -> Result<CleanupReport, StorageError> {
        let reserved = self.reserve_retention_records(retention, now_ms)?;
        let mut deleted_audio = 0;
        for (index, reservation) in reserved.iter().enumerate() {
            if let Err(error) = remove(&reservation.canonical_path) {
                self.restore_retention_reservations(&reserved[index..])?;
                return Err(error.into());
            }
            deleted_audio += 1;
        }
        Ok(CleanupReport { deleted_audio })
    }

    #[cfg(test)]
    pub(crate) fn reserve_retention_audio(
        &self,
        retention: Retention,
        now_ms: i64,
    ) -> Result<Vec<PathBuf>, StorageError> {
        self.reserve_retention_records(retention, now_ms)
            .map(|reservations| {
                reservations
                    .into_iter()
                    .map(|reservation| reservation.canonical_path)
                    .collect()
            })
    }

    fn reserve_retention_records(
        &self,
        retention: Retention,
        now_ms: i64,
    ) -> Result<Vec<RetentionReservation>, StorageError> {
        let Retention::Days(days @ (1 | 7 | 30)) = retention else {
            return Ok(Vec::new());
        };
        let cutoff = now_ms.saturating_sub(i64::from(days) * 86_400_000);
        fs::create_dir_all(&self.recordings_dir)?;
        let managed_root = self.recordings_dir.canonicalize()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates: Vec<(String, String)> = {
            let mut statement = transaction.prepare(
                "SELECT id,audio_path FROM history
                 WHERE created_at < ?1 AND audio_path IS NOT NULL
                   AND status IN ('completed','failed','cancelled')
                 ORDER BY created_at ASC,id ASC",
            )?;
            statement
                .query_map([cutoff], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut reserved = Vec::new();
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
                let changed = transaction.execute(
                    "UPDATE history SET audio_path=NULL
                     WHERE id=?1 AND audio_path=?2
                       AND status IN ('completed','failed','cancelled')",
                    params![id, path.to_string_lossy().as_ref()],
                )?;
                if changed > 0 {
                    reserved.push(RetentionReservation {
                        id,
                        original_path: path.to_string_lossy().into_owned(),
                        canonical_path: canonical,
                    });
                }
            }
        }
        transaction.commit()?;
        Ok(reserved)
    }

    fn restore_retention_reservations(
        &self,
        reservations: &[RetentionReservation],
    ) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for reservation in reservations {
            transaction.execute(
                "UPDATE history SET audio_path=?2
                 WHERE id=?1 AND audio_path IS NULL
                   AND status IN ('completed','failed','cancelled')",
                params![reservation.id, reservation.original_path],
            )?;
        }
        transaction.commit()?;
        Ok(())
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
    peaks: Option<Vec<u8>>,
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
        peaks: row.get(9)?,
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
            peaks: raw.peaks,
        })
    }
}

pub fn postprocess(text: &str, vocabulary: &[VocabularyEntry]) -> String {
    let whitespace = Regex::new(r"\s+").expect("static whitespace regex is valid");
    let punctuation = Regex::new(r"\s+([,.;:!?])").expect("static punctuation regex is valid");
    let processed = whitespace.replace_all(text.trim(), " ").into_owned();
    let processed = apply_vocabulary(processed, vocabulary);
    punctuation.replace_all(&processed, "$1").into_owned()
}

fn clean_paragraphs(text: &str, vocabulary: &[VocabularyEntry]) -> String {
    let paragraph_break =
        Regex::new(r"(?:\r?\n)[ \t]*(?:\r?\n)+").expect("static paragraph regex is valid");
    paragraph_break
        .split(text.trim())
        .filter_map(|paragraph| {
            let cleaned = postprocess(paragraph, vocabulary);
            (!cleaned.is_empty()).then_some(cleaned)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn postprocess_for_mode(text: &str, vocabulary: &[VocabularyEntry], mode: &Mode) -> String {
    if !mode.enabled {
        return clean_paragraphs(text, vocabulary);
    }
    match mode.id.as_str() {
        "clean" => clean_paragraphs(text, vocabulary),
        "message" => postprocess(text, vocabulary),
        "code" => {
            let punctuation =
                Regex::new(r"[ \t]+([,.;:!?])").expect("static punctuation regex is valid");
            let processed = apply_vocabulary(text.trim().to_owned(), vocabulary);
            punctuation.replace_all(&processed, "$1").into_owned()
        }
        _ => apply_custom_directives(clean_paragraphs(text, vocabulary), &mode.prompt),
    }
}

fn apply_custom_directives(mut text: String, prompt: &str) -> String {
    let mut case = None;
    let mut layout = None;
    let mut prefix = None;
    let mut suffix = None;
    for line in prompt.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "case" if matches!(value, "lower" | "upper" | "sentence") => case = Some(value),
            "layout" if matches!(value, "bullets" | "plain") => layout = Some(value),
            "prefix" => prefix = Some(value),
            "suffix" => suffix = Some(value),
            _ => {}
        }
    }
    text = match case {
        Some("lower") => text.to_lowercase(),
        Some("upper") => text.to_uppercase(),
        Some("sentence") => sentence_case(&text),
        _ => text,
    };
    if layout == Some("bullets") {
        text = text
            .split("\n\n")
            .filter(|item| !item.trim().is_empty())
            .map(|item| format!("• {}", item.trim()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let mut parts = Vec::new();
    if let Some(prefix) = prefix.filter(|value| !value.is_empty()) {
        parts.push(prefix.to_owned());
    }
    if !text.is_empty() {
        parts.push(text);
    }
    if let Some(suffix) = suffix.filter(|value| !value.is_empty()) {
        parts.push(suffix.to_owned());
    }
    parts.join("\n")
}

fn sentence_case(text: &str) -> String {
    let mut sentence_start = true;
    let mut result = String::new();
    for character in text.to_lowercase().chars() {
        if sentence_start && character.is_alphabetic() {
            result.extend(character.to_uppercase());
            sentence_start = false;
        } else {
            result.push(character);
        }
        if matches!(character, '.' | '!' | '?') {
            sentence_start = true;
        }
    }
    result
}

fn apply_vocabulary(mut processed: String, vocabulary: &[VocabularyEntry]) -> String {
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
        processed = fuzzy_apply_vocabulary(processed, &entry);
    }
    processed
}

fn fuzzy_apply_vocabulary(text: String, entry: &VocabularyEntry) -> String {
    let heard = entry.heard.trim().to_lowercase();
    if heard.is_empty() {
        return text;
    }
    let replacement = entry.replacement.trim();
    let mut rebuilt = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, rebuilt: &mut String| {
        if word.is_empty() {
            return;
        }
        let word_lower = word.to_lowercase();
        let word_clean = word_lower.trim_matches(|c: char| !c.is_alphanumeric());
        if !word_clean.is_empty() && levenshtein(word_clean, &heard) <= 1 {
            rebuilt.push_str(replacement);
        } else {
            rebuilt.push_str(word);
        }
        word.clear();
    };
    for character in text.chars() {
        if character.is_whitespace() {
            flush(&mut word, &mut rebuilt);
            rebuilt.push(character);
        } else {
            word.push(character);
        }
    }
    flush(&mut word, &mut rebuilt);
    rebuilt
}

fn levenshtein(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (i, lc) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, rc) in right.iter().enumerate() {
            let cost = if lc == rc { 0 } else { 1 };
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
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
            peaks: None,
        }
    }

    #[test]
    fn migrates_all_tables_and_records_schema_version() {
        let (_temp, storage) = storage();

        assert_eq!(storage.schema_version().unwrap(), 3);
        assert_eq!(
            storage.table_names().unwrap(),
            vec!["history", "modes", "settings", "vocabulary"]
        );
    }

    #[test]
    fn reopening_an_older_database_with_partial_migration_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("mow.sqlite3");
        let recordings = temp.path().join("recordings");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE modes (
                   id TEXT PRIMARY KEY,
                   config_json TEXT NOT NULL,
                   name TEXT NOT NULL DEFAULT '',
                   description TEXT NOT NULL DEFAULT '',
                   prompt TEXT NOT NULL DEFAULT '',
                   enabled INTEGER NOT NULL DEFAULT 1,
                   is_default INTEGER NOT NULL DEFAULT 0,
                   created_at INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version(version) VALUES (0);",
            )
            .unwrap();

        let storage = Storage::open(&database, &recordings).unwrap();

        assert_eq!(storage.schema_version().unwrap(), 3);
        assert_eq!(
            storage.list_modes().unwrap().iter().map(|mode| mode.id.clone()).collect::<Vec<_>>(),
            vec!["clean", "message", "code"]
        );
    }

    #[test]
    fn upgrading_a_database_without_the_peaks_column_keeps_existing_rows() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("mow.sqlite3");
        let recordings = temp.path().join("recordings");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE history (
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
                 INSERT INTO history(id,created_at,status,text)
                   VALUES ('old','1','completed','spoken before envelopes existed');
                 CREATE TABLE modes (
                   id TEXT PRIMARY KEY,
                   config_json TEXT NOT NULL,
                   name TEXT NOT NULL DEFAULT '',
                   description TEXT NOT NULL DEFAULT '',
                   prompt TEXT NOT NULL DEFAULT '',
                   enabled INTEGER NOT NULL DEFAULT 1,
                   is_default INTEGER NOT NULL DEFAULT 0,
                   created_at INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version(version) VALUES (2);",
            )
            .unwrap();
        drop(connection);

        let storage = Storage::open(&database, &recordings).unwrap();

        let migrated = storage.get_recording("old").unwrap().unwrap();
        assert_eq!(migrated.text.as_deref(), Some("spoken before envelopes existed"));
        assert_eq!(migrated.peaks, None);
        assert_eq!(storage.schema_version().unwrap(), 3);
    }

    #[test]
    fn envelopes_round_trip_through_storage() {
        let (_temp, storage) = storage();
        storage
            .insert_recording(&recording("a", RecordingStatus::Completed, Some("hello")))
            .unwrap();

        assert!(storage.store_peaks("a", &[0, 128, 255]).unwrap());

        assert_eq!(
            storage.get_recording("a").unwrap().unwrap().peaks,
            Some(vec![0, 128, 255])
        );
    }

    #[test]
    fn storing_an_envelope_for_a_missing_recording_reports_no_match() {
        let (_temp, storage) = storage();

        assert!(!storage.store_peaks("gone", &[1, 2, 3]).unwrap());
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
            vec!["Clean", "Message", "Code"]
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
    fn vocabulary_approximates_similar_sounding_words() {
        let vocabulary = vec![VocabularyEntry {
            id: 1,
            heard: "parakit".into(),
            replacement: "Parakeet".into(),
        }];

        assert_eq!(
            postprocess("uruchom parakyt i zobacz", &vocabulary),
            "uruchom Parakeet i zobacz"
        );
        assert_eq!(
            postprocess("parakit jest gotowy", &vocabulary),
            "Parakeet jest gotowy"
        );
        assert_eq!(
            postprocess("przetestujmy parakitta", &vocabulary),
            "przetestujmy parakitta"
        );
    }

    fn mode(id: &str, prompt: &str) -> Mode {
        Mode {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            prompt: prompt.into(),
            enabled: true,
            is_default: false,
            created_at: 1,
        }
    }

    #[test]
    fn built_in_modes_have_distinct_layout_contracts() {
        let vocabulary = vec![VocabularyEntry {
            id: 1,
            heard: "parakit".into(),
            replacement: "Parakeet".into(),
        }];
        let input = "pierwszy   parakit ;\nciąg\n\nDrugi akapit !";

        assert_eq!(
            postprocess_for_mode(input, &vocabulary, &mode("clean", "")),
            "pierwszy Parakeet; ciąg\n\nDrugi akapit!"
        );
        assert_eq!(
            postprocess_for_mode(input, &vocabulary, &mode("message", "")),
            "pierwszy Parakeet; ciąg Drugi akapit!"
        );
        assert_eq!(
            postprocess_for_mode(
                "fn main() {\n    parakit () ;\n}",
                &vocabulary,
                &mode("code", "")
            ),
            "fn main() {\n    Parakeet ();\n}"
        );
    }

    #[test]
    fn custom_mode_applies_documented_local_directives() {
        let custom = mode(
            "custom-notes",
            "case: upper\nlayout: bullets\nprefix: NOTATKA\nsuffix: KONIEC",
        );

        assert_eq!(
            postprocess_for_mode("Pierwszy punkt.\n\nDrugi punkt.", &[], &custom),
            "NOTATKA\n• PIERWSZY PUNKT.\n• DRUGI PUNKT.\nKONIEC"
        );
    }

    #[test]
    fn custom_mode_without_recognized_directive_falls_back_to_clean() {
        let custom = mode("custom-freeform", "Napisz to pięknie jak poeta");

        assert_eq!(
            postprocess_for_mode("Pierwszy\n\nDrugi", &[], &custom),
            "Pierwszy\n\nDrugi"
        );
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

    #[test]
    fn retention_reservation_is_conditional_on_terminal_status_and_audio_path() {
        let (temp, storage) = storage();
        let recordings = storage.recordings_dir().to_path_buf();
        fs::create_dir_all(&recordings).unwrap();
        let completed_path = recordings.join("completed.wav");
        let processing_path = recordings.join("processing.wav");
        fs::write(&completed_path, b"wav").unwrap();
        fs::write(&processing_path, b"wav").unwrap();
        let mut completed = recording("completed-reserved", RecordingStatus::Completed, None);
        completed.created_at = 0;
        completed.audio_path = Some(completed_path.to_string_lossy().into_owned());
        storage.insert_recording(&completed).unwrap();
        let mut processing = recording("processing-survives", RecordingStatus::Processing, None);
        processing.created_at = 0;
        processing.audio_path = Some(processing_path.to_string_lossy().into_owned());
        storage.insert_recording(&processing).unwrap();

        let reserved = storage
            .reserve_retention_audio(Retention::Days(1), 172_800_000)
            .unwrap();

        assert_eq!(reserved, vec![completed_path.canonicalize().unwrap()]);
        assert!(
            storage
                .get_recording("completed-reserved")
                .unwrap()
                .unwrap()
                .audio_path
                .is_none()
        );
        assert_eq!(
            storage
                .get_recording("processing-survives")
                .unwrap()
                .unwrap()
                .audio_path
                .as_deref(),
            Some(processing_path.to_string_lossy().as_ref())
        );
        assert!(processing_path.is_file());
        drop(temp);
    }

    #[test]
    fn retention_delete_failure_restores_failing_and_unprocessed_reservations() {
        let (_temp, storage) = storage();
        let recordings = storage.recordings_dir().to_path_buf();
        fs::create_dir_all(&recordings).unwrap();
        let mut paths = Vec::new();
        for (index, id) in ["first", "second", "third"].into_iter().enumerate() {
            let path = recordings.join(format!("{id}.wav"));
            fs::write(&path, b"wav").unwrap();
            let mut row = recording(id, RecordingStatus::Completed, None);
            row.created_at = index as i64;
            row.audio_path = Some(path.to_string_lossy().into_owned());
            storage.insert_recording(&row).unwrap();
            paths.push(path);
        }
        let attempts = std::cell::Cell::new(0);

        let error = storage
            .cleanup_retention_with(Retention::Days(1), 172_800_000, |path| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    return Err(std::io::Error::other("forced second delete failure"));
                }
                fs::remove_file(path)
            })
            .unwrap_err();

        assert!(matches!(error, StorageError::Io(_)));
        assert!(!paths[0].exists());
        assert!(paths[1].is_file());
        assert!(paths[2].is_file());
        assert!(
            storage
                .get_recording("first")
                .unwrap()
                .unwrap()
                .audio_path
                .is_none()
        );
        for (id, path) in [("second", &paths[1]), ("third", &paths[2])] {
            assert_eq!(
                storage
                    .get_recording(id)
                    .unwrap()
                    .unwrap()
                    .audio_path
                    .as_deref(),
                Some(path.to_string_lossy().as_ref())
            );
        }

        assert_eq!(
            storage
                .cleanup_retention(Retention::Days(1), 172_800_000)
                .unwrap()
                .deleted_audio,
            2
        );
        assert!(!paths[1].exists());
        assert!(!paths[2].exists());
    }
}
