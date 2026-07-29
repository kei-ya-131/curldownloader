use std::{io, thread::JoinHandle};

pub const EVENT_NAME: &str = "Local\\CurlDownloader-Manual-Shutdown-v1";

#[cfg(windows)]
mod windows_impl {
    use super::EVENT_NAME;
    #[cfg(test)]
    use std::time::Duration;
    use std::{
        ffi::OsStr,
        io,
        os::windows::ffi::OsStrExt,
        ptr,
        thread::{self, JoinHandle},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_OBJECT_0},
        System::Threading::{
            CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, ResetEvent,
            SYNCHRONIZATION_SYNCHRONIZE, SetEvent, WaitForSingleObject,
        },
    };

    fn event_name() -> Vec<u16> {
        OsStr::new(EVENT_NAME)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn last_error(operation: &str) -> io::Error {
        let code = unsafe { GetLastError() };
        io::Error::other(format!("{operation} 失敗：{code}"))
    }

    fn create_event() -> io::Result<HANDLE> {
        let name = event_name();
        let handle = unsafe { CreateEventW(ptr::null(), 1, 0, name.as_ptr()) };
        if handle.is_null() {
            Err(last_error("CreateEventW"))
        } else {
            Ok(handle)
        }
    }

    fn open_event_for_modify_and_wait() -> io::Result<HANDLE> {
        let name = event_name();
        let access = EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE;
        let handle = unsafe { OpenEventW(access, 0, name.as_ptr()) };
        if handle.is_null() {
            // The named kernel object disappears when its last handle closes.
            // Recreate it so the native host can keep a durable wait handle.
            create_event()
        } else {
            Ok(handle)
        }
    }

    pub fn reset_for_gui_start() -> io::Result<()> {
        let handle = create_event()?;
        let result = unsafe { ResetEvent(handle) };
        let close_result = unsafe { CloseHandle(handle) };
        if result == 0 {
            return Err(last_error("ResetEvent"));
        }
        if close_result == 0 {
            return Err(last_error("CloseHandle"));
        }
        Ok(())
    }

    pub fn signal_manual_shutdown() -> io::Result<()> {
        let handle = create_event()?;
        let result = unsafe { SetEvent(handle) };
        let close_result = unsafe { CloseHandle(handle) };
        if result == 0 {
            return Err(last_error("SetEvent"));
        }
        if close_result == 0 {
            return Err(last_error("CloseHandle"));
        }
        Ok(())
    }

    pub fn spawn_native_exit_monitor() -> io::Result<JoinHandle<()>> {
        let handle = open_event_for_modify_and_wait()?;
        let handle_value = handle as isize;
        thread::Builder::new()
            .name("curl-downloader-native-exit-monitor".into())
            .spawn(move || {
                let handle = handle_value as HANDLE;
                let result = unsafe { WaitForSingleObject(handle, INFINITE) };
                if result == WAIT_OBJECT_0 {
                    std::process::exit(0);
                }
                unsafe {
                    CloseHandle(handle);
                }
            })
            .map_err(|error| {
                io::Error::other(format!("無法啟動 Native host event monitor：{error}"))
            })
    }

    #[cfg(test)]
    pub struct TestEvent {
        handle: HANDLE,
    }

    #[cfg(test)]
    impl TestEvent {
        pub fn open() -> io::Result<Self> {
            Ok(Self {
                handle: open_event_for_modify_and_wait()?,
            })
        }

        pub fn is_signaled(&self, timeout: Duration) -> io::Result<bool> {
            let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
            Ok(unsafe { WaitForSingleObject(self.handle, timeout_ms) } == WAIT_OBJECT_0)
        }
    }

    #[cfg(test)]
    impl Drop for TestEvent {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(not(windows))]
mod windows_impl {
    use super::EVENT_NAME;
    #[cfg(test)]
    use std::time::Duration;
    use std::{io, thread};

    pub fn reset_for_gui_start() -> io::Result<()> {
        let _ = EVENT_NAME;
        Ok(())
    }

    pub fn signal_manual_shutdown() -> io::Result<()> {
        let _ = EVENT_NAME;
        Ok(())
    }

    pub fn spawn_native_exit_monitor() -> io::Result<std::thread::JoinHandle<()>> {
        thread::Builder::new()
            .name("curl-downloader-native-exit-monitor".into())
            .spawn(|| {})
            .map_err(io::Error::other)
    }

    #[cfg(test)]
    pub struct TestEvent;

    #[cfg(test)]
    impl TestEvent {
        pub fn open() -> io::Result<Self> {
            Ok(Self)
        }

        pub fn is_signaled(&self, _timeout: Duration) -> io::Result<bool> {
            Ok(false)
        }
    }
}

pub fn reset_for_gui_start() -> io::Result<()> {
    windows_impl::reset_for_gui_start()
}

pub fn signal_manual_shutdown() -> io::Result<()> {
    windows_impl::signal_manual_shutdown()
}

pub fn spawn_native_exit_monitor() -> io::Result<JoinHandle<()>> {
    windows_impl::spawn_native_exit_monitor()
}

#[cfg(test)]
pub fn open_event() -> io::Result<windows_impl::TestEvent> {
    windows_impl::TestEvent::open()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn manual_shutdown_event_is_shared_and_resettable() {
        reset_for_gui_start().unwrap();
        let waiter = open_event().unwrap();
        assert!(!waiter.is_signaled(Duration::from_millis(1)).unwrap());
        signal_manual_shutdown().unwrap();
        assert!(waiter.is_signaled(Duration::from_millis(100)).unwrap());
        reset_for_gui_start().unwrap();
        assert!(!waiter.is_signaled(Duration::from_millis(1)).unwrap());
    }
}
