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

/// Reveal an existing file in Explorer; unfinished or removed files open their folder.
pub fn reveal_file_foreground(path: &Path) -> io::Result<OpenTargetOutcome> {
    if !path.is_file() {
        return open_folder_foreground(path.parent().unwrap_or(path));
    }
    #[cfg(windows)]
    {
        let path = path.to_owned();
        windows_impl::run_sta(move || windows_impl::reveal_file(&path))
    }
    #[cfg(not(windows))]
    {
        open_folder_foreground(path.parent().unwrap_or(path))
    }
}

fn normalize_windows_path(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('/', "\\");
    if let Some(stripped) = normalized.strip_prefix(r"\\?\UNC\") {
        normalized = format!(r"\\{stripped}");
    } else if let Some(stripped) = normalized.strip_prefix(r"\\?\") {
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
    fn foreground_window(&self) -> Option<WindowId>;
    fn is_minimized(&self, hwnd: WindowId) -> bool;
    fn show_restore(&self, hwnd: WindowId) -> bool;
    fn set_foreground(&self, hwnd: WindowId) -> bool;
    fn flash(&self, hwnd: WindowId);
}

fn focus_window_with<A: ForegroundApi>(api: &A, hwnd: WindowId) -> bool {
    // Keep normal/maximized/snapped windows and their child focus untouched.
    // Joining Explorer's input queue or setting focus on its frame can disturb
    // its own UI activation; let the owning process handle activation instead.
    let restored = !api.is_minimized(hwnd) || api.show_restore(hwnd);
    let success = restored
        && (api.foreground_window() == Some(hwnd) || api.set_foreground(hwnd))
        && api.foreground_window() == Some(hwnd);
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
                    CoInitializeEx, CoTaskMemFree, CoUninitialize, DISPATCH_PROPERTYGET,
                    DISPPARAMS, IDispatch,
                },
                Variant::{VARIANT, VT_BSTR, VT_I4, VT_I8},
            },
            UI::Shell::{
                Common::ITEMIDLIST, IShellWindows, SHOpenFolderAndSelectItems, SHParseDisplayName,
                ShellWindows,
            },
        },
        core::{BSTR, GUID, PCWSTR},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HWND as RawHwnd, LPARAM},
        System::Threading::GetProcessId,
        UI::{
            Shell::{
                SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_UNICODE, SHELLEXECUTEINFOW,
                ShellExecuteExW,
            },
            WindowsAndMessaging::{
                EnumWindows, FlashWindow, GW_OWNER, GetForegroundWindow, GetWindow,
                GetWindowPlacement, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
                SW_RESTORE, SW_SHOWMAXIMIZED, SW_SHOWNORMAL, SetForegroundWindow, ShowWindowAsync,
                WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED,
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

    pub(super) fn reveal_file(path: &Path) -> io::Result<super::OpenTargetOutcome> {
        struct ItemIdList(*mut ITEMIDLIST);
        impl Drop for ItemIdList {
            fn drop(&mut self) {
                unsafe { CoTaskMemFree(Some(self.0.cast())) };
            }
        }
        let path = std::fs::canonicalize(path)?;
        let wide_path = to_wide(&super::normalize_windows_path(&path));
        let mut item = ItemIdList(null_mut());
        unsafe {
            SHParseDisplayName(PCWSTR(wide_path.as_ptr()), None, &mut item.0, 0, None)
                .map_err(|error| io::Error::other(error.to_string()))?;
            // With no child array, this absolute PIDL opens the parent and
            // selects the file, even when that folder is already open.
            SHOpenFolderAndSelectItems(item.0, None, 0)
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        let folder = path.parent().unwrap_or(&path);
        let deadline = Instant::now() + POLL_TIMEOUT;
        loop {
            if let Ok(windows) = enumerate_explorer_windows() {
                if let Some(hwnd) = select_explorer_window(&windows, folder) {
                    return Ok(focus_outcome(hwnd));
                }
            }
            if Instant::now() >= deadline {
                return Ok(super::OpenTargetOutcome::OpenedButNotFocused);
            }
            thread::sleep(POLL_INTERVAL);
        }
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
        fn foreground_window(&self) -> Option<WindowId> {
            current_foreground_window()
        }

        fn is_minimized(&self, hwnd: WindowId) -> bool {
            unsafe { IsIconic(win32_handle(hwnd)) != 0 }
        }

        fn show_restore(&self, hwnd: WindowId) -> bool {
            let mut placement: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
            placement.length = size_of::<WINDOWPLACEMENT>() as u32;
            let restore_to_maximized = unsafe {
                GetWindowPlacement(win32_handle(hwnd), &mut placement) != 0
                    && placement.flags & WPF_RESTORETOMAXIMIZED != 0
            };
            let command = if restore_to_maximized {
                SW_SHOWMAXIMIZED
            } else {
                SW_RESTORE
            };
            unsafe { ShowWindowAsync(win32_handle(hwnd), command) != 0 }
        }

        fn set_foreground(&self, hwnd: WindowId) -> bool {
            unsafe { SetForegroundWindow(win32_handle(hwnd)) != 0 }
        }

        fn flash(&self, hwnd: WindowId) {
            unsafe {
                FlashWindow(win32_handle(hwnd), 1);
            }
        }
    }

    #[cfg(test)]
    mod desktop_tests {
        use super::*;
        use windows_sys::Win32::{
            Foundation::RECT,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, GetWindowRect, IsZoomed, MSG,
                PM_REMOVE, PeekMessageW, SW_MINIMIZE, SW_SHOWMAXIMIZED, ShowWindow,
                TranslateMessage, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
            },
        };

        #[test]
        #[ignore = "requires an interactive Windows desktop and an explicit fixture file"]
        fn reveal_selects_fixture_in_explorer() {
            let path = PathBuf::from(
                std::env::var_os("CURL_DOWNLOADER_REVEAL_TEST_FILE")
                    .expect("set CURL_DOWNLOADER_REVEAL_TEST_FILE to an existing fixture"),
            );
            assert!(path.is_file());
            super::super::reveal_file_foreground(&path).unwrap();
        }

        struct TestWindow(RawHwnd);

        impl Drop for TestWindow {
            fn drop(&mut self) {
                unsafe { DestroyWindow(self.0) };
            }
        }

        fn rectangle(hwnd: RawHwnd) -> (i32, i32, i32, i32) {
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { GetWindowRect(hwnd, &mut rect) }, 0);
            (rect.left, rect.top, rect.right, rect.bottom)
        }

        #[test]
        #[ignore = "requires an interactive Windows desktop; creates only a disposable test window"]
        fn activation_preserves_real_window_geometry_and_maximized_restore_state() {
            let class = to_wide("STATIC");
            let title = to_wide("Curl Downloader shell activation regression test");
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    100,
                    100,
                    500,
                    300,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                )
            };
            assert!(!hwnd.is_null());
            let window = TestWindow(hwnd);
            let api = Win32ForegroundApi;
            let normal = rectangle(hwnd);
            let _ = focus_window_with(&api, hwnd as WindowId);
            assert_eq!(
                rectangle(hwnd),
                normal,
                "visible window was resized or moved"
            );

            unsafe { ShowWindow(hwnd, SW_SHOWMAXIMIZED) };
            assert_ne!(unsafe { IsZoomed(hwnd) }, 0);
            let maximized = rectangle(hwnd);
            let _ = focus_window_with(&api, hwnd as WindowId);
            assert_ne!(
                unsafe { IsZoomed(hwnd) },
                0,
                "activation restored a maximized window"
            );
            assert_eq!(rectangle(hwnd), maximized);

            unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
            assert_ne!(unsafe { IsIconic(hwnd) }, 0);
            let _ = focus_window_with(&api, hwnd as WindowId);
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let mut message: MSG = unsafe { std::mem::zeroed() };
                while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
                    unsafe {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                if unsafe { IsIconic(hwnd) } == 0 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "minimized window did not restore"
                );
                thread::sleep(Duration::from_millis(10));
            }
            assert_ne!(
                unsafe { IsZoomed(hwnd) },
                0,
                "minimized maximized state was lost"
            );
            assert_eq!(rectangle(hwnd), maximized);
            drop(window);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_unc_paths_keep_their_network_root() {
        assert_eq!(
            normalize_windows_path(Path::new(r"\\?\UNC\server\share\file.bin")),
            r"\\server\share\file.bin"
        );
    }

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
    fn focusing_visible_window_does_not_restore_or_reassign_child_focus() {
        let api = RecordingForegroundApi::success();
        assert!(focus_window_with(&api, 42));
        assert_eq!(api.calls(), vec!["foreground:42"]);
    }

    #[test]
    fn minimized_window_is_restored_before_activation() {
        let mut api = RecordingForegroundApi::success();
        api.minimized = true;
        assert!(focus_window_with(&api, 42));
        assert_eq!(api.calls(), vec!["restore:42", "foreground:42"]);
    }

    #[test]
    fn already_foreground_window_keeps_its_existing_child_focus() {
        let api = RecordingForegroundApi::success();
        *api.foreground.lock().unwrap() = 42;
        assert!(focus_window_with(&api, 42));
        assert!(api.calls().is_empty());
    }

    #[test]
    fn failed_activation_only_flashes_without_changing_window_layout() {
        let api = RecordingForegroundApi::foreground_failure();
        assert!(!focus_window_with(&api, 42));
        assert_eq!(api.calls(), vec!["foreground:42", "flash:42"]);
    }

    struct RecordingForegroundApi {
        calls: std::sync::Mutex<Vec<String>>,
        foreground: std::sync::Mutex<WindowId>,
        foreground_result: bool,
        minimized: bool,
    }

    impl RecordingForegroundApi {
        fn success() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                foreground: std::sync::Mutex::new(9),
                foreground_result: true,
                minimized: false,
            }
        }

        fn foreground_failure() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                foreground: std::sync::Mutex::new(9),
                foreground_result: false,
                minimized: false,
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ForegroundApi for RecordingForegroundApi {
        fn foreground_window(&self) -> Option<WindowId> {
            Some(*self.foreground.lock().unwrap())
        }

        fn is_minimized(&self, _hwnd: WindowId) -> bool {
            self.minimized
        }

        fn show_restore(&self, hwnd: WindowId) -> bool {
            self.calls.lock().unwrap().push(format!("restore:{hwnd}"));
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

        fn flash(&self, hwnd: WindowId) {
            self.calls.lock().unwrap().push(format!("flash:{hwnd}"));
        }
    }
}
