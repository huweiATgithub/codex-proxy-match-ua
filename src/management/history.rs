use std::path::Path;

use rusqlite::{Connection, OpenFlags, Row, params};
use serde::Serialize;

const PAGE_SIZE: usize = 25;

struct Cursor {
    at: i64,
    id: i64,
}

impl Cursor {
    fn parse(value: &str) -> Option<Self> {
        let (time, id) = value.split_once('.')?;
        let at = time.parse().ok()?;
        let id = id.parse().ok()?;
        (at >= 0 && id > 0).then_some(Self { at, id })
    }
}

#[derive(Default)]
pub(super) struct Query {
    before: Option<Cursor>,
    request_id: Option<String>,
}

impl Query {
    pub(super) fn parse(raw: &str) -> Result<Self, &'static str> {
        let mut query = Self::default();
        for (key, value) in form_urlencoded::parse(raw.as_bytes()) {
            match key.as_ref() {
                "before" if query.before.is_none() => {
                    query.before = Some(Cursor::parse(&value).ok_or("Invalid page cursor.")?);
                }
                "requestId" if query.request_id.is_none() && !value.is_empty() => {
                    query.request_id = Some(value.into_owned());
                }
                _ => return Err("Invalid or repeated diagnostics query parameter."),
            }
        }
        Ok(query)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    #[serde(skip)]
    id: i64,
    rejected_at: i64,
    request_id: String,
    client_key_id: String,
    account_id: String,
    model: Option<String>,
    attempt_index: u32,
    transport: String,
    reason: String,
    message: String,
    client_platform: Option<String>,
    effective_platform: Option<String>,
    client_ua: Option<String>,
    effective_ua: Option<String>,
    client_ua_count: u32,
    effective_ua_count: u32,
}

impl Entry {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            rejected_at: row.get("rejected_at")?,
            request_id: row.get("request_id")?,
            client_key_id: row.get("client_key_id")?,
            account_id: row.get("account_id")?,
            model: row.get("model")?,
            attempt_index: row.get("attempt_index")?,
            transport: row.get("transport")?,
            reason: row.get("reason")?,
            message: row.get("message")?,
            client_platform: row.get("client_platform")?,
            effective_platform: row.get("effective_platform")?,
            client_ua: row.get("client_ua")?,
            effective_ua: row.get("effective_ua")?,
            client_ua_count: row.get("client_ua_count")?,
            effective_ua_count: row.get("effective_ua_count")?,
        })
    }
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Page {
    items: Vec<Entry>,
    next_cursor: Option<String>,
}

impl Page {
    pub(super) fn read(path: &Path, query: &Query) -> rusqlite::Result<Self> {
        let exists = path
            .try_exists()
            .map_err(|_| rusqlite::Error::InvalidPath(path.to_owned()))?;
        if !exists {
            return Ok(Self::default());
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(crate::diagnostics::BUSY_TIMEOUT)?;
        // The first writer may have opened the file but not yet committed its schema.
        if !db.table_exists(None, "rejected_requests")? {
            return Ok(Self::default());
        }
        let filter = if query.request_id.is_some() {
            "AND request_id = ?3"
        } else {
            ""
        };
        let sql = format!(
            "SELECT * FROM rejected_requests WHERE (rejected_at, id) < (?1, ?2) {filter}
             ORDER BY rejected_at DESC, id DESC LIMIT {}",
            PAGE_SIZE + 1
        );
        let first = Cursor {
            at: i64::MAX,
            id: i64::MAX,
        };
        let before = query.before.as_ref().unwrap_or(&first);
        let mut stmt = db.prepare(&sql)?;
        let mut rows = match &query.request_id {
            Some(id) => stmt.query(params![before.at, before.id, id])?,
            None => stmt.query(params![before.at, before.id])?,
        };
        let mut page = Self::default();
        while let Some(row) = rows.next()? {
            if page.items.len() == PAGE_SIZE {
                let last = page.items.last().expect("a full page has a last entry");
                page.next_cursor = Some(format!("{}.{}", last.rejected_at, last.id));
                break;
            }
            page.items.push(Entry::read(row)?);
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_instance_is_empty_without_creating_or_modifying_its_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rejections.sqlite3");
        assert!(
            Page::read(&path, &Query::default())
                .unwrap()
                .items
                .is_empty()
        );
        assert!(!path.exists());

        let initializing = Connection::open(&path).unwrap();
        assert!(
            Page::read(&path, &Query::default())
                .unwrap()
                .items
                .is_empty()
        );
        assert!(
            !initializing
                .table_exists(None, "rejected_requests")
                .unwrap()
        );
    }

    #[test]
    fn parses_opaque_request_ids_and_rejects_invalid_paging() {
        let query = Query::parse("requestId=req%2Fa%2Bb&before=12.3").unwrap();
        assert_eq!(query.request_id.as_deref(), Some("req/a+b"));
        let before = query.before.unwrap();
        assert_eq!((before.at, before.id), (12, 3));
        for bad in [
            "before=1",
            "before=9.0",
            "before=-2.3",
            "before=1.2&before=1.3",
            "requestId=a&requestId=b",
        ] {
            assert!(Query::parse(bad).is_err());
        }
    }
}
