//! Where the API key and the mail password are kept: the system keychain in the installed
//! app, so they are encrypted at rest and not in a file a backup or a sync folder carries off.
//!
//! The keychain ties an item to the signature of the program that wrote it. A development
//! build gets a new signature every time it is compiled and would be asked about on every
//! run, so those builds (and the tests) keep the secrets in `settings.json` as before.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Mutex;

pub const LLM_API_KEY: &str = "llm_api_key";
pub const EMAIL_PASSWORD: &str = "email_password";

pub trait SecretStore: Debug + Send + Sync {
    fn get(&self, name: &str) -> Option<String>;
    /// An empty value removes the secret.
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
}

/// The login keychain, under the app's name. What was read once is remembered while the app
/// runs, so macOS asks about a secret once per launch at most, however often it is needed.
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
pub struct Keychain {
    seen: Mutex<HashMap<String, String>>,
}

#[cfg(target_os = "macos")]
impl Keychain {
    const SERVICE: &str = "com.zillanote.lite";
    /// `errSecItemNotFound`: nothing to delete is not a failure.
    const NOT_FOUND: i32 = -25300;
}

#[cfg(target_os = "macos")]
impl SecretStore for Keychain {
    fn get(&self, name: &str) -> Option<String> {
        let mut seen = self.seen.lock().ok()?;
        if let Some(value) = seen.get(name) {
            return Some(value.clone());
        }
        let bytes = security_framework::passwords::get_generic_password(Self::SERVICE, name).ok()?;
        let value = String::from_utf8(bytes).ok()?;
        seen.insert(name.to_string(), value.clone());
        Some(value)
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        let mut seen = self.seen.lock().map_err(|e| e.to_string())?;
        if seen.get(name).is_some_and(|known| known == value) {
            return Ok(());
        }
        let result = if value.is_empty() {
            match security_framework::passwords::delete_generic_password(Self::SERVICE, name) {
                Err(error) if error.code() != Self::NOT_FOUND => Err(error),
                _ => Ok(()),
            }
        } else {
            security_framework::passwords::set_generic_password(Self::SERVICE, name, value.as_bytes())
        };
        result.map_err(|error| format!("The keychain refused: {error}"))?;
        seen.insert(name.to_string(), value.to_string());
        Ok(())
    }
}

/// The keychain, where this build should use it.
pub fn system() -> Option<std::sync::Arc<dyn SecretStore>> {
    #[cfg(all(target_os = "macos", not(debug_assertions)))]
    return Some(std::sync::Arc::new(Keychain::default()));
    #[cfg(not(all(target_os = "macos", not(debug_assertions))))]
    None
}

/// For tests: secrets in memory, or a store that refuses everything.
#[derive(Debug, Default)]
pub struct MemorySecrets {
    pub values: Mutex<HashMap<String, String>>,
    pub broken: bool,
}

impl SecretStore for MemorySecrets {
    fn get(&self, name: &str) -> Option<String> {
        self.values.lock().ok()?.get(name).cloned()
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        if self.broken {
            return Err("locked".to_string());
        }
        let mut values = self.values.lock().map_err(|e| e.to_string())?;
        match value {
            "" => values.remove(name),
            value => values.insert(name.to_string(), value.to_string()),
        };
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Writes, reads and removes a throwaway item in the login keychain:
    ///
    /// cargo test -p engine live_keychain -- --ignored --nocapture
    #[test]
    #[ignore = "touches the login keychain"]
    fn live_keychain_keeps_changes_and_forgets_a_secret() {
        const NAME: &str = "test-secret";
        let keychain = Keychain::default();

        keychain.set(NAME, "first").unwrap();
        keychain.set(NAME, "second").unwrap();
        // A fresh instance has nothing remembered: this read goes to the keychain itself.
        assert_eq!(Keychain::default().get(NAME).as_deref(), Some("second"));

        keychain.set(NAME, "").unwrap();
        assert_eq!(Keychain::default().get(NAME), None);
        // Removing what is not there is not an error.
        Keychain::default().set(NAME, "").unwrap();
    }
}
