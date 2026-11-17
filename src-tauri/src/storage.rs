use crate::audio::part_path_for;
use crate::vocabulary::Vocabulary;
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    /// The spelling the user wants: "Claude Code", "try again".
    pub term: String,
    /// Exact renderings the recogniser produces for it ("parakit").
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl VocabularyEntry {
    pub fn matcher(entries: &[Self]) -> Vocabulary {
        Vocabulary::new(entries.iter().map(|entry| (entry.term.as_str(), entry.aliases.as_slice())))
    }
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
    /// Expired files that could not be deleted this pass; their rows keep the path.
    pub skipped_audio: usize,
}

pub struct Storage {
    connection: Mutex<Connection>,
    recordings_dir: PathBuf,
    /// The user's everyday words and how many transcripts they came from.
    everyday: Mutex<Option<(i64, Arc<HashSet<String>>)>>,
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
            everyday: Mutex::new(None),
        };
        storage.migrate()?;
        storage.reconcile_interrupted()?;
        storage.reconcile_missing_audio()?;
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
               term TEXT NOT NULL COLLATE NOCASE UNIQUE,
               aliases_json TEXT NOT NULL DEFAULT '[]'
             );",
        )?;
        let version: i64 =
            self.connection()?
                .query_row("SELECT version FROM schema_version", [], |row| row.get(0))?;
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
        if version < 4 {
            self.migrate_vocabulary_to_terms()?;
        }
        // Cloud engines are gone; the API keys they stored are not kept
        // around. Idempotent, so it needs no version of its own.
        self.connection()?
            .execute("DELETE FROM settings WHERE key='cloud_keys'", [])?;
        Ok(())
    }

    /// Version 4: modes are gone, and a vocabulary entry is a term with
    /// aliases. Every old `heard -> replacement` pair keeps working as an
    /// alias of its replacement; pairs sharing a replacement become one term.
    fn migrate_vocabulary_to_terms(&self) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let columns: Vec<String> = transaction
            .prepare("PRAGMA table_info(vocabulary)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        if columns.iter().any(|column| column == "heard") {
            let pairs: Vec<(String, String)> = transaction
                .prepare("SELECT heard,replacement FROM vocabulary ORDER BY id")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<_, _>>()?;
            let mut terms: Vec<(String, Vec<String>)> = Vec::new();
            for (heard, replacement) in pairs {
                let (heard, replacement) = (heard.trim().to_owned(), replacement.trim().to_owned());
                if replacement.is_empty() {
                    continue;
                }
                let index = match terms.iter().position(|(term, _)| term.to_lowercase() == replacement.to_lowercase()) {
                    Some(index) => index,
                    None => {
                        terms.push((replacement.clone(), Vec::new()));
                        terms.len() - 1
                    }
                };
                merge_aliases(&mut terms[index].1, &replacement, [heard]);
            }
            transaction.execute_batch(
                "DROP TABLE vocabulary;
                 CREATE TABLE vocabulary (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   term TEXT NOT NULL COLLATE NOCASE UNIQUE,
                   aliases_json TEXT NOT NULL DEFAULT '[]'
                 );",
            )?;
            for (term, aliases) in terms {
                transaction.execute(
                    "INSERT INTO vocabulary(term,aliases_json) VALUES (?1,?2)",
                    params![term, serde_json::to_string(&aliases)?],
                )?;
            }
        }
        transaction.execute_batch("DROP TABLE IF EXISTS modes; UPDATE schema_version SET version = 4;")?;
        transaction.commit()?;
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

    pub fn mark_retrying(&self, id: &str) -> Result<bool, StorageError> {
        self.update_status(id, RecordingStatus::Processing, None, None, None, None)
    }

    /// Stores the amplitude envelope captured while the recording ran.
    pub fn store_peaks(&self, id: &str, peaks: &[u8]) -> Result<bool, StorageError> {
        Ok(self
            .connection()?
            .execute("UPDATE history SET peaks=?2 WHERE id=?1", params![id, peaks])?
            > 0)
    }

    pub fn clear_audio_path(&self, id: &str) -> Result<bool, StorageError> {
        Ok(self.connection()?.execute(
            "UPDATE history SET audio_path=NULL WHERE id=?1",
            [id],
        )? > 0)
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

    /// Adds a term, or merges the aliases into the term already saved under
    /// the same spelling (case-insensitively), and returns the saved entry.
    pub fn add_vocabulary(&self, term: &str, aliases: &[String]) -> Result<VocabularyEntry, StorageError> {
        let term = term.trim();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row("SELECT id,term,aliases_json FROM vocabulary WHERE term=?1", [term], vocabulary_from_row)
            .optional()?;
        let mut entry = existing.unwrap_or(VocabularyEntry { id: 0, term: term.to_owned(), aliases: Vec::new() });
        merge_aliases(&mut entry.aliases, &entry.term, aliases.iter().cloned());
        let aliases_json = serde_json::to_string(&entry.aliases)?;
        if entry.id == 0 {
            transaction.execute("INSERT INTO vocabulary(term,aliases_json) VALUES (?1,?2)", params![entry.term, aliases_json])?;
            entry.id = transaction.last_insert_rowid();
        } else {
            transaction.execute("UPDATE vocabulary SET aliases_json=?2 WHERE id=?1", params![entry.id, aliases_json])?;
        }
        transaction.commit()?;
        Ok(entry)
    }

    pub fn list_vocabulary(&self) -> Result<Vec<VocabularyEntry>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT id,term,aliases_json FROM vocabulary ORDER BY id")?;
        Ok(statement.query_map([], vocabulary_from_row)?.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_vocabulary(&self, id: i64) -> Result<bool, StorageError> {
        Ok(self
            .connection()?
            .execute("DELETE FROM vocabulary WHERE id=?1", [id])?
            > 0)
    }

    /// The newest completed transcripts, for learning how the user speaks.
    pub fn recent_transcripts(&self, limit: usize) -> Result<Vec<String>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT text FROM history WHERE status='completed' AND text IS NOT NULL
             ORDER BY created_at DESC LIMIT ?1",
        )?;
        Ok(statement.query_map([limit as i64], |row| row.get(0))?.collect::<Result<Vec<_>, _>>()?)
    }

    /// Words the user says in several transcripts: their own vocabulary, which
    /// a term is never allowed to overwrite by a near match. Cached, and only
    /// recounted after another batch of transcripts has been saved.
    pub fn everyday_words(&self) -> Arc<HashSet<String>> {
        let Ok(count) = self.connection().and_then(|connection| {
            Ok(connection.query_row("SELECT COUNT(*) FROM history WHERE status='completed'", [], |row| row.get::<_, i64>(0))?)
        }) else {
            return Arc::default();
        };
        if let Ok(cached) = self.everyday.lock()
            && let Some((counted, words)) = cached.as_ref()
            && (count - counted).abs() < 25
        {
            return words.clone();
        }
        let texts = self.recent_transcripts(3000).unwrap_or_default();
        let words = Arc::new(crate::vocabulary::frequent_words(texts.iter().map(String::as_str), 3));
        if let Ok(mut cached) = self.everyday.lock() {
            *cached = Some((count, words.clone()));
        }
        words
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

    /// Failed, cancelled, and completed rows can keep an `audio_path` after the
    /// file has already been deleted (overflow used to wipe the take on stop;
    /// cancel used to discard `.part` while the row still pointed at `.wav`).
    /// History then claimed audio was saved and Retry pointed at nothing.
    pub fn reconcile_missing_audio(&self) -> Result<usize, StorageError> {
        let missing_ids: Vec<String> = {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT id,audio_path FROM history
                 WHERE audio_path IS NOT NULL
                   AND status IN ('failed','cancelled','completed')",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter()
                .filter_map(|(id, path)| {
                    let path = path?;
                    if Path::new(&path).is_file() {
                        None
                    } else {
                        Some(id)
                    }
                })
                .collect()
        };
        if missing_ids.is_empty() {
            return Ok(0);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for id in &missing_ids {
            transaction.execute("UPDATE history SET audio_path=NULL WHERE id=?1", [id])?;
        }
        transaction.commit()?;
        Ok(missing_ids.len())
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
        // One locked or busy file must not keep every other expired take on
        // disk: skip it, give its row the path back, and try again next pass.
        let mut skipped = Vec::new();
        for reservation in reserved {
            match remove(&reservation.canonical_path) {
                Ok(()) => deleted_audio += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => deleted_audio += 1,
                Err(_) => skipped.push(reservation),
            }
        }
        if !skipped.is_empty() { self.restore_retention_reservations(&skipped)?; }
        Ok(CleanupReport { deleted_audio, skipped_audio: skipped.len() })
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

fn vocabulary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VocabularyEntry> {
    let aliases: String = row.get(2)?;
    Ok(VocabularyEntry { id: row.get(0)?, term: row.get(1)?, aliases: serde_json::from_str(&aliases).unwrap_or_default() })
}

/// Adds aliases that are new, not empty and not the term itself.
fn merge_aliases(aliases: &mut Vec<String>, term: &str, new: impl IntoIterator<Item = String>) {
    for alias in new {
        let alias = alias.trim();
        let lower = alias.to_lowercase();
        if !alias.is_empty() && lower != term.to_lowercase() && !aliases.iter().any(|known| known.to_lowercase() == lower) {
            aliases.push(alias.to_owned());
        }
    }
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

/// The one pipeline every transcript goes through: whitespace and
/// punctuation cleanup, the user's vocabulary, then sentence case.
/// Paragraph breaks the recogniser produced are kept.
pub fn finish_transcript(text: &str, vocabulary: &Vocabulary) -> String {
    normalize_transcript(&clean_paragraphs(text, vocabulary))
}

fn clean(text: &str, vocabulary: &Vocabulary) -> String {
    let whitespace = Regex::new(r"\s+").expect("static whitespace regex is valid");
    // Only a mark that closes the word before it is pulled in: ".env" or
    // ":root" after a space is a token of its own, not stray punctuation.
    let punctuation = Regex::new(r"\s+([,.;:!?])([^\p{L}\p{N}/\\_]|$)")
        .expect("static punctuation regex is valid");
    let processed = whitespace.replace_all(text.trim(), " ");
    let processed = vocabulary.apply(&processed);
    punctuation.replace_all(&processed, "$1$2").into_owned()
}

fn clean_paragraphs(text: &str, vocabulary: &Vocabulary) -> String {
    let paragraph_break =
        Regex::new(r"(?:\r?\n)[ \t]*(?:\r?\n)+").expect("static paragraph regex is valid");
    paragraph_break
        .split(text.trim())
        .filter_map(|paragraph| {
            let cleaned = clean(paragraph, vocabulary);
            (!cleaned.is_empty()).then_some(cleaned)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Abbreviations whose trailing period does not end a sentence, so the word
/// after them keeps its case ("np. coś", "m.in. to").
const NON_TERMINAL_ABBREVIATIONS: &[&str] = &[
    "np", "tzn", "tj", "tzw", "itd", "itp", "m.in", "ok", "godz", "ul", "al", "nr", "str", "dr",
    "prof", "mgr", "inż", "hab", "ks", "św", "pt", "por", "wg", "zob", "ww", "jw", "sp", "z o.o",
    "e.g", "i.e", "etc", "vs", "mr", "mrs", "ms", "no", "approx", "cf",
];

/// Final, conservative tidy-up of a delivered transcript.
///
/// Parakeet normally writes sentence case itself, but a take that opens with
/// a conjunction ("a próbowałeś…") or after a restart can come back starting
/// lower case. This upper-
/// cases the first letter of the transcript and of every sentence that
/// follows `.`, `?` or `!` plus whitespace, collapses runs of spaces inside a
/// line (indentation is kept), drops spaces before `, . ? ! : ;` when the mark
/// ends a word (so `.env` or `:root` survive), and trims line ends.
///
/// Words that look like code, paths, URLs or e-mail addresses are never
/// re-cased, nor is anything after an ellipsis or a known abbreviation.
/// Loquara does not know what is already in the target field, so a take that
/// continues someone's sentence is still capitalised: the common case is a
/// fresh sentence.
pub fn normalize_transcript(text: &str) -> String {
    capitalize_sentences(&normalize_spacing(text))
}

fn normalize_spacing(text: &str) -> String {
    text.split('\n')
        .map(normalize_line_spacing)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn normalize_line_spacing(line: &str) -> String {
    let line = line.trim_end_matches([' ', '\t', '\r']);
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let (indent, body) = line.split_at(indent_len);
    let chars: Vec<char> = body.chars().collect();
    let mut result = String::with_capacity(line.len());
    result.push_str(indent);
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if character != ' ' && character != '\t' {
            result.push(character);
            index += 1;
            continue;
        }
        let mut next = index;
        while next < chars.len() && (chars[next] == ' ' || chars[next] == '\t') {
            next += 1;
        }
        // A mark that closes the word before it ("tak ,") loses the space; one
        // that starts a token of its own (".env", ":root") keeps it.
        let closes_word = chars.get(next).is_some_and(|mark| matches!(mark, ',' | '.' | '?' | '!' | ':' | ';'))
            && chars.get(next + 1).is_none_or(|after| !after.is_alphanumeric() && !matches!(after, '/' | '\\' | '_'));
        if !closes_word {
            result.push(' ');
        }
        index = next;
    }
    result
}

fn capitalize_sentences(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    let mut sentence_start = true;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if character.is_whitespace() || matches!(character, '"' | '\'' | '„' | '“' | '«' | '»' | '(' | '[' | '•' | '–' | '—' | '-') && sentence_start {
            result.push(character);
            index += 1;
            continue;
        }
        let end = chars[index..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(chars.len(), |offset| index + offset);
        let word: String = chars[index..end].iter().collect();
        if sentence_start {
            result.push_str(&capitalize_word(&word));
        } else {
            result.push_str(&word);
        }
        sentence_start = ends_sentence(&word);
        index = end;
    }
    result
}

/// Whether the text after `word` (and the whitespace that follows it) starts a
/// new sentence.
fn ends_sentence(word: &str) -> bool {
    let trimmed = word.trim_end_matches(['"', '\'', '”', '»', ')', ']']);
    if trimmed.ends_with("...") || trimmed.ends_with('…') {
        return false;
    }
    if trimmed.ends_with('?') || trimmed.ends_with('!') {
        return true;
    }
    let Some(stem) = trimmed.strip_suffix('.') else { return false };
    let stem = stem.trim_start_matches(['"', '\'', '„', '“', '«', '(', '[']).to_lowercase();
    !NON_TERMINAL_ABBREVIATIONS.contains(&stem.as_str())
}

/// Upper-cases the first letter of an ordinary word; leaves anything that
/// looks like code, a path, a URL, an address or a mixed-case name alone.
fn capitalize_word(word: &str) -> String {
    let core = word.trim_end_matches(|c: char| matches!(c, ',' | '.' | '?' | '!' | ':' | ';' | '"' | '\'' | '”' | '»' | ')' | ']'));
    let looks_like_code = core.chars().any(|c| {
        c.is_ascii_digit()
            || matches!(c, '@' | '/' | '\\' | '_' | '`' | '{' | '}' | '[' | ']' | '<' | '>' | '=' | '#' | '$' | '%' | '^' | '&' | '*' | '|' | '~' | '(' | ')' | '+')
    }) || core.contains(['.', ':'])
        || core.chars().skip(1).any(char::is_uppercase);
    let mut characters = word.chars();
    let Some(first) = characters.next() else { return String::new() };
    if looks_like_code || !first.is_lowercase() {
        return word.to_owned();
    }
    let mut upper = first.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) => std::iter::once(single).chain(characters).collect(),
        _ => word.to_owned(),
    }
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
    fn stored_cloud_api_keys_are_dropped_on_open() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("loquara.sqlite3");
        let storage = Storage::open(&path, temp.path().join("recordings")).unwrap();
        storage.set_setting("cloud_keys", &serde_json::json!({ "xai": "00ff" })).unwrap();
        storage.set_setting("kept", &1).unwrap();
        drop(storage);

        let storage = Storage::open(&path, temp.path().join("recordings")).unwrap();
        assert!(storage.get_setting::<serde_json::Value>("cloud_keys").unwrap().is_none());
        assert_eq!(storage.get_setting::<i32>("kept").unwrap(), Some(1));
    }

    #[test]
    fn migrates_all_tables_and_records_schema_version() {
        let (_temp, storage) = storage();

        assert_eq!(storage.schema_version().unwrap(), 4);
        assert_eq!(storage.table_names().unwrap(), vec!["history", "settings", "vocabulary"]);
    }

    #[test]
    fn upgrading_drops_modes_and_turns_replacements_into_terms_with_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("mow.sqlite3");
        let recordings = temp.path().join("recordings");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE vocabulary (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   heard TEXT NOT NULL COLLATE NOCASE UNIQUE,
                   replacement TEXT NOT NULL
                 );
                 INSERT INTO vocabulary(heard,replacement) VALUES
                   ('parakit','Parakeet'),('parakyt','Parakeet'),('loquara','Loquara'),('  ',' ');
                 CREATE TABLE modes (id TEXT PRIMARY KEY, config_json TEXT NOT NULL, name TEXT NOT NULL DEFAULT '');
                 INSERT INTO modes(id,config_json,name) VALUES ('custom','{}','Mine');
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value_json TEXT NOT NULL);
                 INSERT INTO settings VALUES ('app','{\"activeMode\":\"custom\"}');
                 CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version(version) VALUES (3);",
            )
            .unwrap();
        drop(connection);

        let storage = Storage::open(&database, &recordings).unwrap();

        assert_eq!(storage.schema_version().unwrap(), 4);
        assert_eq!(storage.table_names().unwrap(), vec!["history", "settings", "vocabulary"]);
        let terms = storage.list_vocabulary().unwrap();
        assert_eq!(
            terms.iter().map(|entry| (entry.term.as_str(), entry.aliases.clone())).collect::<Vec<_>>(),
            vec![("Parakeet", vec!["parakit".to_owned(), "parakyt".to_owned()]), ("Loquara", vec![])]
        );
        assert_eq!(storage.get_setting::<serde_json::Value>("app").unwrap().unwrap()["activeMode"], "custom");
        drop(storage);
        // Reopening a migrated database changes nothing.
        let reopened = Storage::open(&database, &recordings).unwrap();
        assert_eq!(reopened.list_vocabulary().unwrap(), terms);
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
        assert_eq!(storage.schema_version().unwrap(), 4);
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
    fn completing_without_a_duration_keeps_the_capture_length() {
        let (_temp, storage) = storage();
        let mut row = recording("cap", RecordingStatus::Processing, None);
        row.duration_ms = 409_000;
        storage.insert_recording(&row).unwrap();

        storage
            .update_status(
                "cap",
                RecordingStatus::Completed,
                Some("tekst"),
                Some("parakeet"),
                None,
                None,
            )
            .unwrap();

        let saved = storage.get_recording("cap").unwrap().unwrap();
        assert_eq!(saved.duration_ms, 409_000);
        assert_eq!(saved.text.as_deref(), Some("tekst"));
    }

    #[test]
    fn clear_audio_path_forgets_a_dangling_file_reference() {
        let (_temp, storage) = storage();
        storage
            .insert_recording(&recording("gone", RecordingStatus::Failed, None))
            .unwrap();

        assert!(storage.clear_audio_path("gone").unwrap());
        assert!(storage.get_recording("gone").unwrap().unwrap().audio_path.is_none());
    }

    #[test]
    fn reopening_clears_audio_path_when_the_file_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("mow.sqlite3");
        let recordings = temp.path().join("recordings");
        fs::create_dir_all(&recordings).unwrap();
        let storage = Storage::open(&database, &recordings).unwrap();
        let missing = recordings.join("gone.wav");
        let mut row = recording("gone", RecordingStatus::Failed, None);
        row.audio_path = Some(missing.to_string_lossy().into_owned());
        row.error = Some("audio input buffer overflowed".into());
        storage.insert_recording(&row).unwrap();
        let kept = recordings.join("kept.wav");
        fs::write(&kept, b"wav").unwrap();
        let mut kept_row = recording("kept", RecordingStatus::Failed, None);
        kept_row.audio_path = Some(kept.to_string_lossy().into_owned());
        storage.insert_recording(&kept_row).unwrap();
        drop(storage);

        let reopened = Storage::open(&database, &recordings).unwrap();

        let gone = reopened.get_recording("gone").unwrap().unwrap();
        assert!(gone.audio_path.is_none());
        assert_eq!(gone.error.as_deref(), Some("audio input buffer overflowed"));
        let kept_saved = reopened.get_recording("kept").unwrap().unwrap();
        assert_eq!(
            kept_saved.audio_path.as_deref(),
            Some(kept.to_string_lossy().as_ref())
        );
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
        let entry = storage.add_vocabulary(" Żółw™ ", &["żółw".to_owned()]).unwrap();

        assert_eq!(
            storage.get_setting::<bool>("autopaste").unwrap(),
            Some(true)
        );
        assert_eq!(
            storage.get_setting::<Retention>("retention").unwrap(),
            Some(Retention::Days(7))
        );
        assert_eq!(entry.term, "Żółw™");
        assert_eq!(storage.list_vocabulary().unwrap(), vec![entry.clone()]);
        storage.delete_vocabulary(entry.id).unwrap();
        assert!(storage.list_vocabulary().unwrap().is_empty());
    }

    #[test]
    fn adding_a_known_term_merges_its_aliases() {
        let (_temp, storage) = storage();
        let first = storage.add_vocabulary("Parakeet", &["parakit".to_owned()]).unwrap();
        let merged = storage
            .add_vocabulary("parakeet", &["PARAKIT".to_owned(), " parkit ".to_owned(), "Parakeet".to_owned(), String::new()])
            .unwrap();

        assert_eq!(merged.id, first.id);
        assert_eq!(merged.term, "Parakeet");
        assert_eq!(merged.aliases, vec!["parakit", "parkit"]);
        assert_eq!(storage.list_vocabulary().unwrap(), vec![merged]);
    }

    #[test]
    fn everyday_words_come_from_several_transcripts() {
        let (_temp, storage) = storage();
        for (index, text) in ["tragedia jest", "to tragedia", "tragedia, no", "raz"].into_iter().enumerate() {
            storage.insert_recording(&recording(&index.to_string(), RecordingStatus::Completed, Some(text))).unwrap();
        }
        let words = storage.everyday_words();
        assert!(words.contains("tragedia"));
        assert!(!words.contains("raz"));
    }

    #[test]
    fn every_transcript_gets_cleanup_vocabulary_and_sentence_case() {
        let entries = vec![
            VocabularyEntry { id: 1, term: "żółw".into(), aliases: vec!["zolw".into()] },
            VocabularyEntry { id: 2, term: "Nowy Sącz".into(), aliases: vec![] },
            VocabularyEntry { id: 3, term: "Parakeet".into(), aliases: vec![] },
        ];
        let vocabulary = VocabularyEntry::matcher(&entries);

        assert_eq!(
            finish_transcript("  ZOLW   jest w nowy SĄCZ  , naprawdę ?  ", &vocabulary),
            "Żółw jest w Nowy Sącz, naprawdę?"
        );
        assert_eq!(finish_transcript("przyzolwowy", &vocabulary), "Przyzolwowy");
        assert_eq!(
            finish_transcript("pierwszy   parkit ;\nciąg\n\nDrugi akapit !", &vocabulary),
            "Pierwszy Parakeet; ciąg\n\nDrugi akapit!"
        );
        assert_eq!(finish_transcript("otwórz plik .env teraz", &vocabulary), "Otwórz plik .env teraz");
        assert_eq!(finish_transcript("let x = 1;\nfoo()", &vocabulary), "Let x = 1; foo()");
    }

    #[test]
    fn delivered_transcripts_start_sentences_with_capitals() {
        // The real take: Parakeet heard the opening "A" as a continuation.
        assert_eq!(
            normalize_transcript("a próbowałeś coś zrobić z jakością dźwięku? no i czy jak powiem coś."),
            "A próbowałeś coś zrobić z jakością dźwięku? No i czy jak powiem coś."
        );
        assert_eq!(
            normalize_transcript("żółw idzie.   źle  jest ! ćma leci ; łódź płynie , śnieg pada."),
            "Żółw idzie. Źle jest! Ćma leci; łódź płynie, śnieg pada."
        );
        assert_eq!(normalize_transcript("„dobra, to tyle."), "„Dobra, to tyle.");
        assert_eq!(normalize_transcript("już.\n\nłatwo"), "Już.\n\nŁatwo");
    }

    #[test]
    fn normalizer_leaves_hesitations_abbreviations_and_code_alone() {
        assert_eq!(
            normalize_transcript("chyba, że... do czegokolwiek. np. coś, m.in. to, tzn. tak."),
            "Chyba, że... do czegokolwiek. Np. coś, m.in. to, tzn. tak."
        );
        assert_eq!(
            normalize_transcript("wyślij na jan@example.com. www.loquara.io działa. useState nie. src/main.rs też. npm"),
            "Wyślij na jan@example.com. www.loquara.io działa. useState nie. src/main.rs też. Npm"
        );
        assert_eq!(normalize_transcript("otwórz plik .env i :root"), "Otwórz plik .env i :root");
        assert_eq!(normalize_transcript("wersja 4.0 i 13.52 , ok"), "Wersja 4.0 i 13.52, ok");
        assert_eq!(normalize_transcript("Już Jest Dobrze."), "Już Jest Dobrze.");
        assert_eq!(normalize_transcript("  "), "");
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
    fn retention_skips_an_undeletable_file_and_keeps_cleaning_the_rest() {
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

        let report = storage
            .cleanup_retention_with(Retention::Days(1), 172_800_000, |path| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    return Err(std::io::Error::other("forced second delete failure"));
                }
                fs::remove_file(path)
            })
            .unwrap();

        assert_eq!(report, CleanupReport { deleted_audio: 2, skipped_audio: 1 });
        assert!(!paths[0].exists());
        assert!(paths[1].is_file());
        assert!(!paths[2].exists(), "a later file is still cleaned after one failure");
        for id in ["first", "third"] {
            assert!(storage.get_recording(id).unwrap().unwrap().audio_path.is_none());
        }
        assert_eq!(
            storage.get_recording("second").unwrap().unwrap().audio_path.as_deref(),
            Some(paths[1].to_string_lossy().as_ref())
        );

        assert_eq!(
            storage
                .cleanup_retention(Retention::Days(1), 172_800_000)
                .unwrap()
                .deleted_audio,
            1
        );
        assert!(!paths[1].exists());
    }
}
