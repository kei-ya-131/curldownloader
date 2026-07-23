mod support;

use curl_downloader::model::{ProxyProtocol, TaskStatus};

#[test]
fn authenticated_proxy_password_never_leaves_allowed_stdin_path() {
    let proxy = support::TestProxy::http(b"proxied", Some("Basic YWxpY2U6czNjcmV0"));
    let mut harness = support::EngineHarness::new(2);
    let id = harness.add_with_proxy(
        "http://download.test/file",
        ProxyProtocol::Http,
        &proxy.address,
        "alice",
        "s3cret",
    );
    harness.start(id);
    harness.wait_for(
        id,
        TaskStatus::Completed,
        std::time::Duration::from_secs(60),
    );
    assert!(
        !std::fs::read_to_string(harness.state_path())
            .unwrap()
            .contains("s3cret")
    );
    assert!(!harness.last_diagnostic(id).contains("s3cret"));
    assert!(!harness.last_command_line().contains("s3cret"));
}
