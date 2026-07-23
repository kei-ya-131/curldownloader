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

#[test]
fn socks5_and_socks5h_select_local_or_remote_dns() {
    let proxy = support::TestProxy::socks5(b"ok");
    for protocol in [ProxyProtocol::Socks5, ProxyProtocol::Socks5h] {
        let mut harness = support::EngineHarness::new(1);
        let id = harness.add_with_proxy("http://localhost/file", protocol, &proxy.address, "", "");
        harness.start(id);
        let completed = harness.wait_for(
            id,
            TaskStatus::Completed,
            std::time::Duration::from_secs(60),
        );
        assert_eq!(
            std::fs::read(completed.target_dir.join("file")).unwrap(),
            b"ok"
        );
    }
    let atypes = proxy.recorded_atyp();
    assert_eq!(atypes.len(), 4);
    assert!(atypes[..2].iter().all(|atype| matches!(atype, 0x01 | 0x04)));
    assert!(atypes[2..].iter().all(|atype| *atype == 0x03));
}
