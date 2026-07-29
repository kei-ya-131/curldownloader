use std::sync::mpsc::Receiver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowVisibility {
    Visible,
    HiddenToTray,
}

pub fn startup_visibility(start_minimized: bool) -> WindowVisibility {
    if start_minimized {
        WindowVisibility::HiddenToTray
    } else {
        WindowVisibility::Visible
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    ShowWindow,
    CloseWindow,
}

#[cfg(windows)]
mod windows_impl {
    use super::TrayEvent;
    use std::{
        mem::size_of,
        ptr,
        sync::mpsc::{Receiver, Sender, SyncSender, sync_channel},
        thread::{self, JoinHandle},
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::{
            ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
            WPARAM,
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreatePopupMenu,
                DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
                GetCursorPos, GetMessageW, HMENU, IDI_APPLICATION, LoadIconW, MF_STRING,
                RegisterClassW, SetForegroundWindow, SetWindowLongPtrW, TPM_BOTTOMALIGN,
                TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage,
                WM_APP, WM_LBUTTONDBLCLK, WM_NCCREATE, WM_NCDESTROY, WM_QUIT, WM_RBUTTONUP,
                WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
            },
        },
    };

    const TRAY_ICON_ID: u32 = 1;
    const TRAY_ICON_RESOURCE: usize = 1;
    const TRAY_CALLBACK_MESSAGE: u32 = WM_APP + 1;
    const TRAY_MENU_CLOSE_COMMAND: usize = 1001;
    const TRAY_CLASS_NAME: &[u16] = &[
        'C' as u16, 'u' as u16, 'r' as u16, 'l' as u16, 'D' as u16, 'o' as u16, 'w' as u16,
        'n' as u16, 'l' as u16, 'o' as u16, 'a' as u16, 'd' as u16, 'e' as u16, 'r' as u16,
        'T' as u16, 'r' as u16, 'a' as u16, 'y' as u16, 'W' as u16, 'i' as u16, 'n' as u16,
        'd' as u16, 'o' as u16, 'w' as u16, 0,
    ];
    const TRAY_WINDOW_TITLE: &[u16] = &[
        'C' as u16, 'u' as u16, 'r' as u16, 'l' as u16, 'D' as u16, 'o' as u16, 'w' as u16,
        'n' as u16, 'l' as u16, 'o' as u16, 'a' as u16, 'd' as u16, 'e' as u16, 'r' as u16, 0,
    ];
    const TRAY_TOOLTIP: &[u16] = &[
        'C' as u16, 'u' as u16, 'r' as u16, 'l' as u16, ' ' as u16, 'D' as u16, 'o' as u16,
        'w' as u16, 'n' as u16, 'l' as u16, 'o' as u16, 'a' as u16, 'd' as u16, 'e' as u16,
        'r' as u16, 0,
    ];
    const TRAY_MENU_CLOSE_LABEL: &[u16] = &[
        '關' as u16,
        '閉' as u16,
        ' ' as u16,
        'C' as u16,
        'u' as u16,
        'r' as u16,
        'l' as u16,
        ' ' as u16,
        'D' as u16,
        'o' as u16,
        'w' as u16,
        'n' as u16,
        'l' as u16,
        'o' as u16,
        'a' as u16,
        'd' as u16,
        'e' as u16,
        'r' as u16,
        0,
    ];

    pub struct TrayController {
        thread_id: u32,
        thread: Option<JoinHandle<()>>,
    }

    struct TrayWindowState {
        events: Sender<TrayEvent>,
    }

    impl TrayController {
        pub fn disabled() -> Self {
            Self {
                thread_id: 0,
                thread: None,
            }
        }

        pub fn from_thread(thread_id: u32, thread: JoinHandle<()>) -> Self {
            Self {
                thread_id,
                thread: Some(thread),
            }
        }
    }

    fn show_tray_context_menu(hwnd: HWND, state: &TrayWindowState) {
        let menu: HMENU = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            return;
        }

        let command = unsafe {
            let mut selected = 0;
            if AppendMenuW(
                menu,
                MF_STRING,
                TRAY_MENU_CLOSE_COMMAND,
                TRAY_MENU_CLOSE_LABEL.as_ptr(),
            ) != 0
            {
                let mut point = POINT::default();
                if GetCursorPos(&mut point) != 0 {
                    SetForegroundWindow(hwnd);
                    selected = TrackPopupMenu(
                        menu,
                        TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RETURNCMD,
                        point.x,
                        point.y,
                        0,
                        hwnd,
                        ptr::null(),
                    );
                }
            }
            DestroyMenu(menu);
            selected
        };

        if command == TRAY_MENU_CLOSE_COMMAND as i32 {
            let _ = state.events.send(TrayEvent::CloseWindow);
        }
    }
    unsafe extern "system" fn tray_window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_NCCREATE {
            let create = lparam as *const CREATESTRUCTW;
            if !create.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*create).lpCreateParams as isize);
                }
            }
            return 1;
        }

        let state = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA)
        } as *mut TrayWindowState;
        if message == TRAY_CALLBACK_MESSAGE && wparam == TRAY_ICON_ID as usize && !state.is_null() {
            unsafe {
                let state = &*state;
                match lparam as u32 {
                    WM_LBUTTONDBLCLK => {
                        let _ = state.events.send(TrayEvent::ShowWindow);
                    }
                    WM_RBUTTONUP => show_tray_context_menu(hwnd, state),
                    _ => {}
                }
            }
            return 0;
        }

        if message == WM_NCDESTROY {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
        }
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }

    fn run_tray_thread(events: Sender<TrayEvent>, ready: SyncSender<Result<u32, String>>) {
        let thread_id = unsafe { GetCurrentThreadId() };
        let hinstance = unsafe { GetModuleHandleW(ptr::null()) } as HINSTANCE;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(tray_window_proc),
            hInstance: hinstance,
            lpszClassName: TRAY_CLASS_NAME.as_ptr(),
            ..Default::default()
        };
        let atom = unsafe { RegisterClassW(&class) };
        if atom == 0 && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS {
            let _ = ready.send(Err(format!("RegisterClassW 失敗：{}", unsafe {
                GetLastError()
            })));
            return;
        }

        let state = Box::new(TrayWindowState { events });
        let state_ptr = Box::into_raw(state);
        let hwnd = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                TRAY_CLASS_NAME.as_ptr(),
                TRAY_WINDOW_TITLE.as_ptr(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                hinstance,
                state_ptr.cast(),
            )
        };
        if hwnd.is_null() {
            unsafe {
                drop(Box::from_raw(state_ptr));
            }
            let _ = ready.send(Err("CreateWindowExW 失敗".into()));
            return;
        }

        let icon = tray_icon_data(hwnd);
        let mut added = false;
        for _ in 0..20 {
            if unsafe { Shell_NotifyIconW(NIM_ADD, &icon) } != 0 {
                added = true;
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if !added {
            let error = unsafe { GetLastError() };
            unsafe {
                DestroyWindow(hwnd);
                drop(Box::from_raw(state_ptr));
            }
            let _ = ready.send(Err(format!(
                "Shell_NotifyIconW(NIM_ADD) 失敗：{} size={}",
                error, icon.cbSize
            )));
            return;
        }

        let _ = ready.send(Ok(thread_id));

        let mut message = windows_sys::Win32::UI::WindowsAndMessaging::MSG::default();
        loop {
            let result = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
            if result <= 0 || message.message == WM_QUIT {
                break;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &icon);
            DestroyWindow(hwnd);
            drop(Box::from_raw(state_ptr));
        }
    }

    fn tray_icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY_CALLBACK_MESSAGE,
            hIcon: unsafe {
                let hinstance = GetModuleHandleW(ptr::null()) as HINSTANCE;
                let custom_icon = LoadIconW(hinstance, TRAY_ICON_RESOURCE as *const u16);
                if custom_icon.is_null() {
                    LoadIconW(ptr::null_mut(), IDI_APPLICATION)
                } else {
                    custom_icon
                }
            },
            ..Default::default()
        };
        data.szTip[..TRAY_TOOLTIP.len()].copy_from_slice(TRAY_TOOLTIP);
        data
    }

    impl Drop for TrayController {
        fn drop(&mut self) {
            if self.thread_id != 0 {
                unsafe {
                    let _ = windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                        self.thread_id,
                        WM_QUIT,
                        0,
                        0,
                    );
                }
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    pub fn create() -> Result<(TrayController, Receiver<TrayEvent>), String> {
        let (events, receiver) = std::sync::mpsc::channel();
        let (ready_sender, ready_receiver) = sync_channel(1);
        let thread = thread::Builder::new()
            .name("curl-downloader-tray".into())
            .spawn(move || run_tray_thread(events, ready_sender))
            .map_err(|error| format!("無法啟動系統匣執行緒：{error}"))?;
        let thread_id = ready_receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "系統匣執行緒啟動逾時".to_owned())??;
        Ok((TrayController::from_thread(thread_id, thread), receiver))
    }
}

#[cfg(not(windows))]
mod windows_impl {
    use super::TrayEvent;
    use std::sync::mpsc::Receiver;

    pub struct TrayController;

    impl TrayController {
        pub fn disabled() -> Self {
            Self
        }

        pub fn new() -> Self {
            Self
        }
    }

    impl Drop for TrayController {
        fn drop(&mut self) {}
    }

    pub fn create() -> Result<(TrayController, Receiver<TrayEvent>), String> {
        let (_sender, receiver) = std::sync::mpsc::channel();
        Ok((TrayController::disabled(), receiver))
    }
}

pub struct TrayController {
    _inner: windows_impl::TrayController,
}

impl TrayController {
    pub fn disabled() -> (Self, Receiver<TrayEvent>) {
        let (_sender, receiver) = std::sync::mpsc::channel();
        (
            Self {
                _inner: windows_impl::TrayController::disabled(),
            },
            receiver,
        )
    }

    pub fn create() -> Result<(Self, Receiver<TrayEvent>), String> {
        let (inner, receiver) = windows_impl::create()?;
        Ok((Self { _inner: inner }, receiver))
    }
}

#[cfg(test)]
mod tests {
    use super::{TrayEvent, WindowVisibility, startup_visibility};

    #[test]
    fn extension_startup_is_hidden_instead_of_flashing_a_window() {
        assert_eq!(startup_visibility(true), WindowVisibility::HiddenToTray);
        assert_eq!(startup_visibility(false), WindowVisibility::Visible);
    }

    #[test]
    fn tray_close_is_a_distinct_explicit_exit_event() {
        assert_ne!(TrayEvent::ShowWindow, TrayEvent::CloseWindow);
    }
}
