use eframe::egui;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
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
    startup_hide_cancel: Arc<AtomicBool>,
}

impl EguiMainWindow {
    pub fn new(context: egui::Context) -> Arc<Self> {
        Self::new_with_startup_hide(context, false)
    }

    pub fn new_minimized(context: egui::Context) -> Arc<Self> {
        Self::new_with_startup_hide(context, true)
    }

    fn new_with_startup_hide(context: egui::Context, minimized: bool) -> Arc<Self> {
        let startup_hide_cancel = Arc::new(AtomicBool::new(false));
        if minimized {
            let cancel = Arc::clone(&startup_hide_cancel);
            let _ = thread::Builder::new()
                .name("curl-downloader-startup-hide".into())
                .spawn(move || {
                    for _ in 0..100 {
                        if cancel.load(Ordering::Acquire) {
                            return;
                        }
                        set_main_window_visibility(false);
                        thread::sleep(Duration::from_millis(100));
                    }
                });
        }
        Arc::new(Self {
            context,
            startup_hide_cancel,
        })
    }
}

impl MainWindowControl for EguiMainWindow {
    fn show_and_focus(&self) {
        self.startup_hide_cancel.store(true, Ordering::Release);
        apply_directives(&self.context, &show_directives());
        focus_existing_main_window_async();
    }

    fn hide(&self) {
        apply_directives(&self.context, &hide_directives());
    }

    fn request_close(&self) {
        apply_directives(&self.context, &close_directives());
    }
}

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
                EnumWindows, FindWindowW, GW_OWNER, GetClassNameW, GetWindow, GetWindowTextW,
                GetWindowThreadProcessId,
            },
        };
        use windows_sys::core::BOOL;

        let main_title: Vec<u16> = "Curl Downloader\0".encode_utf16().collect();
        let direct_window = unsafe { FindWindowW(std::ptr::null(), main_title.as_ptr()) };
        if !direct_window.is_null() {
            let mut process_id = 0u32;
            unsafe {
                GetWindowThreadProcessId(direct_window, &mut process_id);
            }
            if window_matches_process(process_id, self.process_id)
                && unsafe { GetWindow(direct_window, GW_OWNER).is_null() }
            {
                return Some(direct_window);
            }
        }

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
            let mut class_name = [0u16; 256];
            let class_length =
                unsafe { GetClassNameW(hwnd, class_name.as_mut_ptr(), class_name.len() as i32) };
            let class_name = String::from_utf16_lossy(&class_name[..class_length as usize]);
            let is_auxiliary_window = matches!(
                class_name.as_str(),
                "CurlDownloaderTrayWindow" | "NVOpenGLPbuffer"
            );
            let is_candidate = window_matches_process(process_id, search.process_id)
                && unsafe { GetWindow(hwnd, GW_OWNER) }.is_null()
                && !is_auxiliary_window;
            if is_candidate {
                if search.window.is_null() {
                    search.window = hwnd;
                }
                if is_main_window {
                    search.window = hwnd;
                    return 0;
                }
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
            SW_SHOW, SetForegroundWindow, ShowWindow,
        };
        if let Some(window) = self.find() {
            unsafe {
                ShowWindow(window, SW_SHOW);
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
static FOCUS_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
pub fn focus_existing_main_window_async() {
    if FOCUS_IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return;
    }
    if thread::Builder::new()
        .name("curl-downloader-window-focus".into())
        .spawn(|| {
            focus_existing_main_window();
            FOCUS_IN_PROGRESS.store(false, Ordering::Release);
        })
        .is_err()
    {
        FOCUS_IN_PROGRESS.store(false, Ordering::Release);
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

#[cfg(not(windows))]
pub fn focus_existing_main_window_async() {}

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
