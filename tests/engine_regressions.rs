use curl_downloader::{
    download::{merge_segments, spawn_engine},
    model::{
        CURRENT_SCHEMA_VERSION, DownloadTask, EngineCommand, EngineEvent, FileDecision,
        GlobalSettings, PersistedState, ProxySettings, RangeSupport, SegmentState, TaskOrigin,
        TaskStatus,
    },
    request_context::{self, SourceAuthorization, WireRequestContext, WireRequestHeader},
    storage,
};
use std::{
    fs,
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

fn test_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "curl-downloader-engine-regression-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

struct SignedUrlServer {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl SignedUrlServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server_requests = Arc::clone(&requests);
        let thread = thread::Builder::new()
            .name("signed-url-regression-server".into())
            .spawn(move || {
                while !server_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                            let mut request = Vec::new();
                            let mut buffer = [0_u8; 1024];
                            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                                match stream.read(&mut buffer) {
                                    Ok(0) | Err(_) => break,
                                    Ok(read) => request.extend_from_slice(&buffer[..read]),
                                }
                                if request.len() > 64 * 1024 {
                                    break;
                                }
                            }
                            let request = String::from_utf8_lossy(&request).into_owned();
                            let request_target = request
                                .lines()
                                .next()
                                .and_then(|line| line.split_whitespace().nth(1))
                                .unwrap_or("")
                                .to_owned();
                            server_requests.lock().unwrap().push(request_target.clone());
                            let method = request.lines().next().unwrap_or("");
                            let body = b"signed-payload";
                            let (status, response_body) = if request_target
                                == "/signed.bin?sig=signature-secret"
                            {
                                ("200 OK", body.as_slice())
                            } else {
                                ("404 Not Found", &[][..])
                            };
                            let content_length = if method.starts_with("HEAD ") {
                                body.len()
                            } else {
                                response_body.len()
                            };
                            let response = format!(
                                "HTTP/1.1 {status}\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
                            );
                            if stream.write_all(response.as_bytes()).is_ok()
                                && method.starts_with("GET ")
                            {
                                let _ = stream.write_all(response_body);
                            }
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
            base_url: format!("http://{address}"),
            requests,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for SignedUrlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn failed_merge_removes_incomplete_merged_part_so_retry_can_start() {
    let dir = test_dir("merge-retry");
    let part = dir.join("segment-0.part");
    let merged = dir.join("merged.part");
    fs::write(&part, b"partial").unwrap();

    assert!(merge_segments(&[part], &merged, 99).is_err());
    assert!(!merged.exists(), "a failed merge must not poison its retry");

    let first = dir.join("segment-0.part");
    fs::write(&first, b"complete").unwrap();
    merge_segments(&[first], &merged, 8).unwrap();
    assert_eq!(fs::read(&merged).unwrap(), b"complete");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn changing_source_cleans_old_partial_files_when_destination_is_unchanged() {
    let dir = test_dir("source-change");
    let state_path = dir.join("state.json");
    let work = storage::task_work_dir_for(&dir, 1);
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join("segment-0.part"), b"old! ").unwrap();
    let mut task = DownloadTask::new(1, "http://example.test/old", "file.bin".into(), dir.clone());
    task.status = TaskStatus::Paused;
    task.total_size = Some(5);
    task.range_support = RangeSupport::Supported;
    task.actual_segments = 1;
    task.segments = vec![SegmentState {
        index: 0,
        start: 0,
        end: 4,
        downloaded: 5,
        started_unix_ms: None,
        completed_unix_ms: None,
        active_millis: 0,
    }];
    storage::save_state(
        &state_path,
        &PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: GlobalSettings {
                last_download_dir: dir.clone(),
                max_curl_processes: 2,
                next_task_id: 2,
            },
            tasks: vec![task],
        },
    )
    .unwrap();
    let engine = spawn_engine(
        state_path,
        storage::load_state(&dir.join("state.json")).unwrap(),
    )
    .unwrap();
    engine
        .commands
        .send(EngineCommand::UpdateDraft {
            id: 1,
            url: "http://example.test/new".into(),
            filename: "file.bin".into(),
            target_dir: dir.clone(),
            requested_segments: 1,
            proxy: ProxySettings::default(),
        })
        .unwrap();
    wait_until_snapshot(&engine.events, |tasks| {
        tasks
            .iter()
            .any(|task| task.original_url == "http://example.test/new")
    });
    assert!(
        !work.exists(),
        "old-source bytes must not survive the metadata reset"
    );
    stop_engine(&engine.commands, &engine.events);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn changing_segment_count_discards_parts_with_old_index_ranges() {
    let dir = test_dir("segment-count");
    let state_path = dir.join("state.json");
    let work = storage::task_work_dir_for(&dir, 1);
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join("segment-0.part"), vec![b'a'; 50]).unwrap();
    fs::write(work.join("segment-1.part"), vec![b'b'; 50]).unwrap();
    let mut task = DownloadTask::new(
        1,
        "http://example.test/file",
        "file.bin".into(),
        dir.clone(),
    );
    task.status = TaskStatus::Paused;
    task.total_size = Some(100);
    task.range_support = RangeSupport::Supported;
    task.requested_segments = 2;
    task.actual_segments = 2;
    task.segments = vec![segment(0, 0, 49, 50), segment(1, 50, 99, 50)];
    storage::save_state(
        &state_path,
        &PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: GlobalSettings {
                last_download_dir: dir.clone(),
                max_curl_processes: 2,
                next_task_id: 2,
            },
            tasks: vec![task],
        },
    )
    .unwrap();
    let engine = spawn_engine(
        state_path.clone(),
        storage::load_state(&state_path).unwrap(),
    )
    .unwrap();
    engine
        .commands
        .send(EngineCommand::UpdateDraft {
            id: 1,
            url: "http://example.test/file".into(),
            filename: "file.bin".into(),
            target_dir: dir.clone(),
            requested_segments: 4,
            proxy: ProxySettings::default(),
        })
        .unwrap();
    wait_until_snapshot(&engine.events, |tasks| {
        tasks
            .iter()
            .any(|task| task.id == 1 && task.actual_segments == 1)
    });
    assert!(
        !work.exists(),
        "bytes belonging to old index ranges must be discarded"
    );
    stop_engine(&engine.commands, &engine.events);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cancel_during_segment_merge_is_handled_by_the_engine_while_worker_runs() {
    let dir = test_dir("cancel-finalization");
    let state_path = dir.join("state.json");
    let work = storage::task_work_dir_for(&dir, 1);
    fs::create_dir_all(&work).unwrap();
    let part_len = 128 * 1024 * 1024_u64;
    for index in 0..2 {
        let file = fs::File::create(work.join(format!("segment-{index}.part"))).unwrap();
        file.set_len(part_len).unwrap();
    }
    let mut task = DownloadTask::new(
        1,
        "http://example.test/file",
        "file.bin".into(),
        dir.clone(),
    );
    task.status = TaskStatus::Paused;
    task.total_size = Some(part_len * 2);
    task.range_support = RangeSupport::Supported;
    task.requested_segments = 2;
    task.actual_segments = 2;
    task.segments = vec![
        segment(0, 0, part_len - 1, part_len),
        segment(1, part_len, part_len * 2 - 1, part_len),
    ];
    storage::save_state(
        &state_path,
        &PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: GlobalSettings {
                last_download_dir: dir.clone(),
                max_curl_processes: 2,
                next_task_id: 2,
            },
            tasks: vec![task],
        },
    )
    .unwrap();
    let engine = spawn_engine(
        state_path.clone(),
        storage::load_state(&state_path).unwrap(),
    )
    .unwrap();
    engine.commands.send(EngineCommand::Start(1)).unwrap();
    wait_until_snapshot(&engine.events, |tasks| {
        tasks
            .iter()
            .any(|task| task.id == 1 && task.status == TaskStatus::Finalizing)
    });

    let (response_tx, response_rx) = std::sync::mpsc::channel();
    engine
        .commands
        .send(EngineCommand::CancelWithResponse {
            id: 1,
            response: response_tx,
        })
        .unwrap();
    response_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("the engine must stay responsive during segment merging")
        .unwrap();
    wait_until_snapshot(&engine.events, |tasks| {
        tasks
            .iter()
            .any(|task| task.id == 1 && task.status == TaskStatus::Cancelled)
    });
    assert!(
        !dir.join("file.bin").exists(),
        "cancelled finalization must not commit output"
    );
    stop_engine(&engine.commands, &engine.events);
    assert!(!work.exists(), "shutdown must wait for worker cleanup");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn successful_background_merge_replaces_only_the_approved_target_version() {
    let dir = test_dir("successful-finalization");
    let state_path = dir.join("state.json");
    let work = storage::task_work_dir_for(&dir, 1);
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join("segment-0.part"), b"abcd").unwrap();
    fs::write(work.join("segment-1.part"), b"efgh").unwrap();
    let target = dir.join("file.bin");
    fs::write(&target, b"old-version").unwrap();
    let mut task = DownloadTask::new(
        1,
        "http://example.test/file",
        "file.bin".into(),
        dir.clone(),
    );
    task.status = TaskStatus::Paused;
    task.total_size = Some(8);
    task.range_support = RangeSupport::Supported;
    task.requested_segments = 2;
    task.actual_segments = 2;
    task.segments = vec![segment(0, 0, 3, 4), segment(1, 4, 7, 4)];
    task.pending_target_fingerprint = storage::target_fingerprint_with_digest(&target).unwrap();
    task.overwrite_approval = Some(FileDecision::Overwrite);
    storage::save_state(
        &state_path,
        &PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: GlobalSettings {
                last_download_dir: dir.clone(),
                max_curl_processes: 2,
                next_task_id: 2,
            },
            tasks: vec![task],
        },
    )
    .unwrap();
    let engine = spawn_engine(
        state_path.clone(),
        storage::load_state(&state_path).unwrap(),
    )
    .unwrap();
    engine.commands.send(EngineCommand::Start(1)).unwrap();
    wait_until_snapshot(&engine.events, |tasks| {
        tasks
            .iter()
            .any(|task| task.id == 1 && task.status == TaskStatus::Completed)
    });
    assert_eq!(fs::read(&target).unwrap(), b"abcdefgh");
    stop_engine(&engine.commands, &engine.events);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn target_changed_during_merge_returns_to_overwrite_decision() {
    let dir = test_dir("target-change-during-merge");
    let state_path = dir.join("state.json");
    let work = storage::task_work_dir_for(&dir, 1);
    fs::create_dir_all(&work).unwrap();
    let part_len = 128 * 1024 * 1024_u64;
    for index in 0..2 {
        let file = fs::File::create(work.join(format!("segment-{index}.part"))).unwrap();
        file.set_len(part_len).unwrap();
    }
    let target = dir.join("file.bin");
    fs::write(&target, b"approved-version").unwrap();
    let mut task = DownloadTask::new(
        1,
        "http://example.test/file",
        "file.bin".into(),
        dir.clone(),
    );
    task.status = TaskStatus::Paused;
    task.total_size = Some(part_len * 2);
    task.range_support = RangeSupport::Supported;
    task.requested_segments = 2;
    task.actual_segments = 2;
    task.segments = vec![
        segment(0, 0, part_len - 1, part_len),
        segment(1, part_len, part_len * 2 - 1, part_len),
    ];
    task.pending_target_fingerprint = storage::target_fingerprint_with_digest(&target).unwrap();
    task.overwrite_approval = Some(FileDecision::Overwrite);
    storage::save_state(
        &state_path,
        &PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: GlobalSettings {
                last_download_dir: dir.clone(),
                max_curl_processes: 2,
                next_task_id: 2,
            },
            tasks: vec![task],
        },
    )
    .unwrap();
    let engine = spawn_engine(
        state_path.clone(),
        storage::load_state(&state_path).unwrap(),
    )
    .unwrap();
    engine.commands.send(EngineCommand::Start(1)).unwrap();
    wait_until_snapshot(&engine.events, |tasks| {
        tasks
            .iter()
            .any(|task| task.id == 1 && task.status == TaskStatus::Finalizing)
    });
    wait_for_merged_scratch(&work, std::time::Duration::from_secs(5));
    fs::write(&target, b"changed-during-merge").unwrap();
    wait_until_snapshot(&engine.events, |tasks| {
        tasks
            .iter()
            .any(|task| task.id == 1 && task.status == TaskStatus::AwaitingFileDecision)
    });
    assert_eq!(fs::read(&target).unwrap(), b"changed-during-merge");
    stop_engine(&engine.commands, &engine.events);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn segment_with_wrong_content_range_is_rejected_and_parts_are_discarded() {
    let mut server = WrongRangeServer::start();
    let dir = test_dir("wrong-content-range");
    let state_path = dir.join("state.json");
    let mut task = DownloadTask::new(
        1,
        &format!("http://{}/file.bin", server.address),
        "file.bin".into(),
        dir.clone(),
    );
    task.status = TaskStatus::Paused;
    task.total_size = Some(8);
    task.range_support = RangeSupport::Supported;
    task.requested_segments = 2;
    task.actual_segments = 2;
    task.etag = Some("\"v1\"".into());
    task.segments = vec![segment(0, 0, 3, 0), segment(1, 4, 7, 0)];
    storage::save_state(
        &state_path,
        &PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: GlobalSettings {
                last_download_dir: dir.clone(),
                max_curl_processes: 2,
                next_task_id: 2,
            },
            tasks: vec![task],
        },
    )
    .unwrap();
    let engine = spawn_engine(
        state_path.clone(),
        storage::load_state(&state_path).unwrap(),
    )
    .unwrap();
    engine.commands.send(EngineCommand::Start(1)).unwrap();
    wait_until_snapshot(&engine.events, |tasks| {
        tasks
            .iter()
            .any(|task| task.id == 1 && task.status == TaskStatus::Failed)
    });
    assert!(!dir.join("file.bin").exists());
    // The invalid response can arrive before its sibling curl process has
    // exited. Keep the engine alive until its deferred cleanup has drained.
    let work = storage::task_work_dir_for(&dir, 1);
    let cleanup_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while work.exists() && std::time::Instant::now() < cleanup_deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !work.exists(),
        "invalid response cleanup did not finish after curl processes stopped"
    );
    stop_engine(&engine.commands, &engine.events);
    assert!(!work.exists());
    server.stop();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn restart_restores_signed_protected_final_url_without_persisting_it_plaintext() {
    let server = SignedUrlServer::start();
    let dir = test_dir("signed-final-url-restart");
    let state_path = dir.join("state.json");
    let initial_url = format!("{}/landing?source-secret=landing-secret", server.base_url);
    let final_url = format!("{}/signed.bin?sig=signature-secret", server.base_url);
    let prepared = request_context::prepare(WireRequestContext {
        headers: vec![WireRequestHeader::new("Cookie", "session=valid")],
        source_page_url: Some("https://app.test/download".into()),
        initial_url: initial_url.clone(),
        final_url: final_url.clone(),
        incognito: false,
        cookie_store_id: Some("firefox-default".into()),
    })
    .unwrap();
    let mut task = DownloadTask::new_with_origin(
        1,
        &initial_url,
        "signed.bin".into(),
        dir.clone(),
        TaskOrigin::Firefox,
    );
    task.status = TaskStatus::Paused;
    task.effective_url = Some(final_url.clone());
    task.request_context = prepared.stored;
    task.authorization = prepared.authorization;
    storage::save_state(
        &state_path,
        &PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: GlobalSettings {
                last_download_dir: dir.clone(),
                max_curl_processes: 1,
                next_task_id: 2,
            },
            tasks: vec![task],
        },
    )
    .unwrap();
    let persisted_text = fs::read_to_string(&state_path).unwrap();
    assert!(!persisted_text.contains("signature-secret"));
    assert!(!persisted_text.contains("landing-secret"));

    let engine = spawn_engine(
        state_path.clone(),
        storage::load_state(&state_path).unwrap(),
    )
    .unwrap();
    engine.commands.send(EngineCommand::Start(1)).unwrap();
    wait_until_snapshot(&engine.events, |tasks| {
        tasks
            .iter()
            .any(|task| task.id == 1 && task.status == TaskStatus::Completed)
    });

    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| { request == "/signed.bin?sig=signature-secret" }),
        "restart should request the encrypted context's signed final URL"
    );
    assert!(
        !server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.contains("landing-secret")),
        "restart must not replay the redacted initial URL"
    );
    assert_eq!(fs::read(dir.join("signed.bin")).unwrap(), b"signed-payload");

    stop_engine(&engine.commands, &engine.events);
    let persisted_text = fs::read_to_string(&state_path).unwrap();
    assert!(!persisted_text.contains("signature-secret"));
    assert!(!persisted_text.contains("landing-secret"));
    let stopped = storage::load_state(&state_path).unwrap();
    let task = stopped.tasks.iter().find(|task| task.id == 1).unwrap();
    assert_eq!(task.authorization, SourceAuthorization::ProtectedCleared);
    assert!(task.request_context.was_protected);
    assert!(task.request_context.encrypted.is_none());
    assert!(!task.original_url.contains("signature-secret"));
    assert!(
        !task
            .effective_url
            .as_deref()
            .unwrap_or("")
            .contains("signature-secret")
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn startup_migrates_terminal_protected_urls_without_context_bytes() {
    let dir = test_dir("terminal-protected-url-migration");
    let state_path = dir.join("state.json");
    let secret_url = "https://files.test/item?sig=legacy-signature-secret";
    let mut task = DownloadTask::new(1, secret_url, "item.bin".into(), dir.clone());
    task.status = TaskStatus::Completed;
    task.effective_url = Some(secret_url.into());
    task.authorization = SourceAuthorization::ProtectedCleared;
    task.request_context.was_protected = true;
    let old_state = PersistedState {
        schema_version: CURRENT_SCHEMA_VERSION,
        settings: GlobalSettings {
            last_download_dir: dir.clone(),
            max_curl_processes: 1,
            next_task_id: 2,
        },
        tasks: vec![task],
    };
    // Simulate an older state file that predates terminal URL migration. Do
    // not use save_state here, since it redacts protected URL copies itself.
    fs::write(&state_path, serde_json::to_vec_pretty(&old_state).unwrap()).unwrap();
    assert!(
        fs::read_to_string(&state_path)
            .unwrap()
            .contains("legacy-signature-secret")
    );

    let engine = spawn_engine(
        state_path.clone(),
        storage::load_state(&state_path).unwrap(),
    )
    .unwrap();
    let observed = Arc::new(Mutex::new(None));
    let snapshot = Arc::clone(&observed);
    wait_until_snapshot(&engine.events, |tasks| {
        if let Some(task) = tasks
            .iter()
            .find(|task| task.id == 1 && task.status == TaskStatus::Completed)
        {
            *snapshot.lock().unwrap() =
                Some((task.original_url.clone(), task.effective_url.clone()));
            true
        } else {
            false
        }
    });
    let (original_url, effective_url) = observed.lock().unwrap().clone().unwrap();
    assert!(!original_url.contains("legacy-signature-secret"));
    assert!(
        !effective_url
            .as_deref()
            .unwrap_or("")
            .contains("legacy-signature-secret")
    );
    assert!(
        !fs::read_to_string(&state_path)
            .unwrap()
            .contains("legacy-signature-secret")
    );

    stop_engine(&engine.commands, &engine.events);
    assert!(
        !fs::read_to_string(&state_path)
            .unwrap()
            .contains("legacy-signature-secret")
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn resumed_segment_validates_the_adjusted_range_and_completes() {
    let mut server = ValidRangeServer::start();
    let dir = test_dir("resume-range");
    let state_path = dir.join("state.json");
    let work = storage::task_work_dir_for(&dir, 1);
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join("segment-0.part"), b"ab").unwrap();
    let mut task = DownloadTask::new(
        1,
        &format!("http://{}/file.bin", server.address),
        "file.bin".into(),
        dir.clone(),
    );
    task.status = TaskStatus::Paused;
    task.total_size = Some(8);
    task.range_support = RangeSupport::Supported;
    task.requested_segments = 2;
    task.actual_segments = 2;
    task.etag = Some("\"v1\"".into());
    task.segments = vec![segment(0, 0, 3, 2), segment(1, 4, 7, 0)];
    storage::save_state(
        &state_path,
        &PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: GlobalSettings {
                last_download_dir: dir.clone(),
                max_curl_processes: 1,
                next_task_id: 2,
            },
            tasks: vec![task],
        },
    )
    .unwrap();
    let engine = spawn_engine(
        state_path.clone(),
        storage::load_state(&state_path).unwrap(),
    )
    .unwrap();
    engine.commands.send(EngineCommand::Start(1)).unwrap();
    wait_until_snapshot(&engine.events, |tasks| {
        tasks
            .iter()
            .any(|task| task.id == 1 && task.status == TaskStatus::Completed)
    });
    assert_eq!(fs::read(dir.join("file.bin")).unwrap(), b"abcdefgh");
    assert_eq!(
        server.requests.lock().unwrap().as_slice(),
        ["bytes=2-3", "bytes=4-7"]
    );
    stop_engine(&engine.commands, &engine.events);
    server.stop();
    fs::remove_dir_all(dir).unwrap();
}

struct WrongRangeServer {
    address: String,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl WrongRangeServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => serve_wrong_range(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for WrongRangeServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve_wrong_range(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request = Vec::new();
    let mut buffer = [0; 512];
    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
        let Ok(count) = stream.read(&mut buffer) else {
            return;
        };
        if count == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..count]);
    }
    let text = String::from_utf8_lossy(&request);
    let Some((start, end)) = text.lines().find_map(|line| {
        let value = line.strip_prefix("Range: bytes=")?;
        let (start, end) = value.split_once('-')?;
        Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
    }) else {
        return;
    };
    let body = b"abcdefgh";
    let length = end - start + 1;
    let wrong_start = start + 1;
    let wrong_end = end + 1;
    let headers = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {length}\r\nContent-Range: bytes {wrong_start}-{wrong_end}/8\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(headers.as_bytes()).is_ok() {
        let _ = stream.write_all(&body[start..=end]);
    }
}

struct ValidRangeServer {
    address: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ValidRangeServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => serve_valid_range(stream, &thread_requests),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        Self {
            address,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ValidRangeServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve_valid_range(mut stream: TcpStream, requests: &Mutex<Vec<String>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request = Vec::new();
    let mut buffer = [0; 512];
    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
        let Ok(count) = stream.read(&mut buffer) else {
            return;
        };
        if count == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..count]);
    }
    let text = String::from_utf8_lossy(&request);
    let Some((start, end)) = text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("range") {
            return None;
        }
        let value = value.trim().strip_prefix("bytes=")?;
        let (start, end) = value.split_once('-')?;
        Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
    }) else {
        return;
    };
    requests
        .lock()
        .unwrap()
        .push(format!("bytes={start}-{end}"));
    let body = b"abcdefgh";
    let chunk = &body[start..=end];
    let headers = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/8\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n",
        chunk.len()
    );
    if stream.write_all(headers.as_bytes()).is_ok() {
        let _ = stream.write_all(chunk);
    }
}

fn segment(index: u8, start: u64, end: u64, downloaded: u64) -> SegmentState {
    SegmentState {
        index,
        start,
        end,
        downloaded,
        started_unix_ms: None,
        completed_unix_ms: None,
        active_millis: 0,
    }
}

fn wait_until_snapshot<F>(events: &std::sync::mpsc::Receiver<EngineEvent>, predicate: F)
where
    F: Fn(&[curl_downloader::model::TaskSnapshot]) -> bool,
{
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "engine snapshot timed out"
        );
        if let Ok(EngineEvent::Snapshot(tasks)) =
            events.recv_timeout(std::time::Duration::from_millis(100))
            && predicate(&tasks)
        {
            return;
        }
    }
}

fn stop_engine(
    commands: &std::sync::mpsc::Sender<EngineCommand>,
    events: &std::sync::mpsc::Receiver<EngineEvent>,
) {
    commands.send(EngineCommand::Shutdown).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!remaining.is_zero(), "engine shutdown timed out");
        match events.recv_timeout(remaining.min(std::time::Duration::from_millis(250))) {
            Ok(EngineEvent::ShutdownComplete) => return,
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("engine event channel disconnected before shutdown completed")
            }
        }
    }
}

fn wait_for_merged_scratch(work: &std::path::Path, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let found = fs::read_dir(work)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with("merged-"));
        if found {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "merge worker did not start"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
