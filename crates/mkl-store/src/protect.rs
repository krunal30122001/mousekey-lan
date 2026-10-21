//! At-rest protection for the device private key.
//! Windows: DPAPI (`CryptProtectData`), bound to the signed-in user account.
//! Other platforms: stored as-is for now (keychain / libsecret integration is future work).

use crate::{Result, StoreError};

#[cfg(windows)]
pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
    dpapi(data, true)
}

#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
    dpapi(data, false)
}

#[cfg(windows)]
fn dpapi(data: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};
    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
        let ok = if encrypt {
            CryptProtectData(&input, null(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        } else {
            CryptUnprotectData(&input, null_mut(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        };
        if ok == 0 {
            return Err(StoreError::Protect(format!("DPAPI call failed (os error {})", GetLastError())));
        }
        let out = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as _);
        Ok(out)
    }
}

#[cfg(not(windows))]
pub fn protect(_data: &[u8]) -> Result<Vec<u8>> {
    Err(StoreError::Protect("no OS key protection available".into()))
}

#[cfg(not(windows))]
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn dpapi_roundtrip() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let p = super::protect(secret).unwrap();
        assert_ne!(&p[..], &secret[..]);
        assert_eq!(super::unprotect(&p).unwrap(), secret);
    }
}
