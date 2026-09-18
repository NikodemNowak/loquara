//! API keys at rest.
//!
//! Provider keys live in the app's SQLite file. On Windows they are wrapped
//! with DPAPI before being written, so the file is only usable by the Windows
//! account that wrote it: copying `loquara.sqlite3` to another machine — or
//! reading it from a backup — yields ciphertext. Elsewhere the value is stored
//! as-is, which is no worse than the settings around it.
//!
//! The stored form is hex so the whole thing stays valid JSON (and readable in
//! a SQLite browser, where a wall of hex at least says "this is a secret").

/// Encrypts one key for storage.
pub fn protect(plain: &str) -> Result<String, String> {
    platform::wrap(plain.as_bytes()).map(|bytes| hex_encode(&bytes))
}

/// Decrypts a stored key.
pub fn unprotect(stored: &str) -> Result<String, String> {
    let bytes = hex_decode(stored)?;
    let plain = platform::unwrap(&bytes)?;
    String::from_utf8(plain).map_err(|_| "the stored key is not valid text".to_owned())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("the stored key is not valid".to_owned());
    }
    (0..text.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&text[index..index + 2], 16)
                .map_err(|_| "the stored key is not valid".to_owned())
        })
        .collect()
}

#[cfg(windows)]
mod platform {
    //! DPAPI, through the same DLLs the operating system uses for saved
    //! passwords and the credential manager.

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
    };

    /// Never show a prompt: Loquara can be mid-dictation with no window to
    /// draw one in.
    const UI_FORBIDDEN: u32 = 0x1;

    pub fn wrap(data: &[u8]) -> Result<Vec<u8>, String> {
        if data.is_empty() {
            return Err("there is no key to protect".to_owned());
        }
        unsafe {
            let input = blob(data);
            let mut output = empty_blob();
            let ok = CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                UI_FORBIDDEN,
                &mut output,
            );
            take(output, ok, "encrypt")
        }
    }

    pub fn unwrap(data: &[u8]) -> Result<Vec<u8>, String> {
        if data.is_empty() {
            return Err("the stored key is empty".to_owned());
        }
        unsafe {
            let input = blob(data);
            let mut output = empty_blob();
            let ok = CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                UI_FORBIDDEN,
                &mut output,
            );
            take(output, ok, "decrypt")
        }
    }

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    fn empty_blob() -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        }
    }

    /// Copies an output blob out and hands its memory back to Windows.
    unsafe fn take(output: CRYPT_INTEGER_BLOB, ok: i32, verb: &str) -> Result<Vec<u8>, String> {
        if ok == 0 {
            return Err(format!(
                "Windows could not {verb} the key: {}",
                std::io::Error::last_os_error()
            ));
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
        };
        unsafe {
            LocalFree(output.pbData as *mut core::ffi::c_void);
        }
        Ok(bytes)
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn wrap(data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }

    pub fn unwrap(data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_survives_a_round_trip() {
        let stored = protect("xai-1234567890abcdef").unwrap();

        assert_ne!(stored, "xai-1234567890abcdef");
        assert!(stored.chars().all(|character| character.is_ascii_hexdigit()));
        assert_eq!(unprotect(&stored).unwrap(), "xai-1234567890abcdef");
    }

    #[test]
    fn an_empty_key_is_refused() {
        assert!(protect("").is_err());
    }

    #[test]
    fn a_stored_value_that_is_not_hex_is_refused() {
        assert!(unprotect("not-hex!").is_err());
        assert!(unprotect("abc").is_err());
    }

    #[test]
    fn a_tampered_value_does_not_come_back_as_text() {
        let stored = protect("sk-test").unwrap();
        let mut tampered = stored.clone();
        tampered.replace_range(0..2, "00");

        // DPAPI authentication catches the change on Windows; the plain
        // fallback has nothing to catch, so only the happy path is required
        // to hold everywhere.
        if cfg!(windows) {
            assert!(unprotect(&tampered).is_err());
        }
    }
}
