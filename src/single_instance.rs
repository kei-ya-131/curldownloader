pub const GUI_MUTEX_NAME: &str = r"Local\CurlDownloader-GUI-v1";

pub struct GuiInstanceGuard {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

pub fn acquire() -> Result<Option<GuiInstanceGuard>, String> {
    #[cfg(windows)]
    {
        use std::ptr;
        use windows_sys::Win32::{
            Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError},
            System::Threading::CreateMutexW,
        };

        let name: Vec<u16> = GUI_MUTEX_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(format!("無法建立 GUI 單例 Mutex：Win32 {}", unsafe {
                GetLastError()
            }));
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(handle);
            }
            return Ok(None);
        }
        return Ok(Some(GuiInstanceGuard { handle }));
    }

    #[cfg(not(windows))]
    {
        Ok(Some(GuiInstanceGuard {}))
    }
}

impl Drop for GuiInstanceGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn acquire_returns_none_when_gui_mutex_is_already_owned() {
        let first = acquire().unwrap();
        assert!(first.is_some());
        let second = acquire().unwrap();
        assert!(second.is_none());
    }
}
