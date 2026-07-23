mod support;

use curl_downloader::model::TaskStatus;

#[test]
fn batch_additions_remain_queued_until_explicitly_started() {
    let server = support::TestHttpServer::start(vec![
        support::Route {
            path: "/one.bin",
            body: b"one",
            ranges: true,
            etag: "one-v1",
            filename: "one.bin",
        },
        support::Route {
            path: "/two.bin",
            body: b"two",
            ranges: true,
            etag: "two-v1",
            filename: "two.bin",
        },
    ]);
    let mut harness = support::EngineHarness::new(1);
    let urls = vec![
        format!("{}/one.bin", server.base_url),
        format!("{}/two.bin", server.base_url),
    ];

    let queued = harness.add_batch(&urls);
    assert_eq!(queued.len(), 2);
    assert!(queued.iter().all(|task| task.status == TaskStatus::Queued));
    assert!(queued.iter().all(|task| task.total_size.is_none()));

    harness.start(queued[0].id);
    let first = harness.wait_for(
        queued[0].id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );
    assert_eq!(
        std::fs::read(first.target_dir.join(first.filename)).unwrap(),
        b"one"
    );

    harness.start(queued[1].id);
    let second = harness.wait_for(
        queued[1].id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );
    assert_eq!(
        std::fs::read(second.target_dir.join(second.filename)).unwrap(),
        b"two"
    );
}
