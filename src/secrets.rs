use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use keyring::Entry;
use thiserror::Error;

const SERVICE_NAME: &str = "model-gateway";

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("secret name is not a safe identifier: {0}")]
    InvalidName(String),
    #[error("secret store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("keychain operation failed: {0}")]
    Keychain(String),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, name: &str, value: &str) -> Result<(), SecretError>;
    fn remove(&self, name: &str) -> Result<(), SecretError>;
    fn source(&self) -> &'static str;
}

#[derive(Debug, Clone)]
pub struct FileSecretStore {
    root: PathBuf,
}

impl FileSecretStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, name: &str) -> Result<PathBuf, SecretError> {
        validate_secret_name(name)?;
        Ok(self.root.join(name))
    }

    fn ensure_root(&self) -> Result<(), SecretError> {
        fs::create_dir_all(&self.root)?;
        set_unix_mode(&self.root, 0o700)?;
        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        let path = self.path(name)?;
        match fs::read_to_string(path) {
            Ok(value) => Ok(Some(value.trim_end_matches(['\r', '\n']).to_owned())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.ensure_root()?;
        let path = self.path(name)?;
        let temporary = path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        set_unix_mode(&temporary, 0o600)?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), SecretError> {
        let path = self.path(name)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn source(&self) -> &'static str {
        "protected-file"
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EnvironmentSecretStore;

impl SecretStore for EnvironmentSecretStore {
    fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        validate_secret_name(name)?;
        Ok(env::var(name).ok())
    }

    fn set(&self, _name: &str, _value: &str) -> Result<(), SecretError> {
        Err(SecretError::Keychain(
            "environment secrets cannot be persisted by the gateway".to_owned(),
        ))
    }

    fn remove(&self, _name: &str) -> Result<(), SecretError> {
        Err(SecretError::Keychain(
            "environment secrets cannot be removed by the gateway".to_owned(),
        ))
    }

    fn source(&self) -> &'static str {
        "environment"
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct KeychainSecretStore;

impl KeychainSecretStore {
    fn entry(name: &str) -> Result<Entry, SecretError> {
        validate_secret_name(name)?;
        Entry::new(SERVICE_NAME, name).map_err(|error| SecretError::Keychain(error.to_string()))
    }
}

fn is_missing_keychain_error(error: &keyring::Error) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("no entry")
        || message.contains("not found")
        || message.contains("could not be found")
        || message.contains("no such")
}

impl SecretStore for KeychainSecretStore {
    fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        match Self::entry(name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(error) if is_missing_keychain_error(&error) => Ok(None),
            Err(error) => Err(SecretError::Keychain(error.to_string())),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        Self::entry(name)?
            .set_password(value)
            .map_err(|error| SecretError::Keychain(error.to_string()))
    }

    fn remove(&self, name: &str) -> Result<(), SecretError> {
        match Self::entry(name)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(error) if is_missing_keychain_error(&error) => Ok(()),
            Err(error) => Err(SecretError::Keychain(error.to_string())),
        }
    }

    fn source(&self) -> &'static str {
        "os-keychain"
    }
}

#[derive(Debug)]
pub struct SecretResolver {
    pub environment: EnvironmentSecretStore,
    pub files: Option<FileSecretStore>,
    pub keychain: Option<KeychainSecretStore>,
}

impl Default for SecretResolver {
    fn default() -> Self {
        Self {
            environment: EnvironmentSecretStore,
            files: env::var_os("MODEL_GATEWAY_SECRET_DIR").map(FileSecretStore::new),
            keychain: Some(KeychainSecretStore),
        }
    }
}

impl SecretResolver {
    pub fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        if let Some(value) = self.environment.get(name)? {
            return Ok(Some(value));
        }
        let file_value = match &self.files {
            Some(files) => files.get(name)?,
            None => None,
        };
        if let Some(value) = file_value {
            return Ok(Some(value));
        }
        match &self.keychain {
            Some(keychain) => keychain.get(name),
            None => Ok(None),
        }
    }

    pub fn source(&self, name: &str) -> Result<Option<&'static str>, SecretError> {
        if self.environment.get(name)?.is_some() {
            return Ok(Some(self.environment.source()));
        }
        let file_source = match &self.files {
            Some(files) if files.get(name)?.is_some() => Some(files.source()),
            _ => None,
        };
        if let Some(source) = file_source {
            return Ok(Some(source));
        }
        let keychain_source = match &self.keychain {
            Some(keychain) if keychain.get(name)?.is_some() => Some(keychain.source()),
            _ => None,
        };
        if let Some(source) = keychain_source {
            return Ok(Some(source));
        }
        Ok(None)
    }

    pub fn set_preferred(&self, name: &str, value: &str) -> Result<&'static str, SecretError> {
        if let Some(files) = &self.files {
            files.set(name, value)?;
            return Ok(files.source());
        }
        if let Some(keychain) = &self.keychain {
            keychain.set(name, value)?;
            return Ok(keychain.source());
        }
        Err(SecretError::Keychain(
            "no writable secret store is configured".to_owned(),
        ))
    }

    pub fn remove(&self, name: &str) -> Result<(), SecretError> {
        if let Some(files) = &self.files {
            files.remove(name)?;
        }
        if let Some(keychain) = &self.keychain {
            keychain.remove(name)?;
        }
        Ok(())
    }
}

pub fn validate_secret_name(name: &str) -> Result<(), SecretError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(SecretError::InvalidName(name.to_owned()));
    }
    Ok(())
}

fn set_unix_mode(path: &Path, mode: u32) -> Result<(), SecretError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FileSecretStore, SecretStore, validate_secret_name};

    #[test]
    fn rejects_path_traversal_names() {
        assert!(validate_secret_name("../secret").is_err());
        assert!(validate_secret_name("OPENROUTER_API_KEY").is_ok());
    }

    #[test]
    fn file_store_round_trips_without_newline() {
        let root = tempfile::tempdir().expect("tempdir");
        let store = FileSecretStore::new(root.path());
        store.set("TEST_KEY", "secret\n").expect("set");
        assert_eq!(
            store.get("TEST_KEY").expect("get"),
            Some("secret".to_owned())
        );
        store.remove("TEST_KEY").expect("remove");
        assert_eq!(store.get("TEST_KEY").expect("get"), None);
    }
}
