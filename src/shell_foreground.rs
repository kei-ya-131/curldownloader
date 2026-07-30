use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
};

type WindowId = isize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenTargetOutcome {
    Focused,
    OpenedButNotFocused,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExplorerWindow {
    hwnd: WindowId,
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TopLevelWindow {
    hwnd: WindowId,
    process_id: u32,
    visible: bool,
    has_owner: bool,
}

#[cfg(windows)]
pub fn open_file_foreground(path: &Path) -> io::Result<OpenTargetOutcome> {
    let path = path.to_owned();
    windows_impl::run_sta(move || windows_impl::open_file(&path))
}

#[cfg(not(windows))]
pub fn open_file_foreground(_path: &Path) -> io::Result<OpenTargetOutcome> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "foreground shell support is only available on Windows",
    ))
}

#[cfg(windows)]
pub fn open_folder_foreground(path: &Path) -> io::Result<OpenTargetOutcome> {
    let path = path.to_owned();
    windows_impl::run_sta(move || windows_impl::open_folder(&path))
}

#[cfg(not(windows))]
pub fn open_folder_foreground(_path: &Path) -> io::Result<OpenTargetOutcome> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "foreground shell support is only available on Windows",
    ))
}

fn normalize_windows_path(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('/', "\\");
    if let Some(stripped) = normalized.strip_prefix(r"\\?\") {
        normalized = stripped.to_owned();
    }
    let is_drive_root = normalized.len() == 3
        && normalized.as_bytes().get(1) == Some(&b':')
        && normalized.ends_with('\\');
    while normalized.ends_with('\\') && !is_drive_root {
        normalized.pop();
    }
    normalized
}

fn select_explorer_window(windows: &[ExplorerWindow], target: &Path) -> Option<WindowId> {
    let target = normalize_windows_path(target);
    windows
        .iter()
        .find(|window| normalize_windows_path(&window.path).eq_ignore_ascii_case(&target))
        .map(|window| window.hwnd)
}

fn select_file_window(
    before: &HashSet<WindowId>,
    after: &[TopLevelWindow],
    launched_process_id: Option<u32>,
    current_foreground: Option<WindowId>,
) -> Option<WindowId> {
    let candidates = after
        .iter()
        .filter(|window| window.visible && !window.has_owner);
    if let Some(process_id) = launched_process_id {
        if let Some(window) = candidates
            .clone()
            .find(|window| window.process_id == process_id)
        {
            return Some(window.hwnd);
        }
    }
    if let Some(window) = candidates
        .clone()
        .find(|window| !before.contains(&window.hwnd))
    {
        return Some(window.hwnd);
    }
    current_foreground.filter(|hwnd| {
        !before.contains(hwnd) && candidates.clone().any(|window| window.hwnd == *hwnd)
    })
}

trait ForegroundApi {
    fn current_thread_id(&self) -> u32;
    fn foreground_window(&self) -> Option<WindowId>;
    fn window_thread_id(&self, hwnd: WindowId) -> u32;
    fn show_restore(&self, hwnd: WindowId) -> bool;
    fn attach_thread_input(&self, from: u32, to: u32, attach: bool) -> bool;
    fn bring_to_top(&self, hwnd: WindowId) -> bool;
    fn set_top(&self, hwnd: WindowId) -> bool;
    fn set_foreground(&self, hwnd: WindowId) -> bool;
    fn set_focus(&self, hwnd: WindowId) -> bool;
    fn flash(&self, hwnd: WindowId);
}

struct AttachedThreadInputs<'a, A: ForegroundApi> {
    api: &'a A,
    attached: Vec<(u32, u32)>,
}

impl<'a, A: ForegroundApi> AttachedThreadInputs<'a, A> {
    fn new(api: &'a A) -> Self {
        Self {
            api,
            attached: Vec::new(),
        }
    }

    fn attach(&mut self, from: u32, to: u32) {
        if from == 0 || to == 0 || from == to {
            return;
        }
        if self.api.attach_thread_input(from, to, true) {
            self.attached.push((from, to));
        }
    }
}

impl<A: ForegroundApi> Drop for AttachedThreadInputs<'_, A> {
    fn drop(&mut self) {
        for &(from, to) in self.attached.iter().rev() {
            let _ = self.api.attach_thread_input(from, to, false);
        }
    }
}

fn focus_window_with<A: ForegroundApi>(api: &A, hwnd: WindowId) -> bool {
    let success = {
        let mut attached = AttachedThreadInputs::new(api);
        let mut success = api.show_restore(hwnd);
        let caller_thread = api.current_thread_id();
        let foreground_thread = api
            .foreground_window()
            .map(|foreground| api.window_thread_id(foreground));
        let target_thread = api.window_thread_id(hwnd);
        if let Some(foreground_thread) = foreground_thread {
            attached.attach(caller_thread, foreground_thread);
        }
        attached.attach(caller_thread, target_thread);
        success &= api.bring_to_top(hwnd);
        success &= api.set_top(hwnd);
        success &= api.set_foreground(hwnd);
        success &= api.set_focus(hwnd);
        success && api.foreground_window() == Some(hwnd)
    };
    if !success {
        api.flash(hwnd);
    }
    success
}

#[cfg(windows)]
mod windows_impl {
    use super::{WindowId, focus_window_with, select_explorer_window, select_file_window};
    use std::{
        collections::HashSet,
        ffi::c_void,
        io,
        mem::size_of,
        path::{Path, PathBuf},
        ptr::null_mut,
        thread,
        time::{Duration, Instant},
    };

    use windows::{
        Win32::{
            Globalization::LOCALE_SYSTEM_DEFAULT,
            System::{
                Com::{
                    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance,
                    CoInitializeEx, CoUninitialize, DISPATCH_PROPERTYGET, DISPPARAMS, IDispatch,
                },
                Variant::{VARIANT, VT_BSTR, VT_I4, VT_I8},
            },
            UI::Shell::{IShellWindows, ShellWindows},
        },
        core::{BSTR, GUID, PCWSTR},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HWND as RawHwnd, LPARAM},
        System::Threading::{AttachThreadInput, GetCurrentThreadId, GetProcessId},
        UI::{
            Input::KeyboardAndMouse::SetFocus,
            Shell::{
                SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_UNICODE, SHELLEXECUTEINFOW,
                ShellExecuteExW,
            },
            WindowsAndMessaging::{
                BringWindowToTop, EnumWindows, FlashWindow, GW_OWNER, GetForegroundWindow,
                GetWindow, GetWindowThreadProcessId, HWND_TOP, IsWindowVisible, SW_RESTORE,
                SW_SHOWNORMAL, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow,
                SetWindowPos, ShowWindowAsync,
            },
        },
    };

    const POLL_INTERVAL: Duration = Duration::from_millis(50);
    const POLL_TIMEOUT: Duration = Duration::from_secs(2);

    pub(super) fn run_sta<T, F>(operation: F) -> io::Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> io::Result<T> + Send + 'static,
    {
        let handle = thread::Builder::new()
            .name("curl-downloader-shell-sta".into())
            .spawn(move || {
                let initialize_result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                if initialize_result.0 < 0 {
                    return Err(io::Error::other(format!(
                        "CoInitializeEx failed: HRESULT 0x{:08x}",
                        initialize_result.0 as u32
                    )));
                }

                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
                unsafe {
                    CoUninitialize();
                }

                match result {
                    Ok(result) => result,
                    Err(_) => Err(io::Error::other("Windows shell worker panicked")),
                }
            })
            .map_err(|error| io::Error::other(error.to_string()))?;

        handle
            .join()
            .map_err(|_| io::Error::other("Windows shell worker panicked"))?
    }

    pub(super) fn open_file(path: &Path) -> io::Result<super::OpenTargetOutcome> {
        let before_windows = enumerate_top_level_windows().unwrap_or_default();
        let before: HashSet<_> = before_windows.iter().map(|window| window.hwnd).collect();

        let launched_process_id = shell_open(path)?;
        let deadline = Instant::now() + POLL_TIMEOUT;
        loop {
            let after = enumerate_top_level_windows().unwrap_or_default();
            let current_foreground = current_foreground_window();
            if let Some(hwnd) =
                select_file_window(&before, &after, launched_process_id, current_foreground)
            {
                return Ok(focus_outcome(hwnd));
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(POLL_INTERVAL);
        }

        Ok(super::OpenTargetOutcome::OpenedButNotFocused)
    }

    pub(super) fn open_folder(path: &Path) -> io::Result<super::OpenTargetOutcome> {
        if let Ok(windows) = enumerate_explorer_windows() {
            if let Some(hwnd) = select_explorer_window(&windows, path) {
                return Ok(focus_outcome(hwnd));
            }
        }

        let before_windows = enumerate_top_level_windows().unwrap_or_default();
        let before: HashSet<_> = before_windows.iter().map(|window| window.hwnd).collect();
        let launched_process_id = shell_open(path)?;
        let deadline = Instant::now() + POLL_TIMEOUT;

        loop {
            if let Ok(windows) = enumerate_explorer_windows() {
                if let Some(hwnd) = select_explorer_window(&windows, path) {
                    return Ok(focus_outcome(hwnd));
                }
            }

            let after = enumerate_top_level_windows().unwrap_or_default();
            if let Some(hwnd) = select_file_window(
                &before,
                &after,
                launched_process_id,
                current_foreground_window(),
            ) {
                return Ok(focus_outcome(hwnd));
            }

            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(POLL_INTERVAL);
        }

        Ok(super::OpenTargetOutcome::OpenedButNotFocused)
    }

    fn focus_outcome(hwnd: WindowId) -> super::OpenTargetOutcome {
        let api = Win32ForegroundApi;
        for attempt in 0..3 {
            if focus_window_with(&api, hwnd) {
                return super::OpenTargetOutcome::Focused;
            }
            if attempt < 2 {
                thread::sleep(Duration::from_millis(25));
            }
        }
        super::OpenTargetOutcome::OpenedButNotFocused
    }

    fn current_foreground_window() -> Option<WindowId> {
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.is_null()).then_some(hwnd as WindowId)
    }

    fn shell_open(path: &Path) -> io::Result<Option<u32>> {
        let wide_path = to_wide(path.to_string_lossy().as_ref());
        let mut execute_info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        execute_info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        execute_info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_UNICODE;
        execute_info.lpFile = wide_path.as_ptr();
        execute_info.nShow = SW_SHOWNORMAL;

        let launched = unsafe { ShellExecuteExW(&mut execute_info) };
        if launched == 0 {
            return Err(io::Error::last_os_error());
        }

        let process_id = if execute_info.hProcess.is_null() {
            None
        } else {
            let process_id = unsafe { GetProcessId(execute_info.hProcess) };
            unsafe {
                CloseHandle(execute_info.hProcess);
            }
            (process_id != 0).then_some(process_id)
        };
        Ok(process_id)
    }

    fn enumerate_top_level_windows() -> io::Result<Vec<super::TopLevelWindow>> {
        struct Search {
            windows: Vec<super::TopLevelWindow>,
        }

        unsafe extern "system" fn callback(hwnd: RawHwnd, lparam: LPARAM) -> i32 {
            let search = unsafe { &mut *(lparam as *mut Search) };
            let visible = unsafe { IsWindowVisible(hwnd) != 0 };
            if visible {
                let mut process_id = 0u32;
                unsafe {
                    GetWindowThreadProcessId(hwnd, &mut process_id);
                }
                let has_owner = !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null();
                search.windows.push(super::TopLevelWindow {
                    hwnd: hwnd as WindowId,
                    process_id,
                    visible,
                    has_owner,
                });
            }
            1
        }

        let mut search = Search {
            windows: Vec::new(),
        };
        let result = unsafe {
            EnumWindows(
                Some(callback),
                (&mut search as *mut Search).cast::<c_void>() as LPARAM,
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(search.windows)
    }

    fn enumerate_explorer_windows() -> io::Result<Vec<super::ExplorerWindow>> {
        let shell_windows: IShellWindows =
            unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER) }
                .map_err(|error| io::Error::other(error.to_string()))?;

        let count = unsafe { shell_windows.Count() }
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut result = Vec::new();

        for index in 0..count {
            let index_variant = VARIANT::from(index);
            let dispatch = match unsafe { shell_windows.Item(&index_variant) } {
                Ok(dispatch) => dispatch,
                Err(_) => continue,
            };
            let hwnd = match dispatch_property_i64(&dispatch, "HWND") {
                Some(hwnd) if hwnd != 0 => hwnd as WindowId,
                _ => continue,
            };
            let location = match dispatch_property_string(&dispatch, "LocationURL") {
                Some(location) => location,
                None => continue,
            };
            if let Some(path) = location_to_path(&location) {
                result.push(super::ExplorerWindow { hwnd, path });
            }
        }

        Ok(result)
    }

    fn dispatch_property(dispatch: &IDispatch, property: &str) -> Option<VARIANT> {
        let property_name = to_wide(property);
        let property_name = PCWSTR(property_name.as_ptr());
        let names = [property_name];
        let null_iid = GUID::from_u128(0);
        let mut dispid = 0i32;
        unsafe {
            dispatch
                .GetIDsOfNames(
                    &null_iid,
                    names.as_ptr(),
                    names.len() as u32,
                    LOCALE_SYSTEM_DEFAULT,
                    &mut dispid,
                )
                .ok()?;
        }

        let parameters = DISPPARAMS {
            rgvarg: null_mut(),
            rgdispidNamedArgs: null_mut(),
            cArgs: 0,
            cNamedArgs: 0,
        };
        let mut result = VARIANT::default();
        unsafe {
            dispatch
                .Invoke(
                    dispid,
                    &null_iid,
                    LOCALE_SYSTEM_DEFAULT,
                    DISPATCH_PROPERTYGET,
                    &parameters,
                    Some(&mut result),
                    None,
                    None,
                )
                .ok()?;
        }
        Some(result)
    }

    fn dispatch_property_i64(dispatch: &IDispatch, property: &str) -> Option<i64> {
        let result = dispatch_property(dispatch, property)?;
        let value_type = unsafe { result.Anonymous.Anonymous.vt };
        unsafe {
            if value_type == VT_I4 {
                Some(result.Anonymous.Anonymous.Anonymous.lVal as i64)
            } else if value_type == VT_I8 {
                Some(result.Anonymous.Anonymous.Anonymous.llVal)
            } else {
                None
            }
        }
    }

    fn dispatch_property_string(dispatch: &IDispatch, property: &str) -> Option<String> {
        let result = dispatch_property(dispatch, property)?;
        let value_type = unsafe { result.Anonymous.Anonymous.vt };
        if value_type != VT_BSTR {
            return None;
        }
        let bstr_ptr = unsafe {
            (&result.Anonymous.Anonymous.Anonymous.bstrVal as *const std::mem::ManuallyDrop<BSTR>)
                .cast::<BSTR>()
        };
        let bstr = unsafe { &*bstr_ptr };
        String::try_from(bstr).ok()
    }

    fn location_to_path(location: &str) -> Option<PathBuf> {
        if let Ok(url) = url::Url::parse(location) {
            if url.scheme().eq_ignore_ascii_case("file") {
                return url.to_file_path().ok();
            }
        }
        Some(PathBuf::from(location))
    }

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn win32_handle(hwnd: WindowId) -> RawHwnd {
        hwnd as RawHwnd
    }

    struct Win32ForegroundApi;

    impl super::ForegroundApi for Win32ForegroundApi {
        fn current_thread_id(&self) -> u32 {
            unsafe { GetCurrentThreadId() }
        }

        fn foreground_window(&self) -> Option<WindowId> {
            current_foreground_window()
        }

        fn window_thread_id(&self, hwnd: WindowId) -> u32 {
            unsafe { GetWindowThreadProcessId(win32_handle(hwnd), null_mut()) }
        }

        fn show_restore(&self, hwnd: WindowId) -> bool {
            unsafe {
                ShowWindowAsync(win32_handle(hwnd), SW_RESTORE);
            }
            true
        }

        fn attach_thread_input(&self, from: u32, to: u32, attach: bool) -> bool {
            unsafe { AttachThreadInput(from, to, if attach { 1 } else { 0 }) != 0 }
        }

        fn bring_to_top(&self, hwnd: WindowId) -> bool {
            unsafe { BringWindowToTop(win32_handle(hwnd)) != 0 }
        }

        fn set_top(&self, hwnd: WindowId) -> bool {
            unsafe {
                SetWindowPos(
                    win32_handle(hwnd),
                    HWND_TOP,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                ) != 0
            }
        }

        fn set_foreground(&self, hwnd: WindowId) -> bool {
            unsafe { SetForegroundWindow(win32_handle(hwnd)) != 0 }
        }

        fn set_focus(&self, hwnd: WindowId) -> bool {
            unsafe {
                SetFocus(win32_handle(hwnd));
            }
            true
        }

        fn flash(&self, hwnd: WindowId) {
            unsafe {
                FlashWindow(win32_handle(hwnd), 1);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_matching_is_case_insensitive_and_ignores_trailing_separator() {
        let windows = vec![ExplorerWindow {
            hwnd: 7,
            path: PathBuf::from(r"C:\Users\Alice\Downloads\\"),
        }];
        assert_eq!(
            select_explorer_window(&windows, Path::new(r"c:\users\alice\downloads")),
            Some(7)
        );
    }

    #[test]
    fn file_window_prefers_shell_process_id_then_new_window() {
        let before = HashSet::from([10]);
        let after = vec![
            TopLevelWindow {
                hwnd: 10,
                process_id: 80,
                visible: true,
                has_owner: false,
            },
            TopLevelWindow {
                hwnd: 11,
                process_id: 90,
                visible: true,
                has_owner: false,
            },
        ];
        assert_eq!(
            select_file_window(&before, &after, Some(90), None),
            Some(11)
        );
    }

    #[test]
    fn file_window_uses_a_new_visible_ownerless_window_when_process_id_is_missing() {
        let before = HashSet::from([10, 12]);
        let after = vec![
            TopLevelWindow {
                hwnd: 10,
                process_id: 80,
                visible: true,
                has_owner: false,
            },
            TopLevelWindow {
                hwnd: 12,
                process_id: 90,
                visible: true,
                has_owner: true,
            },
            TopLevelWindow {
                hwnd: 13,
                process_id: 91,
                visible: true,
                has_owner: false,
            },
            TopLevelWindow {
                hwnd: 14,
                process_id: 92,
                visible: false,
                has_owner: false,
            },
        ];
        assert_eq!(
            select_file_window(&before, &after, None, Some(10)),
            Some(13)
        );
    }

    #[test]
    fn file_window_does_not_reselect_the_old_foreground_window() {
        let before = HashSet::from([10]);
        let after = vec![TopLevelWindow {
            hwnd: 10,
            process_id: 80,
            visible: true,
            has_owner: false,
        }];
        assert_eq!(select_file_window(&before, &after, None, Some(10)), None);
    }
    #[test]
    fn focus_sequence_detaches_every_successful_thread_attachment() {
        let api = RecordingForegroundApi::success();
        assert!(focus_window_with(&api, 42));
        assert_eq!(
            api.calls(),
            vec![
                "restore:42",
                "attach:caller:foreground",
                "attach:caller:target",
                "bring:42",
                "top:42",
                "foreground:42",
                "focus:42",
                "detach:caller:target",
                "detach:caller:foreground",
            ]
        );
    }

    #[test]
    fn failed_focus_still_detaches_and_flashes() {
        let api = RecordingForegroundApi::foreground_failure();
        assert!(!focus_window_with(&api, 42));
        assert!(api.calls().contains(&"detach:caller:target".into()));
        assert_eq!(api.calls().last().unwrap(), "flash:42");
    }

    struct RecordingForegroundApi {
        calls: std::sync::Mutex<Vec<String>>,
        foreground: std::sync::Mutex<WindowId>,
        foreground_result: bool,
    }

    impl RecordingForegroundApi {
        fn success() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                foreground: std::sync::Mutex::new(9),
                foreground_result: true,
            }
        }

        fn foreground_failure() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                foreground: std::sync::Mutex::new(9),
                foreground_result: false,
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ForegroundApi for RecordingForegroundApi {
        fn current_thread_id(&self) -> u32 {
            1
        }

        fn foreground_window(&self) -> Option<WindowId> {
            Some(*self.foreground.lock().unwrap())
        }

        fn window_thread_id(&self, hwnd: WindowId) -> u32 {
            if hwnd == 9 { 2 } else { 3 }
        }

        fn show_restore(&self, hwnd: WindowId) -> bool {
            self.calls.lock().unwrap().push(format!("restore:{hwnd}"));
            true
        }

        fn attach_thread_input(&self, _from: u32, to: u32, attach: bool) -> bool {
            self.calls.lock().unwrap().push(if attach {
                if to == 2 {
                    "attach:caller:foreground".into()
                } else {
                    "attach:caller:target".into()
                }
            } else if to == 2 {
                "detach:caller:foreground".into()
            } else {
                "detach:caller:target".into()
            });
            true
        }

        fn bring_to_top(&self, hwnd: WindowId) -> bool {
            self.calls.lock().unwrap().push(format!("bring:{hwnd}"));
            true
        }

        fn set_top(&self, hwnd: WindowId) -> bool {
            self.calls.lock().unwrap().push(format!("top:{hwnd}"));
            true
        }

        fn set_foreground(&self, hwnd: WindowId) -> bool {
            self.calls
                .lock()
                .unwrap()
                .push(format!("foreground:{hwnd}"));
            if self.foreground_result {
                *self.foreground.lock().unwrap() = hwnd;
            }
            self.foreground_result
        }

        fn set_focus(&self, hwnd: WindowId) -> bool {
            self.calls.lock().unwrap().push(format!("focus:{hwnd}"));
            true
        }

        fn flash(&self, hwnd: WindowId) {
            self.calls.lock().unwrap().push(format!("flash:{hwnd}"));
        }
    }
}
