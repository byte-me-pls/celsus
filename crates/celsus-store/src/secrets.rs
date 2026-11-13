//! API keys live in the operating system's credential store (Windows Credential Manager, macOS Keychain, the
//! Secret Service on Linux), never in the database or a settings file.

use std::collections::HashMap;
use std::sync::Mutex;

const SERVICE: &str = "Celsus";

/// The keys read so far in this run, and the connections known to have none. The store is asked once a run for
/// each: on a Mac, every reading by a version of the app that the keychain does not trust yet asks the user,
/// and trying sixty models would ask sixty times.
static READ: Mutex<Option<HashMap<i64, Option<String>>>> = Mutex::new(None);

fn remember(provider: i64, key: Option<String>) {
    let mut read = READ.lock().unwrap_or_else(|e| e.into_inner());
    read.get_or_insert_with(HashMap::new).insert(provider, key);
}

fn entry(provider: i64) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, &format!("provider-{provider}")).map_err(|e| e.to_string())
}

pub fn set_key(provider: i64, key: &str) -> Result<(), String> {
    let e = entry(provider)?;
    if key.is_empty() {
        return match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {
                remember(provider, None);
                Ok(())
            }
            Err(err) => Err(err.to_string()),
        };
    }
    e.set_password(key).map_err(|e| e.to_string())?;
    remember(provider, Some(key.to_owned()));
    Ok(())
}

pub fn get_key(provider: i64) -> Option<String> {
    read_key(provider).ok().flatten()
}

/// The key of a connection, `None` when none was stored, and an error when the store would not give it: on a
/// Mac, for one, the keychain asks again after an update before it lets the new version read what the old one
/// stored, and an answer of no, or no answer, is such an error.
/// What the store would not give is asked for again next time: the user may say yes then.
pub fn read_key(provider: i64) -> Result<Option<String>, String> {
    if let Some(Some(known)) = READ
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|read| read.get(&provider).cloned())
    {
        return Ok(known);
    }
    let key = match entry(provider)?.get_password() {
        Ok(key) => Some(key).filter(|k| !k.is_empty()),
        Err(keyring::Error::NoEntry) => None,
        Err(e) => return Err(e.to_string()),
    };
    remember(provider, key.clone());
    Ok(key)
}

pub fn delete_key(provider: i64) {
    if let Ok(e) = entry(provider) {
        let _ = e.delete_credential();
    }
    if let Some(read) = READ.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        read.remove(&provider);
    }
}
