use std::{
    ffi::OsStr,
    fs, io,
    path::{Component, Path, PathBuf},
    sync::mpsc::{Receiver, SyncSender, sync_channel},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::middleware::outbound::{Metadata, Target},
};
use rusqlite::{Connection, params};

use crate::platform::{Platform, Rejection, UserAgent};

const QUEUE_CAPACITY: usize = 256;
const WRITE_BATCH: usize = 32;
const RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;
const DELETE_BATCH: usize = 1024;
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_millis(200);
const MAX_UA_BYTES: usize = 1024;

pub struct Record {
    at: i64,
    request_id: String,
    key: String,
    account: String,
    attempt: u32,
    model: Option<String>,
    transport: &'static str,
    client_ua: Option<String>,
    effective_ua: Option<String>,
    client_count: usize,
    effective_count: usize,
    rejection: Rejection,
}

impl Record {
    pub fn new(
        meta: &Metadata,
        target: &Target,
        client: &UserAgent<'_>,
        effective: &UserAgent<'_>,
        rejection: Rejection,
    ) -> Self {
        let sample = |ua: &UserAgent<'_>| {
            ua.first.map(|bytes| {
                String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_UA_BYTES)]).into_owned()
            })
        };
        Self {
            at: now_seconds(),
            request_id: meta.request_id.clone(),
            key: meta.client_key_id.clone(),
            account: meta.account_id.clone(),
            attempt: meta.attempt_index,
            model: meta.model.clone(),
            transport: match target {
                Target::Http { .. } => "http",
                Target::WebSocket { .. } => "websocket",
            },
            client_ua: sample(client),
            effective_ua: sample(effective),
            client_count: client.count,
            effective_count: effective.count,
            rejection,
        }
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub enum Diagnostics {
    Active {
        sender: SyncSender<Record>,
        path: PathBuf,
    },
    Unavailable,
}

impl Diagnostics {
    pub fn start(directory: &Path, instance_id: &str) -> Self {
        let started = database_path(directory, instance_id).and_then(|path| {
            let (sender, receiver) = sync_channel(QUEUE_CAPACITY);
            let worker_path = path.clone();
            thread::Builder::new()
                .name("ua-diagnostics".into())
                .spawn(move || write_records(&worker_path, receiver))?;
            Ok(Self::Active { sender, path })
        });
        started.unwrap_or_else(|error| {
            eprintln!("UA diagnostics unavailable: {error}");
            Self::Unavailable
        })
    }

    pub fn submit(&self, record: Record) {
        if let Self::Active { sender, .. } = self {
            // Full queues drop this sample. Admission and its error message already have the decision.
            let _ = sender.try_send(record);
        }
    }

    pub fn database_path(&self) -> Option<&Path> {
        match self {
            Self::Active { path, .. } => Some(path),
            Self::Unavailable => None,
        }
    }

    pub async fn prune(&self, budget: Option<Duration>) -> Result<(), PluginFault> {
        let Self::Active { path, .. } = self else {
            return Err(PluginFault::new(
                ErrorCode::Fault,
                "UA diagnostics storage is unavailable",
            ));
        };
        let available = budget.unwrap_or_default().min(Duration::from_secs(2));
        if available <= BUSY_TIMEOUT + Duration::from_millis(50) {
            return Ok(());
        }
        let deadline = Instant::now() + available - BUSY_TIMEOUT;
        let path = path.clone();
        tokio::task::spawn_blocking(move || -> rusqlite::Result<()> {
            let connection = open_database(&path)?;
            let cutoff = now_seconds() - RETENTION_SECONDS;
            while Instant::now() < deadline {
                if delete_batch(&connection, cutoff)? < DELETE_BATCH {
                    break;
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| {
            PluginFault::new(ErrorCode::Fault, "UA diagnostics maintenance worker failed")
        })?
        .map_err(|error| {
            eprintln!("UA diagnostics retention failed: {error}");
            PluginFault::new(ErrorCode::Fault, "UA diagnostics retention failed")
        })
    }
}

fn database_path(directory: &Path, instance_id: &str) -> io::Result<PathBuf> {
    if !matches!(Path::new(instance_id).components().next(), Some(Component::Normal(name)) if name == OsStr::new(instance_id))
    {
        return Err(io::Error::other("invalid plugin instance ID"));
    }
    let data = directory
        .parent()
        .filter(|cache| cache.file_name() == Some(OsStr::new("plugins")))
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("unknown host plugin directory"))?;
    Ok(data
        .join("huwei.match-ua")
        .join(instance_id)
        .join("rejections.sqlite3"))
}

fn open_database(path: &Path) -> rusqlite::Result<Connection> {
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory
        .create(path.parent().expect("plugin database has a parent"))
        .map_err(|_| rusqlite::Error::InvalidPath(path.to_owned()))?;
    let db = Connection::open(path)?;
    db.busy_timeout(BUSY_TIMEOUT)?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        CREATE TABLE IF NOT EXISTS rejected_requests (
            id INTEGER PRIMARY KEY, rejected_at INTEGER NOT NULL, request_id TEXT NOT NULL,
            client_key_id TEXT NOT NULL, account_id TEXT NOT NULL, attempt_index INTEGER NOT NULL,
            model TEXT, transport TEXT NOT NULL, reason TEXT NOT NULL, message TEXT NOT NULL,
            client_platform TEXT, effective_platform TEXT, client_ua TEXT, effective_ua TEXT,
            client_ua_count INTEGER NOT NULL, effective_ua_count INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS rejection_time ON rejected_requests(rejected_at);
        CREATE INDEX IF NOT EXISTS rejection_request_id ON rejected_requests(request_id, rejected_at);",
    )?;
    Ok(db)
}

fn delete_batch(db: &Connection, cutoff: i64) -> rusqlite::Result<usize> {
    db.execute(
        "DELETE FROM rejected_requests WHERE id IN (
        SELECT id FROM rejected_requests WHERE rejected_at < ?1 ORDER BY rejected_at LIMIT ?2
    )",
        params![cutoff, DELETE_BATCH as i64],
    )
}

fn write_records(path: &Path, receiver: Receiver<Record>) {
    let mut database = None;
    let mut reported_failure = false;
    while let Ok(first) = receiver.recv() {
        let mut batch = vec![first];
        batch.extend(receiver.try_iter().take(WRITE_BATCH - 1));
        let write = (|| {
            let db = match &mut database {
                Some(db) => db,
                None => database.insert(open_database(path)?),
            };
            insert_batch(db, &batch)
        })();
        if let Err(error) = write {
            database = None;
            if !reported_failure {
                eprintln!("UA diagnostics write failed; records dropped: {error}");
            }
            reported_failure = true;
        } else {
            reported_failure = false;
        }
    }
}

fn insert_batch(db: &mut Connection, batch: &[Record]) -> rusqlite::Result<()> {
    let tx = db.transaction()?;
    {
        let mut insert = tx.prepare_cached(
            "INSERT INTO rejected_requests (
            rejected_at, request_id, client_key_id, account_id, attempt_index, model, transport,
            reason, message, client_platform, effective_platform, client_ua, effective_ua,
            client_ua_count, effective_ua_count
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        )?;
        for record in batch {
            let reason = record.rejection;
            insert.execute(params![
                record.at,
                record.request_id,
                record.key,
                record.account,
                record.attempt,
                record.model,
                record.transport,
                reason.reason(),
                reason.to_string(),
                reason.client.ok().map(Platform::name),
                reason.effective.ok().map(Platform::name),
                record.client_ua,
                record.effective_ua,
                record.client_count as i64,
                record.effective_count as i64
            ])?;
        }
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::UaIssue;

    fn sample(at: i64) -> Record {
        Record {
            at,
            request_id: "request".into(),
            key: "key".into(),
            account: "account".into(),
            attempt: 1,
            model: None,
            transport: "http",
            client_ua: None,
            effective_ua: None,
            client_count: 0,
            effective_count: 1,
            rejection: Rejection {
                client: Err(UaIssue::Missing),
                effective: Ok(Platform::Linux),
            },
        }
    }

    #[test]
    fn expiry_is_strict_and_deletes_in_bounded_batches() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_database(&dir.path().join("db")).unwrap();
        let cutoff = 1000;
        let mut records: Vec<_> = (0..=DELETE_BATCH).map(|_| sample(cutoff - 1)).collect();
        records.extend([sample(cutoff), sample(cutoff + 1)]);
        insert_batch(&mut db, &records).unwrap();
        assert_eq!(delete_batch(&db, cutoff).unwrap(), DELETE_BATCH);
        assert_eq!(delete_batch(&db, cutoff).unwrap(), 1);
        assert_eq!(
            db.query_row("SELECT count(*) FROM rejected_requests", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn a_full_queue_drops_the_new_record_without_a_database_or_worker() {
        let (sender, receiver) = sync_channel(1);
        let diagnostics = Diagnostics::Active {
            sender,
            path: "unused".into(),
        };
        diagnostics.submit(sample(1));
        diagnostics.submit(sample(2));
        assert_eq!(receiver.try_recv().unwrap().at, 1);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn package_reextraction_keeps_the_same_instance_database() {
        let first = database_path(Path::new("/data/plugins/plugin-A"), "instance").unwrap();
        assert_eq!(
            first,
            database_path(Path::new("/data/plugins/plugin-B"), "instance").unwrap()
        );
        assert_ne!(
            first,
            database_path(Path::new("/data/plugins/plugin-A"), "other").unwrap()
        );
        assert!(database_path(Path::new("/data/plugins/plugin-A"), "../other").is_err());
    }
}
