mod support;

use curl_downloader::model::{
    CurlSource, EngineCommand, NewTask, ProxySettings, TaskError, TaskStatus,
};

#[test]
fn curl_runtime_is_lazy_until_download_starts() {
    let server = support::TestHttpServer::start(vec![support::Route {
        path: "/lazy.bin",
        body: b"lazy",
        ranges: false,
        etag: "lazy-v1",
        filename: "lazy.bin",
    }]);
    let mut harness = support::EngineHarness::new(1);
    let queued = harness.add_batch(&[format!("{}/lazy.bin", server.base_url)])[0].clone();

    assert_eq!(queued.curl_source, CurlSource::NotStarted);

    harness.start(queued.id);
    let completed = harness.wait_for(
        queued.id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );
    assert!(matches!(
        completed.curl_source,
        CurlSource::Local | CurlSource::Embedded
    ));
}

#[test]
fn successful_download_clears_previous_error() {
    let server = support::TestHttpServer::start(vec![support::Route {
        path: "/retry.bin",
        body: b"retry",
        ranges: false,
        etag: "retry-v1",
        filename: "retry.bin",
    }]);
    let mut harness = support::EngineHarness::new(1);
    let queued = harness
        .add_batch(&[format!("{}/retry.bin", server.base_url)])
        .remove(0);
    let state_path = harness.shutdown_keep_files();
    let mut state = curl_downloader::storage::load_state(&state_path).unwrap();
    state.tasks[0].status = TaskStatus::Failed;
    state.tasks[0].last_error = Some(TaskError {
        kind: curl_downloader::model::ErrorKind::Network,
        summary: "無法取得來源資訊".into(),
        code: Some(35),
        diagnostic: "curl: (35) Recv failure: Connection was reset".into(),
        action: "檢查網址或 Proxy 後重試".into(),
    });
    curl_downloader::storage::save_state(&state_path, &state).unwrap();

    let mut restarted =
        support::EngineHarness::from_state(state_path, harness.download_dir().to_path_buf());
    restarted.start(queued.id);
    let completed = restarted.wait_for(
        queued.id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );

    assert!(completed.error.is_none());
}

#[test]
fn applies_one_proxy_configuration_to_multiple_queued_tasks() {
    let server = support::TestHttpServer::start(vec![
        support::Route {
            path: "/first.bin",
            body: b"first",
            ranges: false,
            etag: "first-v1",
            filename: "first.bin",
        },
        support::Route {
            path: "/second.bin",
            body: b"second",
            ranges: false,
            etag: "second-v1",
            filename: "second.bin",
        },
    ]);
    let mut harness = support::EngineHarness::new(1);
    let tasks = harness.add_batch(&[
        format!("{}/first.bin", server.base_url),
        format!("{}/second.bin", server.base_url),
    ]);
    let ids = tasks.iter().map(|task| task.id).collect::<Vec<_>>();
    let proxy = ProxySettings {
        enabled: true,
        host: "127.0.0.1".into(),
        port: 8080,
        ..ProxySettings::default()
    };

    harness
        .engine
        .commands
        .send(EngineCommand::UpdateProxy { ids, proxy })
        .unwrap();
    let updated = harness.wait_for_proxy(&[tasks[0].id, tasks[1].id], "127.0.0.1");

    assert_eq!(updated.len(), 2);
    assert!(updated.iter().all(|task| task.proxy.enabled));
    assert!(updated.iter().all(|task| task.proxy.port == 8080));
}

#[test]
fn reports_applied_and_skipped_tasks_for_mixed_bulk_proxy_update() {
    let server = support::TestHttpServer::start(vec![
        support::Route {
            path: "/completed.bin",
            body: b"completed",
            ranges: false,
            etag: "completed-v1",
            filename: "completed.bin",
        },
        support::Route {
            path: "/queued.bin",
            body: b"queued",
            ranges: false,
            etag: "queued-v1",
            filename: "queued.bin",
        },
    ]);
    let mut harness = support::EngineHarness::new(1);
    let tasks = harness.add_batch(&[
        format!("{}/completed.bin", server.base_url),
        format!("{}/queued.bin", server.base_url),
    ]);
    harness.start(tasks[0].id);
    let completed = harness.wait_for(
        tasks[0].id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );
    assert_eq!(completed.filename, "completed.bin");

    let proxy = ProxySettings {
        enabled: true,
        host: "127.0.0.1".into(),
        port: 8080,
        ..ProxySettings::default()
    };
    harness
        .engine
        .commands
        .send(EngineCommand::UpdateProxy {
            ids: vec![tasks[0].id, tasks[1].id],
            proxy,
        })
        .unwrap();

    let updated = harness.wait_for_proxy(&[tasks[1].id], "127.0.0.1");
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].id, tasks[1].id);
    assert_eq!(harness.wait_for_batch_proxy_result(), (1, 1));
}

#[test]
fn downloads_four_ranges_and_merges_exact_bytes() {
    let server = support::TestHttpServer::start(vec![support::Route {
        path: "/range.bin",
        body: b"abcdefghijklmnopqrstuvwxyz",
        ranges: true,
        etag: "v1",
        filename: "range.bin",
    }]);
    let mut harness = support::EngineHarness::new(4);
    let id = harness.add_and_start(format!("{}/range.bin", server.base_url), 4);
    let completed = harness.wait_for(
        id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );
    assert_eq!(
        std::fs::read(completed.target_dir.join("range.bin")).unwrap(),
        b"abcdefghijklmnopqrstuvwxyz"
    );
    assert_eq!(completed.segments.len(), 4);
    for segment in &completed.segments {
        assert_eq!(
            segment.downloaded,
            segment.end.saturating_sub(segment.start).saturating_add(1)
        );
        assert!(segment.started_unix_ms.is_some());
        assert!(segment.completed_unix_ms.is_some());
        assert!(segment.active_millis > 0);
        assert!(!segment.active);
    }
    assert!(harness.max_observed_processes() <= 4);

    let state_path = harness.shutdown_keep_files();
    let persisted = curl_downloader::storage::load_state(&state_path).unwrap();
    let persisted_task = persisted.tasks.iter().find(|task| task.id == id).unwrap();
    assert_eq!(persisted_task.segments.len(), 4);
    assert!(
        persisted_task
            .segments
            .iter()
            .all(|segment| segment.completed_unix_ms.is_some())
    );
}

#[test]
fn server_without_ranges_falls_back_to_single_stream() {
    let server = support::TestHttpServer::start(vec![support::Route {
        path: "/single.bin",
        body: b"single",
        ranges: false,
        etag: "v1",
        filename: "single.bin",
    }]);
    let mut harness = support::EngineHarness::new(4);
    let id = harness.add_and_start(format!("{}/single.bin", server.base_url), 4);
    let completed = harness.wait_for(
        id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );
    assert_eq!(completed.actual_segments, 1);
    assert_eq!(completed.segments.len(), 1);
    assert!(completed.segments[0].started_unix_ms.is_some());
    assert!(completed.segments[0].completed_unix_ms.is_some());
    assert!(completed.segments[0].active_millis > 0);
}

#[test]
fn segment_active_time_excludes_pause_and_accumulates_after_resume() {
    static PAUSE_BODY: [u8; 512] = [b'x'; 512];
    let server = support::TestHttpServer::start_slow(&PAUSE_BODY, 100);
    let mut harness = support::EngineHarness::new(2);
    let id = harness.add_and_start(format!("{}/slow.bin", server.base_url), 2);
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    harness
        .engine
        .commands
        .send(EngineCommand::Pause(id))
        .unwrap();
    let _paused = harness.wait_for(id, TaskStatus::Paused, std::time::Duration::from_secs(5));
    let paused_segment =
        harness.wait_for_segment(id, std::time::Duration::from_secs(5), |segment| {
            segment.started_unix_ms.is_some() && segment.active_millis > 0
        });

    std::thread::sleep(std::time::Duration::from_millis(350));
    harness.poll_once(std::time::Duration::from_millis(700));
    let still_paused = harness.wait_for_segment(id, std::time::Duration::from_secs(2), |segment| {
        segment.index == paused_segment.index
    });
    assert_eq!(still_paused.active_millis, paused_segment.active_millis);

    harness.resume(id);
    let completed = harness.wait_for(
        id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(30),
    );
    let completed_segment = completed
        .segments
        .iter()
        .find(|segment| segment.index == paused_segment.index)
        .unwrap();
    assert_eq!(
        completed_segment.started_unix_ms,
        paused_segment.started_unix_ms
    );
    assert!(completed_segment.active_millis > paused_segment.active_millis);
}
#[test]
fn shutdown_then_restart_resumes_without_redownloading_prefix() {
    let server =
        support::TestHttpServer::start_slow(b"0123456789abcdefghijklmnopqrstuvwxyz", 1_000);
    let mut harness = support::EngineHarness::new(1);
    let id = harness.add_and_start(format!("{}/slow.bin", server.base_url), 2);
    harness.wait_until_downloaded(id, 8, std::time::Duration::from_secs(15));
    let state_path = harness.shutdown_keep_files();
    let before = support::part_lengths(harness.download_dir(), id);
    assert!(before.iter().any(|length| *length > 0));
    let persisted = curl_downloader::storage::load_state(&state_path).unwrap();
    let stopped = persisted.tasks.iter().find(|task| task.id == id).unwrap();
    assert_eq!(stopped.status, TaskStatus::Paused);
    assert!(
        support::part_lengths(harness.download_dir(), id)
            .iter()
            .any(|length| *length > 0)
    );

    let mut restarted =
        support::EngineHarness::from_state(state_path, harness.download_dir().to_path_buf());
    restarted.resume(id);
    let completed = restarted.wait_for(
        id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );
    assert_eq!(
        std::fs::read(completed.target_dir.join("slow.bin")).unwrap(),
        b"0123456789abcdefghijklmnopqrstuvwxyz"
    );
}

#[test]
fn completed_history_can_be_cleared_without_removing_target_file() {
    let server = support::TestHttpServer::start(vec![support::Route {
        path: "/history.bin",
        body: b"history",
        ranges: true,
        etag: "history-v1",
        filename: "history.bin",
    }]);
    let mut harness = support::EngineHarness::new(1);
    let completed_id = harness.add_and_start(format!("{}/history.bin", server.base_url), 1);
    let completed = harness.wait_for(
        completed_id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );
    let target = completed.target_dir.join(&completed.filename);

    harness
        .engine
        .commands
        .send(EngineCommand::Add(NewTask {
            url: format!("{}/history.bin", server.base_url),
            target_dir: harness.download_dir().to_path_buf(),
        }))
        .unwrap();
    let cancelled = harness.wait_for_count(2, std::time::Duration::from_secs(5));
    harness
        .engine
        .commands
        .send(EngineCommand::Cancel(cancelled[1].id))
        .unwrap();
    harness.wait_for(
        cancelled[1].id,
        TaskStatus::Cancelled,
        std::time::Duration::from_secs(5),
    );

    harness
        .engine
        .commands
        .send(EngineCommand::ClearHistory)
        .unwrap();
    harness.wait_for_empty(std::time::Duration::from_secs(5));
    assert!(target.is_file());
}

#[test]
fn completed_progress_remains_full_after_work_dir_cleanup() {
    let server = support::TestHttpServer::start(vec![support::Route {
        path: "/progress.bin",
        body: b"progress",
        ranges: true,
        etag: "progress-v1",
        filename: "progress.bin",
    }]);
    let mut harness = support::EngineHarness::new(1);
    let id = harness.add_and_start(format!("{}/progress.bin", server.base_url), 1);
    let completed = harness.wait_for(
        id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );
    std::thread::sleep(std::time::Duration::from_millis(1_200));
    for _ in 0..4 {
        harness.poll_once(std::time::Duration::from_millis(100));
    }
    let refreshed = harness.wait_for(id, TaskStatus::Completed, std::time::Duration::from_secs(5));
    assert_eq!(refreshed.downloaded, completed.total_size.unwrap());
    assert_eq!(refreshed.total_size, Some(8));
}

#[test]
fn configured_task_uses_external_filename_directory_and_proxy() {
    let server = support::TestHttpServer::start(vec![support::Route {
        path: "/payload.bin",
        body: b"configured payload",
        ranges: false,
        etag: "configured-v1",
        filename: "server-name.bin",
    }]);
    let mut harness = support::EngineHarness::new(1);
    let target_dir = harness.download_dir().join("external-target");
    std::fs::create_dir_all(&target_dir).unwrap();

    let id = harness.add_configured(
        format!("{}/payload.bin", server.base_url),
        "renamed-from-firefox.bin".into(),
        target_dir.clone(),
        ProxySettings::default(),
    );
    let completed = harness.wait_for(
        id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );

    assert_eq!(completed.target_dir, target_dir);
    assert_eq!(completed.filename, "renamed-from-firefox.bin");
    assert_eq!(
        std::fs::read(target_dir.join("renamed-from-firefox.bin")).unwrap(),
        b"configured payload"
    );
    let saved = curl_downloader::storage::load_state(harness.state_path()).unwrap();
    assert_eq!(saved.settings.last_download_dir, target_dir);
}
