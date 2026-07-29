use curl_downloader::controller::{LifecycleState, SharedControllerState};
use std::time::Duration;

#[test]
fn controller_is_not_ready_until_engine_and_ui_are_ready() {
    let state = SharedControllerState::new(LifecycleState::Starting);
    state.mark_engine_ready();
    assert!(!state.wait_ready(Duration::from_millis(1)));
    state.mark_ui_ready();
    assert!(state.wait_ready(Duration::from_millis(1)));
}
