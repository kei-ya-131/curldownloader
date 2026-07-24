mod support;

use curl_downloader::{
    ipc::{IpcRequest, IpcResponse, WireProxy},
    model::TaskStatus,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[test]
fn pipe_enqueue_reaches_the_single_download_engine() {
    let server = support::TestHttpServer::start(vec![support::Route {
        path: "/bridge.bin",
        body: b"bridge payload",
        ranges: false,
        etag: "bridge-v1",
        filename: "server.bin",
    }]);
    let mut harness = support::EngineHarness::new(1);
    let stop = Arc::new(AtomicBool::new(false));
    let defaults = Arc::new(Mutex::new(harness.download_dir().to_path_buf()));
    let pipe = curl_downloader::ipc::spawn_server(
        harness.engine.commands.clone(),
        defaults,
        Arc::clone(&stop),
    );
    let target = harness.download_dir().join("bridge-target");
    std::fs::create_dir_all(&target).unwrap();

    let request = IpcRequest::Enqueue {
        request_id: "bridge-test".into(),
        url: format!("{}/bridge.bin", server.base_url),
        filename: "from-firefox.bin".into(),
        target_dir: target.to_string_lossy().into_owned(),
        proxy: WireProxy::direct(),
    };
    let response = curl_downloader::ipc::call_pipe(&request, Duration::from_secs(5)).unwrap();
    let IpcResponse::EnqueueResult {
        ok: true,
        task_id: Some(id),
        ..
    } = response
    else {
        panic!("unexpected response: {response:?}");
    };

    let completed = harness.wait_for(id, TaskStatus::Completed, Duration::from_secs(60));
    assert_eq!(
        std::fs::read(completed.target_dir.join("from-firefox.bin")).unwrap(),
        b"bridge payload"
    );

    stop.store(true, Ordering::Release);
    let _ = pipe.join();
}
