#![allow(dead_code)]
#![allow(clippy::field_reassign_with_default)]

use curl_downloader::{
    download::{EngineHandle, spawn_engine},
    model::{
        CURRENT_SCHEMA_VERSION, ConfiguredTask, EngineCommand, EngineEvent, GlobalSettings,
        NewTask, PersistedState, ProxySettings, SegmentSnapshot, TaskId, TaskSnapshot, TaskStatus,
    },
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub struct TestProxy {
    pub address: String,
    pub requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    atypes: Arc<Mutex<Vec<u8>>>,
}

impl TestProxy {
    pub fn http(body: &'static [u8], expected_basic: Option<&'static str>) -> Self {
        Self::start(ProxyKind::Http {
            body,
            expected_basic,
        })
    }

    pub fn socks5(body: &'static [u8]) -> Self {
        Self::start(ProxyKind::Socks5 { body })
    }

    pub fn recorded_atyp(&self) -> Vec<u8> {
        self.atypes.lock().unwrap().clone()
    }

    fn start(kind: ProxyKind) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = Arc::clone(&requests);
        let atypes = Arc::new(Mutex::new(Vec::new()));
        let thread_atypes = Arc::clone(&atypes);
        let thread = thread::Builder::new()
            .name("test-proxy".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            let kind = kind.clone();
                            let requests = Arc::clone(&thread_requests);
                            let atypes = Arc::clone(&thread_atypes);
                            let _ = thread::Builder::new().spawn(move || match kind {
                                ProxyKind::Http {
                                    body,
                                    expected_basic,
                                } => serve_http_proxy(stream, body, expected_basic, requests),
                                ProxyKind::Socks5 { body } => {
                                    serve_socks5_proxy(stream, body, requests, atypes)
                                }
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
            .unwrap();
        Self {
            address,
            requests,
            stop,
            thread: Some(thread),
            atypes,
        }
    }
}

impl Drop for TestProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
enum ProxyKind {
    Http {
        body: &'static [u8],
        expected_basic: Option<&'static str>,
    },
    Socks5 {
        body: &'static [u8],
    },
}

fn read_until_headers(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.len() > 64 * 1024 {
            return None;
        }
    }
    Some(request)
}

fn serve_http_proxy(
    mut stream: TcpStream,
    body: &'static [u8],
    expected_basic: Option<&'static str>,
    requests: Arc<Mutex<Vec<String>>>,
) {
    let Some(request) = read_until_headers(&mut stream) else {
        return;
    };
    let text = String::from_utf8_lossy(&request).into_owned();
    requests.lock().unwrap().push(text.clone());
    let authorized = expected_basic.is_none_or(|expected| {
        text.lines().any(|line| {
            line.strip_prefix("Proxy-Authorization:")
                .is_some_and(|value| value.trim() == expected)
        })
    });
    if !authorized {
        let _ = stream.write_all(
            b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=proxy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: proxy-v1\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if stream.write_all(response.as_bytes()).is_ok() {
        let _ = stream.write_all(body);
    }
}

fn read_exact(stream: &mut TcpStream, buffer: &mut [u8]) -> bool {
    stream.read_exact(buffer).is_ok()
}

fn serve_socks5_proxy(
    mut stream: TcpStream,
    body: &'static [u8],
    requests: Arc<Mutex<Vec<String>>>,
    atypes: Arc<Mutex<Vec<u8>>>,
) {
    let mut greeting = [0_u8; 2];
    if !read_exact(&mut stream, &mut greeting) || greeting[0] != 5 {
        return;
    }
    let mut methods = vec![0_u8; usize::from(greeting[1])];
    if !read_exact(&mut stream, &mut methods) {
        return;
    }
    let method = if methods.contains(&2) {
        2
    } else if methods.contains(&0) {
        0
    } else {
        0xff
    };
    if stream.write_all(&[5, method]).is_err() || method == 0xff {
        return;
    }
    if method == 2 {
        let mut auth_header = [0_u8; 2];
        if !read_exact(&mut stream, &mut auth_header) || auth_header[0] != 1 {
            return;
        }
        let mut username = vec![0_u8; usize::from(auth_header[1])];
        if !read_exact(&mut stream, &mut username) {
            return;
        }
        let mut password_length = [0_u8; 1];
        if !read_exact(&mut stream, &mut password_length) {
            return;
        }
        let mut password = vec![0_u8; usize::from(password_length[0])];
        if !read_exact(&mut stream, &mut password) || stream.write_all(&[1, 0]).is_err() {
            return;
        }
    }
    let mut request = [0_u8; 4];
    if !read_exact(&mut stream, &mut request) || request[0] != 5 || request[1] != 1 {
        return;
    }
    atypes.lock().unwrap().push(request[3]);
    match request[3] {
        1 => {
            let mut address = [0_u8; 4];
            if !read_exact(&mut stream, &mut address) {
                return;
            }
        }
        3 => {
            let mut length = [0_u8; 1];
            if !read_exact(&mut stream, &mut length) {
                return;
            }
            let mut address = vec![0_u8; usize::from(length[0])];
            if !read_exact(&mut stream, &mut address) {
                return;
            }
        }
        4 => {
            let mut address = [0_u8; 16];
            if !read_exact(&mut stream, &mut address) {
                return;
            }
        }
        _ => return,
    }
    let mut port = [0_u8; 2];
    if !read_exact(&mut stream, &mut port) {
        return;
    }
    if stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).is_err() {
        return;
    }
    let Some(request) = read_until_headers(&mut stream) else {
        return;
    };
    requests
        .lock()
        .unwrap()
        .push(String::from_utf8_lossy(&request).into_owned());
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: socks-v1\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if stream.write_all(response.as_bytes()).is_ok() {
        let _ = stream.write_all(body);
    }
}

pub struct TestHttpServer {
    pub base_url: String,
    pub stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

pub struct Route {
    pub path: &'static str,
    pub body: &'static [u8],
    pub ranges: bool,
    pub etag: &'static str,
    pub filename: &'static str,
}

impl TestHttpServer {
    pub fn start(routes: Vec<Route>) -> Self {
        Self::start_with_delay(routes, 0)
    }

    pub fn start_slow(body: &'static [u8], delay_ms: u64) -> Self {
        Self::start_with_delay(
            vec![Route {
                path: "/slow.bin",
                body,
                ranges: true,
                etag: "v1",
                filename: "slow.bin",
            }],
            delay_ms,
        )
    }

    fn start_with_delay(routes: Vec<Route>, delay_ms: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let ready = Arc::new(AtomicBool::new(false));
        let thread_ready = Arc::clone(&ready);
        let thread = thread::Builder::new()
            .name("test-http-server".into())
            .spawn(move || {
                let mut announced_ready = false;
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            let routes = routes
                                .iter()
                                .map(|route| Route {
                                    path: route.path,
                                    body: route.body,
                                    ranges: route.ranges,
                                    etag: route.etag,
                                    filename: route.filename,
                                })
                                .collect::<Vec<_>>();
                            let _ = thread::Builder::new()
                                .name("test-http-request".into())
                                .spawn(move || serve_connection(stream, &routes, delay_ms));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                    if !announced_ready {
                        thread_ready.store(true, Ordering::Release);
                        announced_ready = true;
                    }
                }
            })
            .unwrap();
        while !ready.load(Ordering::Acquire) {
            thread::yield_now();
        }
        Self {
            base_url: format!("http://{address}"),
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_connection(mut stream: TcpStream, routes: &[Route], delay_ms: u64) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let Ok(read) = stream.read(&mut buffer) else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.len() > 64 * 1024 {
            return;
        }
    }
    let request = String::from_utf8_lossy(&request);
    let mut lines = request.lines();
    let Some(request_line) = lines.next() else {
        return;
    };
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or("GET");
    let requested_path = request_parts
        .next()
        .and_then(|value| value.split('?').next())
        .unwrap_or("/");
    let route = routes.iter().find(|route| route.path == requested_path);
    let Some(route) = route else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let range = request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("range") {
            return None;
        }
        let value = value.trim().strip_prefix("bytes=")?;
        let (start, end) = value.split_once('-')?;
        Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
    });
    let (status, body, content_range) = if route.ranges {
        if let Some((start, requested_end)) = range {
            if start >= route.body.len() {
                (416, &route.body[0..0], None)
            } else {
                let end = requested_end.min(route.body.len() - 1);
                (206, &route.body[start..=end], Some((start, end)))
            }
        } else {
            (200, route.body, None)
        }
    } else {
        (200, route.body, None)
    };
    let status_text = match status {
        200 => "OK",
        206 => "Partial Content",
        416 => "Range Not Satisfiable",
        _ => "Error",
    };
    let mut response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Length: {}\r\nETag: {}\r\nContent-Disposition: attachment; filename={}\r\n",
        body.len(),
        route.etag,
        route.filename
    );
    if route.ranges {
        response.push_str("Accept-Ranges: bytes\r\n");
    }
    if let Some((start, end)) = content_range {
        response.push_str(&format!(
            "Content-Range: bytes {start}-{end}/{}\r\n",
            route.body.len()
        ));
    }
    response.push_str("Connection: close\r\n\r\n");
    if stream.write_all(response.as_bytes()).is_ok() && method != "HEAD" && status != 416 {
        if delay_ms == 0 {
            let _ = stream.write_all(body);
        } else {
            for chunk in body.chunks(4) {
                if stream.write_all(chunk).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(delay_ms));
            }
        }
    }
}

pub struct EngineHarness {
    pub engine: EngineHandle,
    state_path: PathBuf,
    download_dir: PathBuf,
    latest: Arc<Mutex<Vec<TaskSnapshot>>>,
    batch_proxy_result: Arc<Mutex<Option<(usize, usize)>>>,
    keep_files: bool,
}

impl EngineHarness {
    pub fn new(max_processes: u8) -> Self {
        let download_dir = unique_dir("engine");
        let state_path = download_dir.join("state.json");
        let state = PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: GlobalSettings {
                last_download_dir: download_dir.clone(),
                max_curl_processes: max_processes,
                next_task_id: 1,
            },
            tasks: Vec::new(),
        };
        let engine = spawn_engine(state_path.clone(), state).unwrap();
        Self {
            engine,
            state_path,
            download_dir,
            latest: Arc::new(Mutex::new(Vec::new())),
            batch_proxy_result: Arc::new(Mutex::new(None)),
            keep_files: false,
        }
    }

    pub fn from_state(state_path: PathBuf, download_dir: PathBuf) -> Self {
        let state = curl_downloader::storage::load_state(&state_path).unwrap();
        let engine = spawn_engine(state_path.clone(), state).unwrap();
        Self {
            engine,
            state_path,
            download_dir,
            latest: Arc::new(Mutex::new(Vec::new())),
            batch_proxy_result: Arc::new(Mutex::new(None)),
            keep_files: false,
        }
    }

    pub fn add_and_start(&mut self, url: String, segments: u8) -> TaskId {
        self.engine
            .commands
            .send(EngineCommand::Add(NewTask {
                url,
                target_dir: self.download_dir.clone(),
            }))
            .unwrap();
        let snapshot = self.wait_for_any(Duration::from_secs(5));
        let task = snapshot.first().unwrap().clone();
        self.engine
            .commands
            .send(EngineCommand::UpdateDraft {
                id: task.id,
                url: task.original_url.clone(),
                filename: task.filename.clone(),
                target_dir: self.download_dir.clone(),
                requested_segments: segments,
                proxy: ProxySettings::default(),
            })
            .unwrap();
        self.engine
            .commands
            .send(EngineCommand::Start(task.id))
            .unwrap();
        task.id
    }

    pub fn add_batch(&mut self, urls: &[String]) -> Vec<TaskSnapshot> {
        self.engine
            .commands
            .send(EngineCommand::AddBatch(
                urls.iter()
                    .cloned()
                    .map(|url| NewTask {
                        url,
                        target_dir: self.download_dir.clone(),
                    })
                    .collect(),
            ))
            .unwrap();
        self.wait_for_count(urls.len(), Duration::from_secs(5))
    }

    pub fn add_configured(
        &mut self,
        url: String,
        filename: String,
        target_dir: PathBuf,
        proxy: ProxySettings,
    ) -> TaskId {
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        self.engine
            .commands
            .send(EngineCommand::AddConfigured {
                task: ConfiguredTask {
                    url,
                    filename,
                    target_dir,
                    requested_segments: 1,
                    proxy,
                    request_id: None,
                    request_context: None,
                },
                response: response_tx,
            })
            .unwrap();
        response_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
    }

    pub fn add_with_proxy(
        &mut self,
        url: &str,
        protocol: curl_downloader::model::ProxyProtocol,
        proxy_address: &str,
        username: &str,
        password: &str,
    ) -> TaskId {
        let id = 1;
        self.engine
            .commands
            .send(EngineCommand::Add(NewTask {
                url: url.to_owned(),
                target_dir: self.download_dir.clone(),
            }))
            .unwrap();
        let proxy_url = url::Url::parse(proxy_address).unwrap();
        let mut proxy = ProxySettings::default();
        proxy.enabled = true;
        proxy.protocol = protocol;
        proxy.host = proxy_url.host_str().unwrap().to_owned();
        proxy.port = proxy_url.port().unwrap();
        proxy.username = username.to_owned();
        proxy.set_password(password.to_owned()).unwrap();
        self.engine
            .commands
            .send(EngineCommand::UpdateDraft {
                id,
                url: url.to_owned(),
                filename: url::Url::parse(url)
                    .ok()
                    .and_then(|parsed| {
                        parsed
                            .path_segments()
                            .and_then(|mut segments| segments.next_back())
                            .filter(|name| !name.is_empty())
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "download-1".to_owned()),
                target_dir: self.download_dir.clone(),
                requested_segments: 1,
                proxy,
            })
            .unwrap();
        id
    }

    pub fn wait_for(&mut self, id: TaskId, status: TaskStatus, timeout: Duration) -> TaskSnapshot {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(task) = self
                .latest
                .lock()
                .unwrap()
                .iter()
                .find(|task| task.id == id && task.status == status)
                .cloned()
            {
                return task;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for {status:?}; latest={:?}",
                self.latest.lock().unwrap()
            );
            self.poll_once(Duration::from_millis(100));
        }
    }

    pub fn wait_for_segment<F>(
        &mut self,
        id: TaskId,
        timeout: Duration,
        predicate: F,
    ) -> SegmentSnapshot
    where
        F: Fn(&SegmentSnapshot) -> bool,
    {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(segment) = self
                .latest
                .lock()
                .unwrap()
                .iter()
                .find(|task| task.id == id)
                .and_then(|task| task.segments.iter().find(|segment| predicate(segment)))
                .cloned()
            {
                return segment;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for segment; latest={:?}",
                self.latest.lock().unwrap()
            );
            self.poll_once(Duration::from_millis(100));
        }
    }
    pub fn wait_for_count(&mut self, count: usize, timeout: Duration) -> Vec<TaskSnapshot> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let snapshot = self.latest.lock().unwrap().clone();
            if snapshot.len() >= count {
                return snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for {count} tasks; latest={snapshot:?}"
            );
            self.poll_once(Duration::from_millis(100));
        }
    }

    pub fn wait_for_proxy(&mut self, ids: &[TaskId], host: &str) -> Vec<TaskSnapshot> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = self.latest.lock().unwrap().clone();
            if ids.iter().all(|id| {
                snapshot
                    .iter()
                    .any(|task| task.id == *id && task.proxy.enabled && task.proxy.host == host)
            }) {
                return snapshot
                    .into_iter()
                    .filter(|task| ids.contains(&task.id))
                    .collect();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for proxy update; latest={snapshot:?}"
            );
            self.poll_once(Duration::from_millis(100));
        }
    }

    pub fn wait_for_batch_proxy_result(&mut self) -> (usize, usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = self.batch_proxy_result.lock().unwrap().take() {
                return result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for batch proxy result"
            );
            self.poll_once(Duration::from_millis(100));
        }
    }

    pub fn wait_for_empty(&mut self, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let snapshot = self.latest.lock().unwrap().clone();
            if snapshot.is_empty() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for empty task history; latest={snapshot:?}"
            );
            self.poll_once(Duration::from_millis(100));
        }
    }

    pub fn max_observed_processes(&self) -> usize {
        self.engine.max_observed_processes()
    }

    pub fn resume(&mut self, id: TaskId) {
        self.engine.commands.send(EngineCommand::Start(id)).unwrap();
    }

    pub fn start(&mut self, id: TaskId) {
        self.resume(id);
    }

    pub fn last_diagnostic(&self, id: TaskId) -> String {
        self.latest
            .lock()
            .unwrap()
            .iter()
            .find(|task| task.id == id)
            .and_then(|task| task.error.as_ref())
            .map(|error| error.diagnostic.clone())
            .unwrap_or_default()
    }

    pub fn last_command_line(&self) -> String {
        self.engine.last_command_line()
    }

    pub fn wait_until_downloaded(&mut self, id: TaskId, bytes: u64, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self
                .latest
                .lock()
                .unwrap()
                .iter()
                .any(|task| task.id == id && task.actual_segments > 1 && task.downloaded >= bytes)
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for partial download"
            );
            self.poll_once(Duration::from_millis(100));
        }
    }

    pub fn shutdown_keep_files(&mut self) -> PathBuf {
        self.keep_files = true;
        self.engine.commands.send(EngineCommand::Shutdown).unwrap();
        loop {
            if let Ok(EngineEvent::ShutdownComplete) =
                self.engine.events.recv_timeout(Duration::from_millis(100))
            {
                break;
            }
        }
        self.state_path.clone()
    }

    pub fn poll_once(&mut self, timeout: Duration) {
        match self.engine.events.recv_timeout(timeout) {
            Ok(EngineEvent::Snapshot(snapshot)) => {
                *self.latest.lock().unwrap() = snapshot;
            }
            Ok(EngineEvent::BatchProxyApplied { applied, skipped }) => {
                *self.batch_proxy_result.lock().unwrap() = Some((applied, skipped));
            }
            _ => {}
        }
    }

    fn wait_for_any(&mut self, timeout: Duration) -> Vec<TaskSnapshot> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.poll_once(Duration::from_millis(100));
            let snapshot = self.latest.lock().unwrap().clone();
            if !snapshot.is_empty() {
                return snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for task"
            );
        }
    }

    pub fn state_path(&self) -> &std::path::Path {
        &self.state_path
    }

    pub fn download_dir(&self) -> &std::path::Path {
        &self.download_dir
    }
}

impl Drop for EngineHarness {
    fn drop(&mut self) {
        let _ = self.engine.commands.send(EngineCommand::Shutdown);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Ok(EngineEvent::ShutdownComplete) =
                self.engine.events.recv_timeout(Duration::from_millis(100))
            {
                break;
            }
        }
        if !self.keep_files {
            let _ = std::fs::remove_dir_all(&self.download_dir);
        }
    }
}

fn unique_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "curl-downloader-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

pub fn part_lengths(dir: &std::path::Path, id: TaskId) -> Vec<u64> {
    let work = dir.join(".curl-downloader").join(id.to_string());
    let mut lengths = std::fs::read_dir(work)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("segment-") {
                std::fs::metadata(entry.path())
                    .ok()
                    .map(|metadata| metadata.len())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    lengths.sort_unstable();
    lengths
}
