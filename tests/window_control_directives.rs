use curl_downloader::window_control::{
    WindowDirective, close_directives, hide_directives, show_directives,
};

#[test]
fn show_directives_restore_and_focus_the_existing_viewport() {
    assert_eq!(
        show_directives(),
        [
            WindowDirective::Visible(true),
            WindowDirective::Minimized(false),
            WindowDirective::Focus,
        ]
    );
}

#[test]
fn hide_and_close_have_distinct_viewport_commands() {
    assert_eq!(hide_directives(), [WindowDirective::Visible(false)]);
    assert_eq!(close_directives(), [WindowDirective::Close]);
}
