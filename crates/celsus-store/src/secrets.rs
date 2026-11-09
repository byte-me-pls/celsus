//! API keys live in the operating system's credential store (Windows Credential Manager, macOS Keychain, the
//! Secret Service on Linux), never in the database or a settings file.

const SERVICE: &str = "Celsus";

fn entry(provider: i64) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, &format!("provider-{provider}")).map_err(|e| e.to_string())
}

pub fn set_key(provider: i64, key: &str) -> Result<(), String> {
    let e = entry(provider)?;
    if key.is_empty() {
        return match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.to_string()),
        };
    }
    e.set_password(key).map_err(|e| e.to_string())
}

pub fn get_key(provider: i64) -> Option<String> {
    entry(provider)
        .ok()?
        .get_password()
        .ok()
        .filter(|k| !k.is_empty())
}

pub fn delete_key(provider: i64) {
    if let Ok(e) = entry(provider) {
        let _ = e.delete_credential();
    }
}
