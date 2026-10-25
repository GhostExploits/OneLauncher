//! Token, pairing-code, and Discord interaction-signature primitives.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ED25519, UnparsedPublicKey};
use subtle::ConstantTimeEq;

/// 32 unambiguous characters. 256 is exactly divisible by 32, so mapping each
/// random byte with modulo introduces no bias.
const PAIR_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const PAIR_CODE_LEN: usize = 12;
const DISCORD_MAX_SKEW_SECONDS: i64 = 300;
const DPAPI_PREFIX: &str = "dpapi:v1:";
const PLAIN_PREFIX: &str = "plain:v1:";

/// Protect a credential at rest.
///
/// Windows uses the current user's DPAPI key, so copying `agent.json` to
/// another account or machine does not reveal the device token. Other
/// platforms retain the existing `0600` file boundary and mark the value as
/// plaintext so the limitation is explicit rather than implied.
pub fn protect_secret(plaintext: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        let encrypted = dpapi_protect(plaintext.as_bytes())?;
        Ok(format!("{DPAPI_PREFIX}{}", STANDARD.encode(encrypted)))
    }
    #[cfg(not(windows))]
    {
        Ok(format!("{PLAIN_PREFIX}{plaintext}"))
    }
}

/// Read a protected credential, accepting the pre-DPAPI plaintext format so an
/// existing paired agent keeps working and is upgraded on its next save.
pub fn unprotect_secret(stored: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        if let Some(encoded) = stored.strip_prefix(DPAPI_PREFIX) {
            let encrypted = STANDARD
                .decode(encoded)
                .map_err(|_| "the stored device credential is not valid base64".to_string())?;
            let plaintext = dpapi_unprotect(&encrypted)?;
            return String::from_utf8(plaintext)
                .map_err(|_| "the stored device credential is not valid UTF-8".to_string());
        }
        if let Some(plaintext) = stored.strip_prefix(PLAIN_PREFIX) {
            return Ok(plaintext.to_string());
        }
        // Version 1 migration path: a config written before at-rest protection.
        Ok(stored.to_string())
    }
    #[cfg(not(windows))]
    {
        Ok(stored
            .strip_prefix(PLAIN_PREFIX)
            .unwrap_or(stored)
            .to_string())
    }
}

#[cfg(windows)]
fn dpapi_protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: plaintext
            .len()
            .try_into()
            .map_err(|_| "the device credential is too large".to_string())?,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!(
            "Windows could not protect the device credential: {}",
            std::io::Error::last_os_error()
        ));
    }
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    Ok(result)
}

#[cfg(windows)]
fn dpapi_unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: ciphertext
            .len()
            .try_into()
            .map_err(|_| "the stored device credential is too large".to_string())?,
        pbData: ciphertext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(
            "Windows could not unprotect the device credential; pair this computer again"
                .to_string(),
        );
    }
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    Ok(result)
}

pub fn hash_secret(secret: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, secret.as_bytes());
    hex::encode(digest.as_ref())
}

pub fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// 256 bits of OS randomness, URL-safe and padded-free.
pub fn new_device_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "the operating system random source failed".to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn new_pairing_code() -> Result<String, String> {
    let mut bytes = [0u8; PAIR_CODE_LEN];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "the operating system random source failed".to_string())?;
    let code: String = bytes
        .iter()
        .map(|byte| PAIR_ALPHABET[(*byte as usize) % PAIR_ALPHABET.len()] as char)
        .collect();
    Ok(format!("{}-{}-{}", &code[0..4], &code[4..8], &code[8..12]))
}

pub fn normalize_pairing_code(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_uppercase)
        .collect()
}

pub fn bearer_token(header: Option<&str>) -> Option<&str> {
    let value = header?;
    let token = value.strip_prefix("Bearer ")?;
    let token = token.trim();
    if token.is_empty() || token.contains(' ') {
        return None;
    }
    Some(token)
}

/// Verify Discord's `X-Signature-Ed25519` over `timestamp + body`.
///
/// The timestamp is checked to limit replay. Discord retries are normally
/// immediate; a five-minute window is generous without accepting old captures.
pub fn verify_discord_signature(
    public_key_hex: &str,
    signature_hex: &str,
    timestamp: &str,
    body: &[u8],
    now_unix: i64,
) -> bool {
    let Ok(public_key) = hex::decode(public_key_hex) else {
        return false;
    };
    let Ok(signature) = hex::decode(signature_hex) else {
        return false;
    };
    if public_key.len() != 32 || signature.len() != 64 {
        return false;
    }
    let Ok(timestamp) = timestamp.parse::<i64>() else {
        return false;
    };
    if (now_unix - timestamp).abs() > DISCORD_MAX_SKEW_SECONDS {
        return false;
    }

    let mut message = Vec::with_capacity(timestamp.to_string().len() + body.len());
    message.extend_from_slice(timestamp.to_string().as_bytes());
    message.extend_from_slice(body);

    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&message, &signature)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    #[test]
    fn secrets_are_unique_and_hashed() {
        let a = new_device_token().unwrap();
        let b = new_device_token().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        let hash = hash_secret(&a);
        assert_eq!(hash.len(), 64);
        assert!(constant_time_eq(&hash, &hash_secret(&a)));
        assert!(!constant_time_eq(&hash, &hash_secret(&b)));
    }

    #[test]
    fn protected_secrets_round_trip_and_accept_legacy_plaintext() {
        let token = "c".repeat(43);
        let stored = protect_secret(&token).unwrap();
        assert_ne!(stored, token, "the stored form must be marked");
        assert_eq!(unprotect_secret(&stored).unwrap(), token);
        // Configs written before at-rest protection contain the bare token.
        assert_eq!(unprotect_secret(&token).unwrap(), token);
    }

    #[cfg(windows)]
    #[test]
    fn corrupt_windows_blobs_are_rejected_not_treated_as_plaintext() {
        assert!(unprotect_secret("dpapi:v1:not-base64!!").is_err());
    }

    #[test]
    fn pairing_codes_are_readable_and_normalized() {
        let code = new_pairing_code().unwrap();
        assert_eq!(code.len(), 14);
        assert_eq!(code.chars().filter(|c| *c == '-').count(), 2);
        assert_eq!(
            normalize_pairing_code(&code.to_lowercase()),
            normalize_pairing_code(&code)
        );
        assert!(
            code.chars()
                .filter(|c| *c != '-')
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        );
    }

    #[test]
    fn bearer_header_is_strict() {
        assert_eq!(bearer_token(Some("Bearer abc")), Some("abc"));
        assert_eq!(bearer_token(Some("bearer abc")), None);
        assert_eq!(bearer_token(Some("Bearer ")), None);
        assert_eq!(bearer_token(Some("Bearer a b")), None);
        assert_eq!(bearer_token(None), None);
    }

    #[test]
    fn discord_signature_round_trip_and_replay_window() {
        let rng = SystemRandom::new();
        let document = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(document.as_ref()).unwrap();
        let public = hex::encode(pair.public_key().as_ref());
        let timestamp = "1_700_000_000".replace('_', "");
        let body = br#"{"type":1}"#;
        let mut message = timestamp.clone().into_bytes();
        message.extend_from_slice(body);
        let signature = hex::encode(pair.sign(&message).as_ref());

        assert!(verify_discord_signature(
            &public,
            &signature,
            &timestamp,
            body,
            1_700_000_010
        ));
        assert!(!verify_discord_signature(
            &public,
            &signature,
            &timestamp,
            br#"{"type":2}"#,
            1_700_000_010
        ));
        assert!(!verify_discord_signature(
            &public,
            &signature,
            &timestamp,
            body,
            1_700_000_000 + DISCORD_MAX_SKEW_SECONDS + 1
        ));
    }
}
