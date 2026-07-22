use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use keyring::Entry;
use thiserror::Error;

use crate::storage::write_atomic;

const SERVICE_NAME: &str = "model-gateway";

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("secret name is not a safe identifier: {0}")]
    InvalidName(String),
    #[error("secret store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("keychain operation failed: {0}")]
    Keychain(String),
    #[error("invalid MODEL_GATEWAY_SECRET_STORE value: {0}")]
    InvalidStore(String),
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
        write_atomic(&path, value.as_bytes())?;
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
        || message.contains("no matching entry")
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

pub struct SecretResolver {
    pub environment: EnvironmentSecretStore,
    files: Option<Box<dyn SecretStore>>,
    keychain: Option<Box<dyn SecretStore>>,
    initialization_error: Option<String>,
}

impl Default for SecretResolver {
    fn default() -> Self {
        let mode = env::var("MODEL_GATEWAY_SECRET_STORE").ok();
        let configured_files = env::var_os("MODEL_GATEWAY_SECRET_DIR")
            .map(|path| Box::new(FileSecretStore::new(path)) as Box<dyn SecretStore>);
        let (files, keychain, initialization_error) =
            match mode.as_deref() {
                None | Some("keychain") => (
                    configured_files,
                    Some(Box::new(KeychainSecretStore) as Box<dyn SecretStore>),
                    None,
                ),
                Some("file") => (
                    Some(configured_files.unwrap_or_else(|| {
                        Box::new(FileSecretStore::new(default_file_store_root()))
                    })),
                    None,
                    None,
                ),
                Some("environment") => (None, None, None),
                Some(value) => (None, None, Some(value.to_owned())),
            };
        Self {
            environment: EnvironmentSecretStore,
            files,
            keychain,
            initialization_error,
        }
    }
}

impl SecretResolver {
    #[cfg(test)]
    fn with_stores(
        files: Option<Box<dyn SecretStore>>,
        keychain: Option<Box<dyn SecretStore>>,
    ) -> Self {
        Self {
            environment: EnvironmentSecretStore,
            files,
            keychain,
            initialization_error: None,
        }
    }

    fn check_initialized(&self) -> Result<(), SecretError> {
        match &self.initialization_error {
            Some(value) => Err(SecretError::InvalidStore(value.clone())),
            None => Ok(()),
        }
    }

    pub fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        self.check_initialized()?;
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
        self.check_initialized()?;
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
        self.check_initialized()?;
        if let Some(files) = &self.files {
            files.set(name, value)?;
            return Ok(files.source());
        }
        if let Some(keychain) = &self.keychain {
            keychain.set(name, value).map_err(|error| {
                SecretError::Keychain(format!(
                    "{error}; choose MODEL_GATEWAY_SECRET_STORE=file or environment explicitly"
                ))
            })?;
            return Ok(keychain.source());
        }
        Err(SecretError::Keychain(
            "environment-only mode cannot persist credentials; export the named variable or choose MODEL_GATEWAY_SECRET_STORE=file".to_owned(),
        ))
    }

    pub fn remove(&self, name: &str) -> Result<(), SecretError> {
        self.check_initialized()?;
        if let Some(files) = &self.files {
            files.remove(name)?;
        }
        if let Some(keychain) = &self.keychain {
            keychain.remove(name)?;
        }
        Ok(())
    }
}

fn default_file_store_root() -> PathBuf {
    if let Some(path) = env::var_os("MODEL_GATEWAY_HOME") {
        return PathBuf::from(path).join("secrets");
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("model-gateway")
        .join("secrets")
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
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::{FileSecretStore, SecretError, SecretResolver, SecretStore, validate_secret_name};

    #[derive(Default)]
    struct FakeSecretStore {
        values: Mutex<BTreeMap<String, String>>,
    }

    impl SecretStore for FakeSecretStore {
        fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
            Ok(self.values.lock().expect("fake lock").get(name).cloned())
        }

        fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
            self.values
                .lock()
                .expect("fake lock")
                .insert(name.to_owned(), value.to_owned());
            Ok(())
        }

        fn remove(&self, name: &str) -> Result<(), SecretError> {
            self.values.lock().expect("fake lock").remove(name);
            Ok(())
        }

        fn source(&self) -> &'static str {
            "fake-keychain"
        }
    }

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

    #[test]
    fn resolver_uses_files_before_keychain() {
        let directory = tempfile::tempdir().expect("tempdir");
        let files = FileSecretStore::new(directory.path());
        files
            .set("RESOLVER_TEST_KEY", "file-value")
            .expect("file set");
        let keychain = FakeSecretStore::default();
        keychain
            .set("RESOLVER_TEST_KEY", "keychain-value")
            .expect("keychain set");
        let resolver = SecretResolver::with_stores(Some(Box::new(files)), Some(Box::new(keychain)));
        assert_eq!(
            resolver.get("RESOLVER_TEST_KEY").expect("resolve"),
            Some("file-value".to_owned())
        );
        assert_eq!(
            resolver.source("RESOLVER_TEST_KEY").expect("source"),
            Some("protected-file")
        );
    }

    #[test]
    fn fake_keychain_supports_set_get_and_remove() {
        let resolver =
            SecretResolver::with_stores(None, Some(Box::new(FakeSecretStore::default())));
        assert_eq!(
            resolver
                .set_preferred("FAKE_KEYCHAIN_TEST", "value")
                .expect("set"),
            "fake-keychain"
        );
        assert_eq!(
            resolver.get("FAKE_KEYCHAIN_TEST").expect("get"),
            Some("value".to_owned())
        );
        resolver.remove("FAKE_KEYCHAIN_TEST").expect("remove");
        assert_eq!(resolver.get("FAKE_KEYCHAIN_TEST").expect("get"), None);
    }

    #[cfg(unix)]
    #[test]
    fn file_store_enforces_directory_and_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("secrets");
        let store = FileSecretStore::new(&root);
        store.set("PERMISSIONS_TEST", "value").expect("set");
        assert_eq!(
            std::fs::metadata(&root)
                .expect("root metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(root.join("PERMISSIONS_TEST"))
                .expect("secret metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
