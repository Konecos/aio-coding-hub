use super::*;
use futures_util::StreamExt;
use std::collections::VecDeque;
use std::io;

fn trace(id: &str, time: i64) -> DiagnosticTrace {
    DiagnosticTrace {
        trace_id: id.into(),
        cli_key: "codex".into(),
        method: "POST".into(),
        path: "/v1/responses".into(),
        created_at_ms: time,
        status: None,
        capture_limited: false,
    }
}

fn enabled_store(path: &Path) -> Store {
    let mut store = Store::open(path).unwrap();
    store.enabled = true;
    store
}

fn seed(store: &Store, id: &str) {
    store
        .capture(Message::Begin(store.epoch, trace(id, now_ms())))
        .unwrap();
    store
        .capture(Message::Event(
            store.epoch,
            id.into(),
            format!("{id}-event"),
            "client_request",
            "headers".into(),
            now_ms(),
        ))
        .unwrap();
}

#[test]
fn persists_settings_bodies_and_marks_interrupted_streams_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostics.sqlite3");
    {
        let mut store = Store::open(&path).unwrap();
        assert!(!store.enabled);
        assert_eq!(store.retention_days, 15);
        store
            .conn
            .execute("UPDATE config SET enabled=1,days=30", [])
            .unwrap();
        store.enabled = true;
        seed(&store, "one");
        // Split a multibyte UTF-8 code point across two body chunks.
        let bytes = "中文".as_bytes();
        store
            .capture(Message::Chunk(
                0,
                "one-event".into(),
                bytes[..2].to_vec(),
                2,
            ))
            .unwrap();
        store
            .capture(Message::Chunk(
                0,
                "one-event".into(),
                bytes[2..].to_vec(),
                6,
            ))
            .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert!(store.enabled);
    assert_eq!(store.retention_days, 30);
    let events = store.events("one").unwrap();
    assert_eq!(events[0].body, "中文");
    assert_eq!(events[0].bytes_seen, 6);
    assert!(events[0].complete);
    assert_eq!(events[0].note.as_deref(), Some("应用重启，采集未完成"));
}

#[test]
fn caps_bodies_and_preserves_binary_bytes_with_nuls() {
    let store = enabled_store(Path::new(":memory:"));
    seed(&store, "binary");
    store
        .capture(Message::Chunk(0, "binary-event".into(), vec![0, 1, 2], 3))
        .unwrap();
    store
        .capture(Message::Chunk(0, "binary-event".into(), vec![3, 4], 5))
        .unwrap();
    let event = &store.events("binary").unwrap()[0];
    assert_eq!(event.body_encoding, "base64");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&event.body)
            .unwrap(),
        vec![0, 1, 2, 3, 4]
    );
    seed(&store, "large");
    store
        .capture(Message::Chunk(
            0,
            "large-event".into(),
            vec![b'x'; BODY_LIMIT + 100],
            (BODY_LIMIT + 100) as u32,
        ))
        .unwrap();
    let event = &store.events("large").unwrap()[0];
    assert_eq!(event.body.len(), BODY_LIMIT);
    assert!(event.truncated);
}

#[test]
fn expires_by_configured_age_and_caps_trace_and_event_counts() {
    let mut store = enabled_store(Path::new(":memory:"));
    let now = now_ms();
    store
        .capture(Message::Begin(0, trace("old", now - 16 * 86_400_000)))
        .unwrap();
    store
        .capture(Message::Begin(0, trace("keep", now - 2 * 86_400_000)))
        .unwrap();
    store.cleanup(now).unwrap();
    assert_eq!(store.snapshot(0).unwrap().traces[0].trace_id, "keep");
    store.retention_days = 1;
    store.cleanup(now).unwrap();
    assert!(store.snapshot(0).unwrap().traces.is_empty());
    for index in 0..(TRACE_LIMIT + 1) {
        store
            .capture(Message::Begin(
                0,
                trace(&format!("trace-{index}"), now + index),
            ))
            .unwrap();
    }
    store.cleanup(now).unwrap();
    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM traces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, TRACE_LIMIT);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM traces WHERE trace_id='trace-0'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    for index in 0..(EVENT_LIMIT + 1) {
        store
            .capture(Message::Event(
                0,
                "trace-1".into(),
                format!("event-{index}"),
                "upstream_request",
                "".into(),
                now,
            ))
            .unwrap();
    }
    assert_eq!(store.events("trace-1").unwrap().len(), EVENT_LIMIT as usize);
    assert!(store
        .conn
        .query_row(
            "SELECT capture_limited FROM traces WHERE trace_id='trace-1'",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
}

#[test]
fn evicts_oldest_trace_when_total_storage_limit_is_exceeded() {
    let store = enabled_store(Path::new(":memory:"));
    seed(&store, "oldest");
    std::thread::sleep(Duration::from_millis(2));
    seed(&store, "newest");
    store
        .conn
        .execute(
            "UPDATE events SET body=zeroblob(?1) WHERE trace_id='oldest'",
            [STORAGE_LIMIT],
        )
        .unwrap();
    store.cleanup(now_ms()).unwrap();
    assert!(store.events("oldest").unwrap().is_empty());
    assert_eq!(store.snapshot(0).unwrap().traces[0].trace_id, "newest");
}

struct Harness {
    client: Arc<Client>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Harness {
    fn new(path: &Path) -> Self {
        let store = Store::open(path).unwrap();
        let (tx, rx) = mpsc::sync_channel(256);
        let client = Arc::new(Client {
            tx,
            enabled: AtomicBool::new(store.enabled),
            epoch: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        });
        let worker = client.clone();
        let thread = std::thread::spawn(move || run_worker(store, rx, worker));
        Self {
            client,
            thread: Some(thread),
        }
    }
    fn configure(&self, enabled: bool, days: u32) {
        let (tx, rx) = mpsc::channel();
        self.client
            .tx
            .send(Message::Configure(enabled, days, tx))
            .unwrap();
        rx.recv().unwrap().unwrap();
    }
    fn clear(&self, reset: bool) {
        let (tx, rx) = mpsc::channel();
        self.client.tx.send(Message::Clear(reset, tx)).unwrap();
        rx.recv().unwrap().unwrap();
    }
    fn read(&self, trace: Option<String>) -> ReadResult {
        let (tx, rx) = mpsc::channel();
        self.client.tx.send(Message::Read(trace, tx)).unwrap();
        rx.recv().unwrap().unwrap()
    }
    fn begin(&self, id: &str) -> Capture {
        let epoch = self.client.epoch.load(Ordering::Acquire);
        self.client
            .tx
            .send(Message::Begin(epoch, trace(id, now_ms())))
            .unwrap();
        Capture {
            client: self.client.clone(),
            epoch,
            trace: id.into(),
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.client.tx.send(Message::Stop).unwrap();
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn clear_and_disable_reject_late_stream_data_and_reset_restores_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostics.sqlite3");
    {
        let h = Harness::new(&path);
        h.configure(true, 7);
        let capture = h.begin("before-clear");
        let mut body = capture.event("client_request", "".into());
        body.chunk(b"before");
        h.clear(false);
        body.chunk(b"after");
        body.finish(None);
        if let ReadResult::Snapshot(snapshot) = h.read(None) {
            assert!(snapshot.traces.is_empty());
            assert!(snapshot.enabled);
        } else {
            panic!();
        }
        let capture = h.begin("before-disable");
        let mut body = capture.event("client_request", "".into());
        body.chunk(b"kept");
        h.configure(false, 7);
        body.chunk(b"discarded");
        h.configure(true, 7);
        // A late response must not attach using the new generation either.
        let late = Capture {
            client: h.client.clone(),
            epoch: h.client.epoch.load(Ordering::Acquire),
            trace: capture.trace.clone(),
        };
        late.event("client_response", "".into()).finish(None);
        if let ReadResult::Events(events) = h.read(Some(capture.trace)) {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].body, "kept");
            assert!(events[0].complete);
        } else {
            panic!();
        }
    }
    {
        let h = Harness::new(&path);
        if let ReadResult::Snapshot(snapshot) = h.read(None) {
            assert!(snapshot.enabled);
            assert_eq!(snapshot.retention_days, 7);
        } else {
            panic!();
        }
        h.clear(true);
        if let ReadResult::Snapshot(snapshot) = h.read(None) {
            assert!(!snapshot.enabled);
            assert_eq!(snapshot.retention_days, 15);
            assert!(snapshot.traces.is_empty());
        } else {
            panic!();
        }
    }
}

#[test]
fn metadata_redacts_credentials_and_url_queries() {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer secret".parse().unwrap());
    headers.insert("cookie", "session=private".parse().unwrap());
    headers.insert("x-api-key", "private-key".parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());
    let url =
        reqwest::Url::parse("https://user:password@example.com/v1/messages?key=private#secret")
            .unwrap();
    let metadata = http_metadata(&safe_url(&url), &headers);
    for secret in [
        "Bearer secret",
        "session=private",
        "private-key",
        "password",
        "user:",
        "key=private",
        "#secret",
    ] {
        assert!(!metadata.contains(secret));
    }
    assert!(metadata.contains("application/json"));
    let error = io::Error::other(format!("failed sending {}", url));
    let details = transport_error_details(&error, Some(&url));
    assert!(!details.contains("password"));
    assert!(!details.contains("key=private"));
    assert!(details.contains("example.com/v1/messages"));
}

fn capture_pair() -> (Capture, mpsc::Receiver<Message>) {
    let (tx, rx) = mpsc::sync_channel(256);
    let client = Arc::new(Client {
        tx,
        enabled: AtomicBool::new(true),
        epoch: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
    });
    (
        Capture {
            client,
            epoch: 0,
            trace: "stream".into(),
        },
        rx,
    )
}

#[tokio::test]
async fn captured_stream_preserves_chunks_errors_and_cancellation() {
    let (capture, rx) = capture_pair();
    let items: Vec<Result<Bytes, io::Error>> = vec![
        Ok(Bytes::from_static(b"one")),
        Ok(Bytes::from_static(b"two")),
        Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset")),
    ];
    let mut stream = CapturedStream::new(
        futures_util::stream::iter(items),
        capture.event("upstream_response", "".into()),
    );
    assert_eq!(stream.next().await.unwrap().unwrap(), "one");
    assert_eq!(stream.next().await.unwrap().unwrap(), "two");
    assert_eq!(
        stream.next().await.unwrap().unwrap_err().kind(),
        io::ErrorKind::ConnectionReset
    );
    assert!(stream.next().await.is_none());
    assert!(rx
        .try_iter()
        .any(|m| matches!(m, Message::End(_, _, 6, true, Some(_)))));
    let body = capture.event("client_request", "".into());
    drop(body);
    assert!(rx
        .try_iter()
        .any(|m| matches!(m, Message::End(_, _, 0, true, Some(_)))));
}

struct FramedBody {
    frames: VecDeque<http_body::Frame<Bytes>>,
}
impl HttpBody for FramedBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, io::Error>>> {
        Poll::Ready(self.frames.pop_front().map(Ok))
    }
    fn is_end_stream(&self) -> bool {
        self.frames.is_empty()
    }
}

#[tokio::test]
async fn captured_http_body_preserves_trailers_and_empty_completion() {
    let (capture, rx) = capture_pair();
    let mut trailers = HeaderMap::new();
    trailers.insert("x-finished", "yes".parse().unwrap());
    let frames = VecDeque::from(vec![
        http_body::Frame::data(Bytes::from_static(b"hello")),
        http_body::Frame::trailers(trailers.clone()),
    ]);
    let mut body = capture_body(
        Body::new(FramedBody { frames }),
        capture.event("client_response", "".into()),
    );
    let first = futures_util::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.data_ref().unwrap(), "hello");
    let last = futures_util::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.trailers_ref().unwrap(), &trailers);
    drop(body);
    assert!(rx
        .try_iter()
        .any(|m| matches!(m, Message::End(_, _, 5, true, None))));
    drop(capture_body(
        Body::empty(),
        capture.event("client_response", "".into()),
    ));
    assert!(rx
        .try_iter()
        .any(|m| matches!(m, Message::End(_, _, 0, true, None))));
}

#[test]
fn saturated_queue_drops_capture_without_blocking_and_reports_loss() {
    let (tx, _rx) = mpsc::sync_channel(1);
    let client = Client {
        tx,
        enabled: AtomicBool::new(true),
        epoch: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
    };
    client.offer(Message::Begin(0, trace("first", now_ms())));
    client.offer(Message::Begin(0, trace("second", now_ms())));
    assert_eq!(client.dropped.load(Ordering::Relaxed), 1);
}
