mod support;

use curl_downloader::{
    controller::{ControllerCommand, LifecycleState, SharedControllerState},
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
fn enqueue_accepts_a_firefox_request_context_without_echoing_secrets() {
    let value = serde_json::json!({
        "type": "enqueue",
        "request_id": "auth-1",
        "url": "https://files.test/a.pdf",
        "filename": "a.pdf",
        "target_dir": "C:\\Downloads",
        "requested_segments": 4,
        "proxy": WireProxy::direct(),
        "request_context": {
            "headers": [{"name":"Cookie","value":"session=secret"}],
            "source_page_url":"https://app.test/page",
            "initial_url":"https://files.test/a.pdf",
            "final_url":"https://files.test/a.pdf",
            "incognito":false,
            "cookie_store_id":"firefox-default"
        }
    });
    let request: IpcRequest = serde_json::from_value(value).unwrap();
    assert!(!format!("{request:?}").contains("session=secret"));
    let IpcRequest::Enqueue {
        request_context, ..
    } = request
    else {
        panic!("expected enqueue request");
    };
    assert!(request_context.is_some());
}

#[test]
fn pipe_enqueue_reaches_the_single_download_engine() {
    let server = support::TestHttpServer::start(vec![support::Route {
        path: "/bridge.bin",
        body: b"bridge payload",
        ranges: false,
        etag: "bridge-v1",
        filename: "server.bin",
        required_headers: Vec::new(),
    }]);
    let mut harness = support::EngineHarness::new(1);
    let stop = Arc::new(AtomicBool::new(false));
    let defaults = Arc::new(Mutex::new(harness.download_dir().to_path_buf()));
    let state = SharedControllerState::new(LifecycleState::RunningHidden);
    let (controller_tx, _controller_rx) = std::sync::mpsc::channel::<ControllerCommand>();
    let pipe_suffix = format!("test-{}", std::process::id());
    unsafe { std::env::set_var("CURL_DOWNLOADER_PIPE_SUFFIX", &pipe_suffix) };
    let pipe = curl_downloader::ipc::spawn_server(
        harness.engine.commands.clone(),
        Arc::clone(&defaults),
        state,
        controller_tx,
        Arc::clone(&stop),
    );
    let target = harness.download_dir().join("bridge-target");
    std::fs::create_dir_all(&target).unwrap();

    let request = IpcRequest::Enqueue {
        request_id: "bridge-test".into(),
        url: format!("{}/bridge.bin", server.base_url),
        filename: "from-firefox.bin".into(),
        target_dir: target.to_string_lossy().into_owned(),
        requested_segments: 8,
        proxy: WireProxy::direct(),
        request_context: None,
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
    assert_eq!(*defaults.lock().unwrap(), target);
    assert_eq!(completed.requested_segments, 8);
    assert_eq!(completed.actual_segments, 1);
    assert_eq!(
        std::fs::read(completed.target_dir.join("from-firefox.bin")).unwrap(),
        b"bridge payload"
    );

    let invalid_response = curl_downloader::ipc::call_pipe(
        &IpcRequest::Enqueue {
            request_id: "invalid-segments".into(),
            url: format!("{}/bridge.bin", server.base_url),
            filename: "invalid.bin".into(),
            target_dir: target.to_string_lossy().into_owned(),
            requested_segments: 9,
            proxy: WireProxy::direct(),
            request_context: None,
        },
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(matches!(
        invalid_response,
        IpcResponse::EnqueueResult {
            ok: false,
            error: Some(ref error),
            ..
        } if error.code == "invalid_task"
    ));

    stop.store(true, Ordering::Release);
    let _ = pipe.join();
    unsafe { std::env::remove_var("CURL_DOWNLOADER_PIPE_SUFFIX") };
}
