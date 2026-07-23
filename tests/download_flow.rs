mod support;

use curl_downloader::model::TaskStatus;

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
