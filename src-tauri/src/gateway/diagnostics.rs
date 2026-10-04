//! Opt-in, bounded communication capture. The proxy never waits for disk I/O.

use axum::body::{Body, Bytes, HttpBody};
use axum::http::HeaderMap;
use base64::Engine;
use futures_core::Stream;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::io::Write;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

const DEFAULT_STORAGE_LIMIT: i64 = 5 * 1024 * 1024 * 1024;
const MIN_STORAGE_LIMIT: i64 = 1024 * 1024;
const MAX_STORAGE_LIMIT: i64 = 9_007_199_254_740_991;
// These bound queue messages and IPC previews, never the retained body size.
const CHUNK_SIZE: usize = 256 * 1024;
const BODY_PREVIEW_LIMIT: usize = 1024 * 1024;
const TRACE_LIMIT: i64 = 1000;
const EVENT_LIMIT: i64 = 64;
static CLIENT: OnceLock<Arc<Client>> = OnceLock::new();
static INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static EVENT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, specta::Type)]
pub(crate) struct DiagnosticTrace {
    pub trace_id: String,
    pub cli_key: String,
    pub method: String,
    pub path: String,
    pub created_at_ms: i64,
    pub status: Option<u16>,
    pub capture_limited: bool,
}

#[derive(Debug, Clone, Serialize, specta::Type)]
pub(crate) struct DiagnosticEvent {
    pub id: String,
    pub phase: String,
    pub created_at_ms: i64,
    pub metadata: String,
    pub body: String,
    pub body_encoding: String,
    pub bytes_seen: i64,
    pub retained_bytes: i64,
    pub preview_truncated: bool,
    pub truncated: bool,
    pub complete: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, specta::Type)]
pub(crate) struct DiagnosticSnapshot {
    pub enabled: bool,
    pub retention_days: u32,
    pub storage_limit_bytes: i64,
    pub stored_bytes: i64,
    pub dropped_messages: u32,
    pub last_error: Option<String>,
    pub traces: Vec<DiagnosticTrace>,
}

struct Client {
    tx: mpsc::SyncSender<Message>,
    enabled: AtomicBool,
    epoch: AtomicU64,
    dropped: AtomicU64,
}

enum Message {
    #[cfg(test)]
    Stop,
    Begin(u64, DiagnosticTrace),
    Event(u64, String, String, &'static str, String, i64),
    Chunk(u64, String, Vec<u8>, i64),
    End(u64, String, i64, bool, Option<String>),
    Status(u64, String, u16),
    Read(Option<String>, mpsc::Sender<Result<ReadResult, String>>),
    Configure(bool, u32, i64, mpsc::Sender<Result<(), String>>),
    SaveBody(String, String, String, mpsc::Sender<Result<(), String>>),
    Clear(bool, mpsc::Sender<Result<(), String>>),
}

enum ReadResult {
    Snapshot(DiagnosticSnapshot),
    Events(Vec<DiagnosticEvent>),
}

struct Store {
    conn: Connection,
    enabled: bool,
    retention_days: u32,
    storage_limit_bytes: i64,
    epoch: u64,
    last_error: Option<String>,
}

impl Store {
    fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        conn.execute_batch(
            "PRAGMA auto_vacuum=INCREMENTAL;
             PRAGMA journal_mode=WAL;
             PRAGMA secure_delete=ON;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS config (id INTEGER PRIMARY KEY CHECK(id=1), enabled INTEGER NOT NULL, days INTEGER NOT NULL);
             INSERT OR IGNORE INTO config(id,enabled,days) VALUES(1, 0, 15);
             CREATE TABLE IF NOT EXISTS traces (trace_id TEXT PRIMARY KEY, cli_key TEXT NOT NULL, method TEXT NOT NULL, path TEXT NOT NULL, created_at_ms INTEGER NOT NULL, status INTEGER, epoch INTEGER NOT NULL, capture_limited INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX IF NOT EXISTS traces_created ON traces(created_at_ms);
             CREATE TABLE IF NOT EXISTS events (id TEXT PRIMARY KEY, trace_id TEXT NOT NULL REFERENCES traces(trace_id) ON DELETE CASCADE, phase TEXT NOT NULL, created_at_ms INTEGER NOT NULL, metadata TEXT NOT NULL, body BLOB NOT NULL DEFAULT X'', bytes_seen INTEGER NOT NULL DEFAULT 0, complete INTEGER NOT NULL DEFAULT 0, note TEXT);
             CREATE INDEX IF NOT EXISTS events_trace ON events(trace_id, created_at_ms);
             CREATE TABLE IF NOT EXISTS event_chunks (id INTEGER PRIMARY KEY, event_id TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE, data BLOB NOT NULL);
             CREATE INDEX IF NOT EXISTS event_chunks_event ON event_chunks(event_id,id);",
        ).map_err(|e| e.to_string())?;
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if version == 0 {
            // Migrate legacy 64 KiB bodies exactly once, without discarding data.
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE config ADD COLUMN storage_limit_bytes INTEGER NOT NULL DEFAULT 5368709120;
                 ALTER TABLE events ADD COLUMN retained_bytes INTEGER NOT NULL DEFAULT 0;
                 INSERT INTO event_chunks(event_id,data) SELECT id,body FROM events WHERE length(body)>0;
                 UPDATE events SET retained_bytes=length(body),body=X'';
                 PRAGMA user_version=1;
                 COMMIT;"
            ).map_err(|e| e.to_string())?;
        }
        let (enabled, retention_days, storage_limit_bytes) = conn
            .query_row(
                "SELECT enabled, days, storage_limit_bytes FROM config WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| e.to_string())?;
        let store = Self {
            conn,
            enabled,
            retention_days,
            storage_limit_bytes,
            epoch: 0,
            last_error: None,
        };
        // A process restart cannot resume an old body stream.
        store
            .conn
            .execute(
                "UPDATE events SET complete=1, note='应用重启，采集未完成' WHERE complete=0",
                [],
            )
            .map_err(|e| e.to_string())?;
        store.cleanup(now_ms())?;
        Ok(store)
    }

    fn cleanup(&self, now: i64) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM traces WHERE created_at_ms < ?1",
                [now.saturating_sub(i64::from(self.retention_days) * 86_400_000)],
            )
            .map_err(|e| e.to_string())?;
        self.conn.execute("DELETE FROM traces WHERE trace_id IN (SELECT trace_id FROM traces ORDER BY created_at_ms DESC, rowid DESC LIMIT -1 OFFSET ?1)", [TRACE_LIMIT]).map_err(|e| e.to_string())?;
        let mut size: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(retained_bytes+length(CAST(metadata AS BLOB))),0) FROM events",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        while size > self.storage_limit_bytes {
            self.conn.execute("DELETE FROM traces WHERE trace_id=(SELECT trace_id FROM traces ORDER BY created_at_ms, rowid LIMIT 1)", []).map_err(|e| e.to_string())?;
            size = self
                .conn
                .query_row(
                    "SELECT COALESCE(SUM(retained_bytes+length(CAST(metadata AS BLOB))),0) FROM events",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn snapshot(&self, dropped: u64) -> Result<DiagnosticSnapshot, String> {
        let mut stmt = self.conn.prepare("SELECT trace_id,cli_key,method,path,created_at_ms,status,capture_limited FROM traces ORDER BY created_at_ms DESC,rowid DESC LIMIT 100").map_err(|e| e.to_string())?;
        let traces = stmt
            .query_map([], |r| {
                Ok(DiagnosticTrace {
                    trace_id: r.get(0)?,
                    cli_key: r.get(1)?,
                    method: r.get(2)?,
                    path: r.get(3)?,
                    created_at_ms: r.get(4)?,
                    status: r.get(5)?,
                    capture_limited: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let stored_bytes = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(retained_bytes+length(CAST(metadata AS BLOB))),0) FROM events",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(DiagnosticSnapshot {
            enabled: self.enabled,
            retention_days: self.retention_days,
            storage_limit_bytes: self.storage_limit_bytes,
            stored_bytes,
            dropped_messages: dropped.min(u32::MAX as u64) as u32,
            last_error: self.last_error.clone(),
            traces,
        })
    }

    fn events(&self, trace_id: &str) -> Result<Vec<DiagnosticEvent>, String> {
        let mut stmt = self.conn.prepare("SELECT id,phase,created_at_ms,metadata,retained_bytes,bytes_seen,complete,note FROM events WHERE trace_id=?1 ORDER BY rowid").map_err(|e| e.to_string())?;
        let events = stmt
            .query_map([trace_id], |r| {
                let id: String = r.get(0)?;
                let retained_bytes: i64 = r.get(4)?;
                let bytes_seen: i64 = r.get(5)?;
                let mut chunks = self.conn.prepare(
                    "SELECT substr(data,1,?2) FROM event_chunks WHERE event_id=?1 ORDER BY id",
                )?;
                let mut rows = chunks.query(params![id, BODY_PREVIEW_LIMIT as i64])?;
                let mut bytes = Vec::new();
                while bytes.len() < BODY_PREVIEW_LIMIT {
                    let Some(row) = rows.next()? else {
                        break;
                    };
                    let chunk: Vec<u8> = row.get(0)?;
                    let count = chunk.len().min(BODY_PREVIEW_LIMIT - bytes.len());
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let binary = std::str::from_utf8(&bytes).is_err()
                    || bytes
                        .iter()
                        .any(|b| *b < 0x20 && !matches!(*b, b'\n' | b'\r' | b'\t'));
                let (body, body_encoding) = if binary {
                    (
                        base64::engine::general_purpose::STANDARD.encode(&bytes),
                        "base64",
                    )
                } else {
                    (String::from_utf8_lossy(&bytes).into_owned(), "utf8")
                };
                Ok(DiagnosticEvent {
                    id,
                    phase: r.get(1)?,
                    created_at_ms: r.get(2)?,
                    metadata: r.get(3)?,
                    body,
                    body_encoding: body_encoding.into(),
                    bytes_seen,
                    retained_bytes,
                    preview_truncated: retained_bytes > bytes.len() as i64,
                    truncated: bytes_seen > retained_bytes,
                    complete: r.get(6)?,
                    note: r.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
        events
    }

    fn write_body(&self, trace: &str, event_id: &str, path: &Path) -> Result<(), String> {
        if !path.is_absolute() {
            return Err("SEC_INVALID_INPUT: 正文保存路径必须为绝对路径".into());
        }
        let exists: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE trace_id=?1 AND id=?2)",
                params![trace, event_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !exists {
            return Err("通信内容已被清理，请刷新后重试".into());
        }
        let file = std::fs::File::create(path).map_err(|e| format!("保存通信正文失败：{e}"))?;
        let mut writer = std::io::BufWriter::new(file);
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM event_chunks WHERE event_id=?1 ORDER BY id")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([event_id]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let chunk: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
            writer
                .write_all(&chunk)
                .map_err(|e| format!("保存通信正文失败：{e}"))?;
        }
        writer.flush().map_err(|e| format!("保存通信正文失败：{e}"))
    }

    fn capture(&self, msg: Message) -> Result<(), String> {
        match msg {
            Message::Begin(epoch, t) if self.enabled && epoch == self.epoch => {
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO traces(trace_id,cli_key,method,path,created_at_ms,status,epoch) VALUES(?1,?2,?3,?4,?5,NULL,?6)",
                        params![
                            t.trace_id,
                            t.cli_key,
                            t.method,
                            t.path,
                            t.created_at_ms,
                            epoch
                        ],
                    )
                    .map_err(|e| e.to_string())?;
            }
            Message::Event(epoch, trace, id, phase, metadata, time)
                if self.enabled && epoch == self.epoch =>
            {
                self.conn.execute("UPDATE traces SET capture_limited=1 WHERE trace_id=?1 AND (SELECT COUNT(*) FROM events WHERE trace_id=?1) >= ?2", params![trace,EVENT_LIMIT]).map_err(|e| e.to_string())?;
                self.conn.execute("INSERT INTO events(id,trace_id,phase,metadata,created_at_ms) SELECT ?1,?2,?3,?4,?5 WHERE EXISTS(SELECT 1 FROM traces WHERE trace_id=?2 AND epoch=?7) AND (SELECT COUNT(*) FROM events WHERE trace_id=?2) < ?6", params![id,trace,phase,metadata,time,EVENT_LIMIT,epoch]).map_err(|e| e.to_string())?;
            }
            Message::Chunk(epoch, id, bytes, seen) if self.enabled && epoch == self.epoch => {
                let tx = self
                    .conn
                    .unchecked_transaction()
                    .map_err(|e| e.to_string())?;
                tx.execute("INSERT INTO event_chunks(event_id,data) SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM events WHERE id=?1)", params![id,bytes]).map_err(|e| e.to_string())?;
                tx.execute(
                    "UPDATE events SET retained_bytes=retained_bytes+?1,bytes_seen=?2 WHERE id=?3",
                    params![bytes.len() as i64, seen, id],
                )
                .map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
            }
            Message::End(epoch, id, seen, complete, note)
                if self.enabled && epoch == self.epoch =>
            {
                self.conn
                    .execute(
                        "UPDATE events SET bytes_seen=?1,complete=?2,note=?3 WHERE id=?4",
                        params![seen, complete, note, id],
                    )
                    .map_err(|e| e.to_string())?;
            }
            Message::Status(epoch, trace, status) if self.enabled && epoch == self.epoch => {
                self.conn
                    .execute(
                        "UPDATE traces SET status=?1 WHERE trace_id=?2 AND epoch=?3",
                        params![status, trace, epoch],
                    )
                    .map_err(|e| e.to_string())?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn now_ms() -> i64 {
    super::util::now_unix_millis().min(i64::MAX as u64) as i64
}

pub(crate) fn init<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<(), String> {
    let _guard = INIT_LOCK.lock().map_err(|e| e.to_string())?;
    if CLIENT.get().is_some() {
        return Ok(());
    }
    let path = crate::app_paths::app_data_dir(app)
        .map_err(|e| e.to_string())?
        .join("gateway-diagnostics.sqlite3");
    let store = Store::open(&path)?;
    let (tx, rx) = mpsc::sync_channel(256);
    let client = Arc::new(Client {
        tx,
        enabled: AtomicBool::new(store.enabled),
        epoch: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
    });
    let worker_client = client.clone();
    std::thread::Builder::new()
        .name("gateway-diagnostics".into())
        .spawn(move || run_worker(store, rx, worker_client))
        .map_err(|e| e.to_string())?;
    CLIENT
        .set(client)
        .map_err(|_| "诊断存储重复初始化".to_string())
}

fn run_worker(mut store: Store, rx: mpsc::Receiver<Message>, client: Arc<Client>) {
    let mut maintenance = Instant::now();
    loop {
        let msg = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(msg) => Some(msg),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if maintenance.elapsed() >= Duration::from_secs(60) {
            if let Err(error) = store.cleanup(now_ms()).and_then(|_| {
                store
                    .conn
                    .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA incremental_vacuum;")
                    .map_err(|e| e.to_string())
            }) {
                store.last_error = Some(error);
            }
            maintenance = Instant::now();
        }
        let Some(msg) = msg else {
            continue;
        };
        match msg {
            #[cfg(test)]
            Message::Stop => break,
            Message::Read(trace, reply) => {
                let result = store.cleanup(now_ms()).and_then(|_| match trace {
                    Some(trace) => store.events(&trace).map(ReadResult::Events),
                    None => store
                        .snapshot(client.dropped.load(Ordering::Relaxed))
                        .map(ReadResult::Snapshot),
                });
                let _ = reply.send(result);
            }
            Message::SaveBody(trace, event_id, path, reply) => {
                let result = store
                    .cleanup(now_ms())
                    .and_then(|_| store.write_body(&trace, &event_id, Path::new(&path)));
                let _ = reply.send(result);
            }
            Message::Configure(enabled, days, storage_limit_bytes, reply) => {
                let result = validate_configuration(days, storage_limit_bytes).and_then(|_| store.conn.execute("UPDATE config SET enabled=?1,days=?2,storage_limit_bytes=?3 WHERE id=1", params![enabled,days,storage_limit_bytes]).map_err(|e| e.to_string())).and_then(|_| {
                    if store.enabled != enabled {
                        store.epoch += 1;
                        client.epoch.store(store.epoch, Ordering::Release);
                        store.conn.execute("UPDATE events SET complete=1,note='采集已停止' WHERE complete=0", []).map_err(|e| e.to_string())?;
                    }
                    store.enabled = enabled;
                    store.retention_days = days;
                    store.storage_limit_bytes = storage_limit_bytes;
                    client.enabled.store(enabled, Ordering::Release);
                    store.cleanup(now_ms())
                });
                let _ = reply.send(result);
            }
            Message::Clear(reset, reply) => {
                store.epoch += 1;
                client.epoch.store(store.epoch, Ordering::Release);
                client.dropped.store(0, Ordering::Relaxed);
                let result = store
                    .conn
                    .execute_batch("DELETE FROM traces; PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
                    .map_err(|e| e.to_string())
                    .and_then(|_| {
                        if reset {
                            store.enabled = false;
                            store.retention_days = 15;
                            store.storage_limit_bytes = DEFAULT_STORAGE_LIMIT;
                            client.enabled.store(false, Ordering::Release);
                            store
                                .conn
                                .execute(
                                    "UPDATE config SET enabled=0,days=15,storage_limit_bytes=?1",
                                    [DEFAULT_STORAGE_LIMIT],
                                )
                                .map_err(|e| e.to_string())?;
                        }
                        store.last_error = None;
                        Ok(())
                    });
                let _ = reply.send(result);
            }
            capture => {
                if let Err(error) = store.capture(capture).and_then(|_| store.cleanup(now_ms())) {
                    store.last_error = Some(error);
                }
            }
        }
    }
}

fn client() -> Result<&'static Arc<Client>, String> {
    CLIENT.get().ok_or_else(|| "通信驻留存储尚未初始化".into())
}

fn read(trace: Option<String>) -> Result<ReadResult, String> {
    let (tx, rx) = mpsc::channel();
    client()?
        .tx
        .send(Message::Read(trace, tx))
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

pub(crate) fn snapshot() -> Result<DiagnosticSnapshot, String> {
    match read(None)? {
        ReadResult::Snapshot(s) => Ok(s),
        _ => unreachable!(),
    }
}

pub(crate) fn events(trace: String) -> Result<Vec<DiagnosticEvent>, String> {
    if trace.is_empty() || trace.len() > 256 {
        return Err("SEC_INVALID_INPUT: invalid trace_id".into());
    }
    match read(Some(trace))? {
        ReadResult::Events(e) => Ok(e),
        _ => unreachable!(),
    }
}

pub(crate) fn save_body(trace: String, event_id: String, path: String) -> Result<(), String> {
    if trace.is_empty() || trace.len() > 256 {
        return Err("SEC_INVALID_INPUT: invalid trace_id".into());
    }
    if event_id.is_empty() || event_id.len() > 256 {
        return Err("SEC_INVALID_INPUT: invalid event_id".into());
    }
    let (tx, rx) = mpsc::channel();
    client()?
        .tx
        .send(Message::SaveBody(trace, event_id, path, tx))
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

fn validate_configuration(days: u32, storage_limit_bytes: i64) -> Result<(), String> {
    if !(1..=365).contains(&days) {
        return Err("驻留时间必须为 1–365 天".into());
    }
    if !(MIN_STORAGE_LIMIT..=MAX_STORAGE_LIMIT).contains(&storage_limit_bytes) {
        return Err("驻留容量必须至少为 1 MiB，且为有效的安全整数".into());
    }
    Ok(())
}

pub(crate) fn configure(enabled: bool, days: u32, storage_limit_bytes: i64) -> Result<(), String> {
    validate_configuration(days, storage_limit_bytes)?;
    let (tx, rx) = mpsc::channel();
    client()?
        .tx
        .send(Message::Configure(enabled, days, storage_limit_bytes, tx))
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

pub(crate) fn clear(reset: bool) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    client()?
        .tx
        .send(Message::Clear(reset, tx))
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

impl Client {
    fn offer(&self, msg: Message) {
        if self.tx.try_send(msg).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Clone)]
pub(in crate::gateway) struct Capture {
    client: Arc<Client>,
    epoch: u64,
    trace: String,
}

impl Capture {
    pub(in crate::gateway) fn begin(
        trace: &str,
        cli: &str,
        method: &str,
        path: &str,
    ) -> Option<Self> {
        let client = CLIENT.get()?;
        if !client.enabled.load(Ordering::Acquire) {
            return None;
        }
        let capture = Self {
            client: client.clone(),
            epoch: client.epoch.load(Ordering::Acquire),
            trace: trace.into(),
        };
        client.offer(Message::Begin(
            capture.epoch,
            DiagnosticTrace {
                trace_id: trace.into(),
                cli_key: cli.into(),
                method: method.into(),
                path: bounded(path, 2048),
                created_at_ms: now_ms(),
                status: None,
                capture_limited: false,
            },
        ));
        Some(capture)
    }

    pub(in crate::gateway) fn for_trace(trace: &str) -> Option<Self> {
        let client = CLIENT.get()?;
        if !client.enabled.load(Ordering::Acquire) {
            return None;
        }
        Some(Self {
            client: client.clone(),
            epoch: client.epoch.load(Ordering::Acquire),
            trace: trace.into(),
        })
    }

    pub(in crate::gateway) fn event(&self, phase: &'static str, metadata: String) -> BodyCapture {
        let id = format!(
            "{}-{}",
            self.trace,
            EVENT_ID.fetch_add(1, Ordering::Relaxed)
        );
        self.client.offer(Message::Event(
            self.epoch,
            self.trace.clone(),
            id.clone(),
            phase,
            bounded(&metadata, 8192),
            now_ms(),
        ));
        BodyCapture {
            capture: self.clone(),
            id,
            seen: 0,
            ended: false,
        }
    }

    pub(in crate::gateway) fn status(&self, status: u16) {
        self.client
            .offer(Message::Status(self.epoch, self.trace.clone(), status));
    }
}

fn bounded(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

pub(in crate::gateway) fn http_metadata(first_line: &str, headers: &HeaderMap) -> String {
    format!(
        "{}\n{}",
        bounded(first_line, 2048),
        super::util::redacted_headers_for_debug(headers)
    )
}

pub(in crate::gateway) fn safe_url(url: &reqwest::Url) -> String {
    let mut safe = url.clone();
    let _ = safe.set_username("");
    let _ = safe.set_password(None);
    safe.set_query(url.query().map(|_| "[redacted]"));
    safe.set_fragment(None);
    safe.to_string()
}

pub(in crate::gateway) fn transport_error_details(
    error: &(dyn std::error::Error + 'static),
    url: Option<&reqwest::Url>,
) -> String {
    let mut parts = Vec::new();
    let mut current = Some(error);
    for _ in 0..8 {
        let Some(error) = current else {
            break;
        };
        let mut text = error.to_string();
        if let Some(url) = url {
            text = text.replace(url.as_str(), &safe_url(url));
        }
        parts.push(bounded(&text, 1024));
        current = error.source();
    }
    parts.join("\n原因：")
}

pub(in crate::gateway) struct BodyCapture {
    capture: Capture,
    id: String,
    seen: i64,
    ended: bool,
}

impl BodyCapture {
    pub(in crate::gateway) fn chunk(&mut self, bytes: &[u8]) {
        if !self.capture.client.enabled.load(Ordering::Acquire)
            || self.capture.epoch != self.capture.client.epoch.load(Ordering::Acquire)
        {
            self.ended = true;
            return;
        }
        for chunk in bytes.chunks(CHUNK_SIZE) {
            self.seen = self.seen.saturating_add(chunk.len() as i64);
            self.capture.client.offer(Message::Chunk(
                self.capture.epoch,
                self.id.clone(),
                chunk.to_vec(),
                self.seen,
            ));
        }
    }

    pub(in crate::gateway) fn finish(&mut self, note: Option<&str>) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.capture.client.offer(Message::End(
            self.capture.epoch,
            self.id.clone(),
            self.seen,
            true,
            note.map(|s| bounded(s, 512)),
        ));
    }
}

impl Drop for BodyCapture {
    fn drop(&mut self) {
        if !self.ended {
            self.finish(Some("流被取消或未读取完毕"));
        }
    }
}

struct CapturedBody {
    inner: Body,
    capture: BodyCapture,
}

impl HttpBody for CapturedBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, axum::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    self.capture.chunk(data);
                }
                if self.inner.is_end_stream() {
                    self.capture.finish(None);
                }
            }
            Poll::Ready(Some(Err(_))) => self.capture.finish(Some("通信流读取错误")),
            Poll::Ready(None) => self.capture.finish(None),
            _ => {}
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

pub(in crate::gateway) fn capture_body(body: Body, mut capture: BodyCapture) -> Body {
    if body.is_end_stream() {
        capture.finish(None);
    }
    Body::new(CapturedBody {
        inner: body,
        capture,
    })
}

pub(in crate::gateway) struct CapturedStream<S> {
    inner: Pin<Box<S>>,
    capture: BodyCapture,
}

impl<S> CapturedStream<S> {
    pub(in crate::gateway) fn new(inner: S, capture: BodyCapture) -> Self {
        Self {
            inner: Box::pin(inner),
            capture,
        }
    }
}

impl<S, E> Stream for CapturedStream<S>
where
    S: Stream<Item = Result<Bytes, E>>,
{
    type Item = Result<Bytes, E>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = self.inner.as_mut().poll_next(cx);
        match &result {
            Poll::Ready(Some(Ok(bytes))) => self.capture.chunk(bytes),
            Poll::Ready(Some(Err(_))) => self.capture.finish(Some("供应商通信流读取错误")),
            Poll::Ready(None) => self.capture.finish(None),
            _ => {}
        }
        result
    }
}

#[cfg(test)]
mod tests;
