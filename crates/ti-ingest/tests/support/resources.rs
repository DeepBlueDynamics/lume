use serde_json::Value;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
pub struct MockSignalK {
    pub url: String,
    pub notes: Arc<Mutex<(u16, Value)>>,
    pub logbook: Arc<Mutex<(u16, Value)>>,
    pub requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl MockSignalK {
    pub fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let notes = Arc::new(Mutex::new((200, serde_json::json!({}))));
        let logbook = Arc::new(Mutex::new((404, serde_json::json!({}))));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (n, l, r, s) = (
            notes.clone(),
            logbook.clone(),
            requests.clone(),
            stop.clone(),
        );
        let worker = thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                // Windows: accepted sockets inherit the listener's non-blocking mode, so an early
                // read would return WouldBlock and the mock would answer an empty request.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let count = stream.read(&mut buf).unwrap_or(0);
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..count]);
                    if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes).to_string();
                r.lock().unwrap().push(request.clone());
                let target = if request.starts_with("GET /signalk/v2/api/resources/notes ") {
                    Some(&n)
                } else if request.starts_with("GET /signalk/v2/api/resources/logentries?") {
                    Some(&l)
                } else {
                    None
                };
                let (code, value) =
                    target
                        .map(|a| a.lock().unwrap().clone())
                        .unwrap_or_else(|| {
                            let path = request.split_whitespace().nth(1).unwrap_or("");
                            l.lock()
                                .unwrap()
                                .1
                                .get("legacy")
                                .and_then(|v| v.get(path))
                                .cloned()
                                .map(|value| (200, value))
                                .unwrap_or((404, serde_json::json!({})))
                        });
                let body = value.to_string();
                let response=format!("HTTP/1.1 {code} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            url,
            notes,
            logbook,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for MockSignalK {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
