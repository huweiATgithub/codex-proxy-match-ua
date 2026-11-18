use std::{
    collections::HashSet,
    path::Path,
    time::{Duration, Instant},
};

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{data::AccountFactsQuery, host::KeyListRequest},
    client::HostClient,
};
use rusqlite::{Connection, params};
use tokio::sync::Mutex;

use super::{RETENTION_SECONDS, now_seconds, open_database};

#[derive(Clone, Copy)]
enum Directory {
    ClientKeys,
    Accounts,
}

impl Directory {
    fn kind(self) -> &'static str {
        match self {
            Self::ClientKeys => "key",
            Self::Accounts => "account",
        }
    }

    async fn fetch(self, host: &HostClient) -> Result<Vec<(String, String)>, PluginFault> {
        let mut names = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let next = match self {
                Self::ClientKeys => {
                    let page = host
                        .list_keys(KeyListRequest { cursor, limit: 100 })
                        .await?;
                    names.extend(page.keys.into_iter().map(|key| (key.id, key.name)));
                    page.next_cursor
                }
                Self::Accounts => {
                    let page = host
                        .account_facts(AccountFactsQuery {
                            provider_id: Some("openai".into()),
                            cursor,
                            limit: 100,
                        })
                        .await?;
                    names.extend(
                        page.accounts
                            .into_iter()
                            .map(|account| (account.account_id, account.name)),
                    );
                    page.next_cursor
                }
            };
            let Some(next) = next else { return Ok(names) };
            if !seen.insert(next.clone()) {
                return Err(PluginFault::new(
                    ErrorCode::Fault,
                    "Host name directory repeated a cursor",
                ));
            }
            cursor = Some(next);
        }
    }
}

#[derive(Default)]
pub struct NameRefresh {
    last_success: Mutex<Option<Instant>>,
}

impl NameRefresh {
    pub async fn refresh(&self, host: &HostClient, path: &Path) -> Result<(), PluginFault> {
        let Ok(mut last) = self.last_success.try_lock() else {
            return Ok(());
        };
        if last.is_some_and(|time| time.elapsed() < Duration::from_secs(60)) {
            return Ok(());
        }
        let keys = Directory::ClientKeys.fetch(host).await?;
        let accounts = Directory::Accounts.fetch(host).await?;
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            save(&mut open_database(&path)?, &keys, &accounts, now_seconds())
        })
        .await
        .map_err(|_| PluginFault::new(ErrorCode::Fault, "UA name cache worker failed"))?
        .map_err(|error| {
            eprintln!("Could not update UA diagnostic names: {error}");
            PluginFault::new(ErrorCode::Fault, "UA name cache update failed")
        })?;
        *last = Some(Instant::now());
        Ok(())
    }
}

fn save(
    db: &mut Connection,
    keys: &[(String, String)],
    accounts: &[(String, String)],
    now: i64,
) -> rusqlite::Result<()> {
    let tx = db.transaction()?;
    {
        let mut update = tx.prepare(
            "INSERT INTO rejection_names (kind, id, name, last_seen) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (kind, id) DO UPDATE SET name = excluded.name, last_seen = excluded.last_seen",
        )?;
        for (directory, entries) in [
            (Directory::ClientKeys, keys),
            (Directory::Accounts, accounts),
        ] {
            for (id, name) in entries {
                update.execute(params![directory.kind(), id, name.trim(), now])?;
            }
        }
    }
    // Keep a deleted entity's last known name while a retained rejection still references it.
    tx.execute(
        "DELETE FROM rejection_names WHERE last_seen < ?1 AND (kind, id) NOT IN (
            SELECT 'key', client_key_id FROM rejected_requests
            UNION ALL SELECT 'account', account_id FROM rejected_requests
        )",
        [now - RETENTION_SECONDS],
    )?;
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{insert_batch, tests::sample};

    #[test]
    fn names_survive_reopening_renames_and_deletion_until_the_last_rejection_expires() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rejections.sqlite3");
        let id = "same-id".to_owned();
        let mut db = open_database(&path).unwrap();
        save(
            &mut db,
            &[
                (id.clone(), "Old key".into()),
                ("unused".into(), "Unused".into()),
            ],
            &[(id.clone(), "Upstream".into())],
            1,
        )
        .unwrap();
        drop(db);

        let mut db = open_database(&path).unwrap();
        let now = RETENTION_SECONDS + 2;
        let mut record = sample(now);
        record.key = id.clone();
        record.account = id.clone();
        insert_batch(&mut db, &[record]).unwrap();
        save(&mut db, &[(id.clone(), "Renamed key".into())], &[], now).unwrap();
        let names = |db: &Connection| {
            db.prepare("SELECT name FROM rejection_names ORDER BY kind, id")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(names(&db), ["Upstream", "Renamed key"]);
        db.execute("DELETE FROM rejected_requests", []).unwrap();
        save(&mut db, &[], &[], now + 1).unwrap();
        assert_eq!(names(&db), ["Renamed key"]);
    }
}
