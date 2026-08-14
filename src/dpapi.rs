//! Small, non-interactive Windows DPAPI wrapper used for Firefox request
//! contexts.  The default DPAPI scope is CurrentUser; this module deliberately
//! never opts into the machine-wide scope.

use std::io;
use zeroize::Zeroizing;

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use super::*;
    use std::{ffi::c_void, ptr::null_mut};
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    struct OutputBlob(CRYPT_INTEGER_BLOB);

    impl Default for OutputBlob {
        fn default() -> Self {
            Self(CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: null_mut(),
            })
        }
    }

    impl Drop for OutputBlob {
        fn drop(&mut self) {
            if self.0.pbData.is_null() {
                return;
            }
            // DPAPI allocates the output with LocalAlloc.  The unprotect path
            // contains plaintext, so wipe it before returning the allocation.
            unsafe {
                std::ptr::write_bytes(self.0.pbData, 0, self.0.cbData as usize);
                let _ = LocalFree(self.0.pbData.cast::<c_void>());
            }
        }
    }

    fn copy_blob(blob: &CRYPT_INTEGER_BLOB) -> io::Result<Vec<u8>> {
        if blob.pbData.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "DPAPI returned an empty buffer",
            ));
        }
        // SAFETY: the Win32 call reported cbData and returned a non-null buffer
        // owned by OutputBlob, which remains alive for this copy.
        Ok(unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec())
    }

    pub fn protect_current_user(plaintext: &[u8]) -> io::Result<Vec<u8>> {
        let length = u32::try_from(plaintext.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "DPAPI input is too large"))?;
        let input = CRYPT_INTEGER_BLOB {
            cbData: length,
            pbData: plaintext.as_ptr().cast_mut(),
        };
        let mut output = OutputBlob::default();
        // SAFETY: input references the caller-owned bytes for the duration of
        // the call.  DPAPI allocates output and OutputBlob frees it exactly
        // once.  No optional entropy or machine-scope flag is supplied.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        copy_blob(&output.0)
    }

    pub fn unprotect_current_user(ciphertext: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
        let length = u32::try_from(ciphertext.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "DPAPI input is too large"))?;
        let input = CRYPT_INTEGER_BLOB {
            cbData: length,
            pbData: ciphertext.as_ptr().cast_mut(),
        };
        let mut output = OutputBlob::default();
        // SAFETY: input references ciphertext for the duration of the call and
        // output is owned by OutputBlob.  UI prompts are explicitly forbidden.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        Ok(Zeroizing::new(copy_blob(&output.0)?))
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn protect_current_user(_plaintext: &[u8]) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows DPAPI is unavailable on this platform",
        ))
    }

    pub fn unprotect_current_user(_ciphertext: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows DPAPI is unavailable on this platform",
        ))
    }
}

pub use platform::{protect_current_user, unprotect_current_user};

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn dpapi_round_trip_does_not_embed_plaintext() {
        let plaintext = b"Cookie: session=super-secret";
        let encrypted = protect_current_user(plaintext).unwrap();
        assert!(
            !encrypted
                .windows(plaintext.len())
                .any(|window| window == plaintext)
        );
        assert_eq!(&*unprotect_current_user(&encrypted).unwrap(), plaintext);
    }

    #[test]
    fn tampered_dpapi_blob_is_rejected() {
        let mut encrypted = protect_current_user(b"secret").unwrap();
        let midpoint = encrypted.len() / 2;
        encrypted[midpoint] ^= 0x55;
        assert!(unprotect_current_user(&encrypted).is_err());
    }
}
