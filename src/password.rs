//! Passwords, for servers without a domain name ("IP seule"): passkeys need a domain, so
//! there the owner signs in with a password. PBKDF2-HMAC-SHA256 through OpenSSL (already a
//! dependency), and a per-address limit on wrong attempts.

use base64::Engine;
use std::collections::HashMap;
use std::sync::Mutex;

const ITERATIONS: usize = 600_000;
pub const MIN_LEN: usize = 12;

/// "pbkdf2-sha256$600000$<salt>$<hash>", base64 without padding.
pub fn hash(password: &str) -> anyhow::Result<String> {
    let mut salt = [0u8; 16];
    openssl::rand::rand_bytes(&mut salt)?;
    let key = derive(password, &salt, ITERATIONS)?;
    let b64 = base64::engine::general_purpose::STANDARD_NO_PAD;
    Ok(format!("pbkdf2-sha256${ITERATIONS}${}${}", b64.encode(salt), b64.encode(key)))
}

pub fn verify(password: &str, stored: &str) -> bool {
    let b64 = base64::engine::general_purpose::STANDARD_NO_PAD;
    let mut parts = stored.split('$');
    let (Some("pbkdf2-sha256"), Some(iter), Some(salt), Some(key), None) = (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let (Ok(iter), Ok(salt), Ok(key)) = (iter.parse::<usize>(), b64.decode(salt), b64.decode(key)) else { return false };
    match derive(password, &salt, iter) {
        Ok(k) => k.len() == key.len() && openssl::memcmp::eq(&k, &key),
        Err(_) => false,
    }
}

fn derive(password: &str, salt: &[u8], iterations: usize) -> anyhow::Result<[u8; 32]> {
    let mut key = [0u8; 32];
    openssl::pkcs5::pbkdf2_hmac(password.as_bytes(), salt, iterations, openssl::hash::MessageDigest::sha256(), &mut key)?;
    Ok(key)
}

/// A password DevHub accepts: long enough, not only spaces.
pub fn check_new(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_LEN || password.trim().is_empty() {
        return Err(format!("mot de passe trop court : {MIN_LEN} caractères minimum"));
    }
    Ok(())
}

/// Wrong attempts per address: after `MAX_FAILS` within the window, that address waits.
const MAX_FAILS: u32 = 5;
const WINDOW: i64 = 15 * 60;
static FAILS: Mutex<Option<HashMap<String, (u32, i64)>>> = Mutex::new(None);

/// Seconds to wait before this address may try again (0 = it may).
pub fn wait_for(ip: &str, now: i64) -> i64 {
    let mut g = FAILS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    m.retain(|_, (_, since)| now - *since < WINDOW);
    match m.get(ip) {
        Some((n, since)) if *n >= MAX_FAILS => WINDOW - (now - since),
        _ => 0,
    }
}

pub fn failed(ip: &str, now: i64) {
    let mut g = FAILS.lock().unwrap();
    let e = g.get_or_insert_with(HashMap::new).entry(ip.to_string()).or_insert((0, now));
    e.0 += 1;
}

pub fn succeeded(ip: &str) {
    if let Some(m) = FAILS.lock().unwrap().as_mut() {
        m.remove(ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let h = hash("correct horse battery").unwrap();
        assert!(h.starts_with("pbkdf2-sha256$600000$"));
        assert!(verify("correct horse battery", &h));
        assert!(!verify("correct horse batterY", &h));
        assert!(!verify("x", "garbage"));
        assert_ne!(hash("same").unwrap(), hash("same").unwrap());
        assert!(check_new("trop court").is_err() && check_new("assez long pour passer").is_ok());
    }

    #[test]
    fn rate_limit() {
        let ip = "198.51.100.9";
        for _ in 0..MAX_FAILS {
            assert_eq!(wait_for(ip, 1000), 0);
            failed(ip, 1000);
        }
        assert!(wait_for(ip, 1001) > 0);
        assert_eq!(wait_for(ip, 1000 + WINDOW), 0);
        succeeded(ip);
    }
}
