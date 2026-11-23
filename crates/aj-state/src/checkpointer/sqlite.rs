//! A durable checkpointer on SQLite.
//!
//! One database file (or an in-memory database) holds every thread. The on-disk
//! layout is versioned through `PRAGMA user_version`; opening a database written
//! by a newer layout fails rather than guessing. Checkpoint bodies are stored as
//! JSON next to indexed columns (thread, id, parent, sequence, step, schema,
//! pending-task count) that serve listing and head lookups without parsing
//! bodies. Every body read back is re-verified against its content-derived id, so
//! a damaged or edited row is reported as such instead of being resumed.
//!
//! Writes run in `IMMEDIATE` transactions, so concurrent writers — threads or
//! processes — are serialized and the head/step rules in
//! [`check_put`](super::check_put) hold across all of them. The file is opened in
//! WAL mode with `synchronous = FULL`: a `put` that returned is on disk.

use std::{
    fmt::Display,
    path::Path,
    sync::{Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    CheckpointSummary, Checkpointer, HeadInfo, PutDecision, StoreError, check_put,
    check_task_writes_target,
};
use crate::{
    checkpoint::{Checkpoint, CheckpointId},
    digest::Digest,
    ids::{SchemaName, ThreadId},
    schema::{SchemaRef, SchemaVersion},
    update::TaskWrites,
};

const LAYOUT_VERSION: i64 = 1;

/// How long a connection waits for another writer before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

const LAYOUT: &str = "
CREATE TABLE IF NOT EXISTS ajentir_checkpoints (
    thread_id          TEXT    NOT NULL,
    checkpoint_id      TEXT    NOT NULL,
    parent_id          TEXT,
    seq                INTEGER NOT NULL,
    step               INTEGER NOT NULL,
    schema_name        TEXT    NOT NULL,
    schema_version     INTEGER NOT NULL,
    schema_fingerprint TEXT    NOT NULL,
    pending_tasks      INTEGER NOT NULL,
    body               TEXT    NOT NULL,
    PRIMARY KEY (thread_id, checkpoint_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS ajentir_checkpoints_by_seq
    ON ajentir_checkpoints (thread_id, seq);
CREATE TABLE IF NOT EXISTS ajentir_task_writes (
    thread_id     TEXT NOT NULL,
    checkpoint_id TEXT NOT NULL,
    task_id       TEXT NOT NULL,
    body          TEXT NOT NULL,
    PRIMARY KEY (thread_id, checkpoint_id, task_id),
    FOREIGN KEY (thread_id, checkpoint_id)
        REFERENCES ajentir_checkpoints (thread_id, checkpoint_id) ON DELETE CASCADE
);
";

/// Stores checkpoints in a SQLite database.
#[derive(Debug)]
pub struct SqliteCheckpointer {
    connection: Mutex<Connection>,
}

impl SqliteCheckpointer {
    /// Opens (creating if necessary) the database at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Storage`] if the file cannot be opened or its layout
    /// version is not one this crate understands.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let connection = Connection::open(path).map_err(storage)?;
        // The timeout must come first: switching the journal mode takes a lock, and
        // without a timeout a second process opening the same new file at the same
        // moment would fail at once instead of waiting its turn.
        connection.busy_timeout(BUSY_TIMEOUT).map_err(storage)?;
        retry_while_busy(|| {
            connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")
        })
        .map_err(storage)?;
        Self::prepare(connection)
    }

    /// Opens a private in-memory database. Durable only for the life of the value;
    /// useful for tests of SQLite-specific behaviour.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Storage`] if SQLite cannot create the database.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory().map_err(storage)?;
        connection.busy_timeout(BUSY_TIMEOUT).map_err(storage)?;
        Self::prepare(connection)
    }

    /// Checks the layout version and creates the tables on a fresh database. The
    /// connection must already have its busy timeout set.
    fn prepare(connection: Connection) -> Result<Self, StoreError> {
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(storage)?;
        let version: i64 = retry_while_busy(|| {
            connection.pragma_query_value(None, "user_version", |row| row.get(0))
        })
        .map_err(storage)?;
        match version {
            0 => retry_while_busy(|| {
                connection
                    .execute_batch(&format!(
                        "BEGIN IMMEDIATE; {LAYOUT} PRAGMA user_version = {LAYOUT_VERSION}; COMMIT;"
                    ))
                    .inspect_err(|_| {
                        // A failure after BEGIN would leave the transaction open and
                        // make the retry fail; with no transaction this is a no-op.
                        let _ = connection.execute_batch("ROLLBACK;");
                    })
            })
            .map_err(storage)?,
            LAYOUT_VERSION => {}
            other => {
                return Err(StoreError::Storage(format!(
                    "unsupported database layout version {other}; this build understands {LAYOUT_VERSION}"
                )));
            }
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::Storage("sqlite checkpointer lock poisoned".to_owned()))
    }
}

/// Whether SQLite refused because another connection holds a conflicting lock.
fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// Runs `operation`, repeating it while SQLite reports the database as busy, for at
/// most [`BUSY_TIMEOUT`].
///
/// The connection's own busy timeout is not enough during initialisation: SQLite
/// returns `SQLITE_BUSY` immediately, without consulting the busy handler, when
/// waiting could deadlock — for example when several connections race to switch a
/// brand-new file to WAL mode.
fn retry_while_busy<T>(mut operation: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        match operation() {
            Err(error) if is_busy(&error) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            outcome => return outcome,
        }
    }
}

fn storage(error: impl Display) -> StoreError {
    StoreError::Storage(error.to_string())
}

fn corrupt(reason: impl Display) -> StoreError {
    StoreError::Corrupt(reason.to_string())
}

fn to_i64(value: u64, what: &str) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_| storage(format!("{what} {value} does not fit SQLite's integer")))
}

fn to_u64(value: i64, what: &str) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| corrupt(format!("negative {what} {value}")))
}

fn parse_id(text: &str) -> Result<CheckpointId, StoreError> {
    CheckpointId::from_hex(text).map_err(|error| corrupt(format!("stored checkpoint id: {error}")))
}

struct HeadRow {
    info: HeadInfo,
    seq: i64,
}

fn head_row(connection: &Connection, thread: &ThreadId) -> Result<Option<HeadRow>, StoreError> {
    let raw = connection
        .query_row(
            "SELECT checkpoint_id, step, seq FROM ajentir_checkpoints
             WHERE thread_id = ?1 ORDER BY seq DESC LIMIT 1",
            params![thread.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    raw.map(|(id, step, seq)| {
        Ok(HeadRow {
            info: HeadInfo {
                id: parse_id(&id)?,
                step: to_u64(step, "step")?,
            },
            seq,
        })
    })
    .transpose()
}

fn decode_checkpoint(
    body: &str,
    thread: &ThreadId,
    id: &CheckpointId,
) -> Result<Checkpoint, StoreError> {
    let checkpoint: Checkpoint = serde_json::from_str(body)
        .map_err(|error| corrupt(format!("checkpoint {id} does not decode: {error}")))?;
    checkpoint.verify()?;
    if checkpoint.id() != *id || checkpoint.thread() != thread {
        return Err(corrupt(format!(
            "row for checkpoint {id} in thread `{thread}` holds a different checkpoint"
        )));
    }
    Ok(checkpoint)
}

fn load_checkpoint(
    connection: &Connection,
    thread: &ThreadId,
    id: &CheckpointId,
) -> Result<Option<Checkpoint>, StoreError> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM ajentir_checkpoints WHERE thread_id = ?1 AND checkpoint_id = ?2",
            params![thread.as_str(), id.to_hex()],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    body.map(|text| decode_checkpoint(&text, thread, id))
        .transpose()
}

struct SummaryRow {
    id: String,
    parent: Option<String>,
    step: i64,
    schema_name: String,
    schema_version: i64,
    schema_fingerprint: String,
    pending_tasks: i64,
}

impl SummaryRow {
    fn into_summary(self) -> Result<CheckpointSummary, StoreError> {
        let version = u32::try_from(self.schema_version)
            .map_err(|_| corrupt(format!("schema version {}", self.schema_version)))?;
        Ok(CheckpointSummary {
            id: parse_id(&self.id)?,
            parent: self.parent.as_deref().map(parse_id).transpose()?,
            step: to_u64(self.step, "step")?,
            schema: SchemaRef {
                name: SchemaName::new(self.schema_name).map_err(corrupt)?,
                version: SchemaVersion::new(version),
                fingerprint: Digest::from_hex(&self.schema_fingerprint).map_err(corrupt)?,
            },
            pending_tasks: usize::try_from(to_u64(self.pending_tasks, "pending task count")?)
                .map_err(corrupt)?,
        })
    }
}

impl Checkpointer for SqliteCheckpointer {
    fn put(&self, checkpoint: &Checkpoint) -> Result<(), StoreError> {
        checkpoint.verify()?;
        let body = serde_json::to_string(checkpoint).map_err(storage)?;
        let step = to_i64(checkpoint.step(), "step")?;
        let pending = to_i64(
            u64::try_from(checkpoint.frontier().len()).unwrap_or(u64::MAX),
            "pending task count",
        )?;

        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let head = head_row(&transaction, checkpoint.thread())?;
        if check_put(head.as_ref().map(|row| &row.info), checkpoint)? == PutDecision::AlreadyStored
        {
            return Ok(());
        }
        let seq = match &head {
            Some(row) => row
                .seq
                .checked_add(1)
                .ok_or_else(|| storage("thread sequence overflow"))?,
            None => 0,
        };
        transaction
            .execute(
                "INSERT INTO ajentir_checkpoints
                 (thread_id, checkpoint_id, parent_id, seq, step,
                  schema_name, schema_version, schema_fingerprint, pending_tasks, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    checkpoint.thread().as_str(),
                    checkpoint.id().to_hex(),
                    checkpoint.parent().map(|parent| parent.to_hex()),
                    seq,
                    step,
                    checkpoint.schema().name.as_str(),
                    i64::from(checkpoint.schema().version.get()),
                    checkpoint.schema().fingerprint.to_hex(),
                    pending,
                    body,
                ],
            )
            .map_err(storage)?;
        transaction.commit().map_err(storage)
    }

    fn get(&self, thread: &ThreadId, id: &CheckpointId) -> Result<Option<Checkpoint>, StoreError> {
        let connection = self.lock()?;
        load_checkpoint(&connection, thread, id)
    }

    fn head(&self, thread: &ThreadId) -> Result<Option<Checkpoint>, StoreError> {
        let connection = self.lock()?;
        match head_row(&connection, thread)? {
            Some(row) => load_checkpoint(&connection, thread, &row.info.id),
            None => Ok(None),
        }
    }

    fn history(&self, thread: &ThreadId) -> Result<Vec<CheckpointSummary>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT checkpoint_id, parent_id, step, schema_name, schema_version,
                        schema_fingerprint, pending_tasks
                 FROM ajentir_checkpoints WHERE thread_id = ?1 ORDER BY seq",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map(params![thread.as_str()], |row| {
                Ok(SummaryRow {
                    id: row.get(0)?,
                    parent: row.get(1)?,
                    step: row.get(2)?,
                    schema_name: row.get(3)?,
                    schema_version: row.get(4)?,
                    schema_fingerprint: row.get(5)?,
                    pending_tasks: row.get(6)?,
                })
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        rows.into_iter().map(SummaryRow::into_summary).collect()
    }

    fn threads(&self) -> Result<Vec<ThreadId>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT DISTINCT thread_id FROM ajentir_checkpoints ORDER BY thread_id")
            .map_err(storage)?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        names
            .into_iter()
            .map(|name| ThreadId::new(name).map_err(corrupt))
            .collect()
    }

    fn put_task_writes(
        &self,
        thread: &ThreadId,
        checkpoint: &CheckpointId,
        writes: &TaskWrites,
    ) -> Result<(), StoreError> {
        let body = serde_json::to_string(writes).map_err(storage)?;
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let head = match head_row(&transaction, thread)? {
            Some(row) => load_checkpoint(&transaction, thread, &row.info.id)?,
            None => None,
        };
        check_task_writes_target(head.as_ref(), thread, checkpoint, writes.task())?;

        let existing: Option<String> = transaction
            .query_row(
                "SELECT body FROM ajentir_task_writes
                 WHERE thread_id = ?1 AND checkpoint_id = ?2 AND task_id = ?3",
                params![thread.as_str(), checkpoint.to_hex(), writes.task().as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        if let Some(text) = existing {
            let stored: TaskWrites = serde_json::from_str(&text)
                .map_err(|error| corrupt(format!("stored task writes: {error}")))?;
            return if &stored == writes {
                Ok(())
            } else {
                Err(StoreError::TaskWritesConflict(writes.task().clone()))
            };
        }
        transaction
            .execute(
                "INSERT INTO ajentir_task_writes (thread_id, checkpoint_id, task_id, body)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    thread.as_str(),
                    checkpoint.to_hex(),
                    writes.task().as_str(),
                    body
                ],
            )
            .map_err(storage)?;
        transaction.commit().map_err(storage)
    }

    fn task_writes(
        &self,
        thread: &ThreadId,
        checkpoint: &CheckpointId,
    ) -> Result<Vec<TaskWrites>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT body FROM ajentir_task_writes
                 WHERE thread_id = ?1 AND checkpoint_id = ?2 ORDER BY task_id",
            )
            .map_err(storage)?;
        let bodies = statement
            .query_map(params![thread.as_str(), checkpoint.to_hex()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        bodies
            .iter()
            .map(|text| {
                serde_json::from_str(text)
                    .map_err(|error| corrupt(format!("stored task writes: {error}")))
            })
            .collect()
    }

    fn delete_thread(&self, thread: &ThreadId) -> Result<bool, StoreError> {
        let connection = self.lock()?;
        let removed = connection
            .execute(
                "DELETE FROM ajentir_checkpoints WHERE thread_id = ?1",
                params![thread.as_str()],
            )
            .map_err(storage)?;
        Ok(removed > 0)
    }
}
