mod support;

use curl_downloader::model::{CurlSource, EngineCommand, NewTask, TaskStatus};

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
    assert!(harness.max_observed_processes() <= 4);
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
