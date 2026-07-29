use eframe::egui;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowDirective {
    Visible(bool),
    Minimized(bool),
    Focus,
    Close,
}

pub fn show_directives() -> [WindowDirective; 3] {
    [
        WindowDirective::Visible(true),
        WindowDirective::Minimized(false),
        WindowDirective::Focus,
    ]
}

pub fn hide_directives() -> [WindowDirective; 1] {
    [WindowDirective::Visible(false)]
}

pub fn close_directives() -> [WindowDirective; 1] {
    [WindowDirective::Close]
}

fn apply_directives(context: &egui::Context, directives: &[WindowDirective]) {
    for directive in directives {
        let command = match directive {
            WindowDirective::Visible(value) => egui::ViewportCommand::Visible(*value),
            WindowDirective::Minimized(value) => egui::ViewportCommand::Minimized(*value),
            WindowDirective::Focus => egui::ViewportCommand::Focus,
            WindowDirective::Close => egui::ViewportCommand::Close,
        };
        context.send_viewport_cmd(command);
    }
    context.request_repaint();
}

pub struct EguiMainWindow {
    context: egui::Context,
}

impl EguiMainWindow {
    pub fn new(context: egui::Context) -> Arc<Self> {
        Arc::new(Self { context })
    }
}

impl MainWindowControl for EguiMainWindow {
    fn show_and_focus(&self) {
        apply_directives(&self.context, &show_directives());
    }

    fn hide(&self) {
        apply_directives(&self.context, &hide_directives());
    }

    fn request_close(&self) {
        apply_directives(&self.context, &close_directives());
    }
}
use std::sync::Arc;

pub trait MainWindowControl: Send + Sync {
    fn show_and_focus(&self);
    fn hide(&self);
    fn request_close(&self);
}

fn window_matches_process(window_process_id: u32, target_process_id: Option<u32>) -> bool {
    match target_process_id {
        Some(target_process_id) => window_process_id == target_process_id,
        None => true,
    }
}

#[cfg(windows)]
pub struct ProcessMainWindow {
    process_id: Option<u32>,
}

#[cfg(windows)]
impl ProcessMainWindow {
    pub fn current() -> Arc<Self> {
        use windows_sys::Win32::System::Threading::GetCurrentProcessId;
        Arc::new(Self {
            process_id: Some(unsafe { GetCurrentProcessId() }),
        })
    }

    fn find(&self) -> Option<windows_sys::Win32::Foundation::HWND> {
        use windows_sys::Win32::{
            Foundation::{HWND, LPARAM},
            UI::WindowsAndMessaging::{
                EnumWindows, GW_OWNER, GetWindow, GetWindowTextW, GetWindowThreadProcessId,
            },
        };
        use windows_sys::core::BOOL;

        struct Search {
            process_id: Option<u32>,
            window: HWND,
        }

        unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let search = unsafe { &mut *(lparam as *mut Search) };
            let mut process_id = 0u32;
            unsafe {
                GetWindowThreadProcessId(hwnd, &mut process_id);
            }
            let mut title = [0u16; 256];
            let title_length =
                unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
            let is_main_window = title_length > 0
                && String::from_utf16_lossy(&title[..title_length as usize]) == "Curl Downloader";
            if window_matches_process(process_id, search.process_id)
                && unsafe { GetWindow(hwnd, GW_OWNER) }.is_null()
                && is_main_window
            {
                search.window = hwnd;
                return 0;
            }
            1
        }

        let mut search = Search {
            process_id: self.process_id,
            window: std::ptr::null_mut(),
        };
        unsafe {
            EnumWindows(
                Some(callback),
                (&mut search as *mut Search).cast::<()>() as LPARAM,
            );
        }
        (!search.window.is_null()).then_some(search.window)
    }
}

#[cfg(windows)]
impl MainWindowControl for ProcessMainWindow {
    fn show_and_focus(&self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SW_RESTORE, SetForegroundWindow, ShowWindow,
        };
        if let Some(window) = self.find() {
            unsafe {
                ShowWindow(window, SW_RESTORE);
                SetForegroundWindow(window);
            }
        }
    }

    fn hide(&self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
        if let Some(window) = self.find() {
            unsafe {
                ShowWindow(window, SW_HIDE);
            }
        }
    }

    fn request_close(&self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
        if let Some(window) = self.find() {
            unsafe {
                PostMessageW(window, WM_CLOSE, 0, 0);
            }
        }
    }
}

#[cfg(windows)]
pub fn set_main_window_visibility(visible: bool) {
    let window = ProcessMainWindow::current();
    if visible {
        window.show_and_focus();
    } else {
        window.hide();
    }
}

#[cfg(windows)]
pub fn focus_existing_main_window() {
    let window = ProcessMainWindow { process_id: None };
    window.show_and_focus();
}

#[cfg(not(windows))]
pub struct ProcessMainWindow;

#[cfg(not(windows))]
impl ProcessMainWindow {
    pub fn current() -> Arc<Self> {
        Arc::new(Self)
    }
}

#[cfg(not(windows))]
impl MainWindowControl for ProcessMainWindow {
    fn show_and_focus(&self) {}
    fn hide(&self) {}
    fn request_close(&self) {}
}

#[cfg(not(windows))]
pub fn set_main_window_visibility(_visible: bool) {}

#[cfg(not(windows))]
pub fn focus_existing_main_window() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_window_filter_rejects_another_process() {
        assert!(window_matches_process(42, Some(42)));
        assert!(!window_matches_process(41, Some(42)));
        assert!(window_matches_process(41, None));
    }
}
