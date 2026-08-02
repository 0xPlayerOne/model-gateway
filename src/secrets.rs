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
        || message.contains("no matching credential")
        || message.contains("not found")
        || message.contains("could not be found")
        || message.contains("no such")
}

/// Returns true when the error means the platform keychain store itself is
/// unavailable (for example, no Secret Service daemon on headless Linux), as
/// opposed to a missing credential or a real keychain I/O failure. Reads treat
/// an unavailable store as "no credential" so the gateway can start without a
/// desktop keychain; writes must still surface the error instead of silently
/// falling back to an insecure store.
fn is_unavailable_keychain_error(error: &SecretError) -> bool {
    match error {
        SecretError::Keychain(message) => {
            let message = message.to_lowercase();
            message.contains("no default store")
                || message.contains("cannot search or create entries")
        }
        _ => false,
    }
}

impl SecretStore for KeychainSecretStore {
    fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        let entry = match Self::entry(name) {
            Ok(entry) => entry,
            Err(error) if is_unavailable_keychain_error(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        match entry.get_password() {
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
    mode: SecretStoreMode,
}

/// Human-readable description of the effective secret store, used for
/// startup diagnostics. Never contains secret values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretStoreMode {
    /// Explicit OS keychain mode. The keychain is never silently combined
    /// with another store.
    Keychain,
    /// Deterministic non-interactive store: protected files under
    /// `MODEL_GATEWAY_SECRET_DIR` or the default secret root.
    File(PathBuf),
    /// Environment variables only; nothing is persisted.
    Environment,
    /// An invalid explicit mode. Operations fail with the original value.
    Invalid,
}

impl std::fmt::Display for SecretStoreMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keychain => write!(formatter, "os-keychain"),
            Self::File(root) => write!(formatter, "protected-file({})", root.display()),
            Self::Environment => write!(formatter, "environment"),
            Self::Invalid => write!(formatter, "invalid"),
        }
    }
}

impl Default for SecretResolver {
    fn default() -> Self {
        let mode = env::var("MODEL_GATEWAY_SECRET_STORE").ok();
        let secret_dir = env::var_os("MODEL_GATEWAY_SECRET_DIR").map(PathBuf::from);
        Self::from_mode(mode.as_deref(), secret_dir)
    }
}

impl SecretResolver {
    /// Resolves the effective stores from an explicit mode and optional file
    /// root. `mode = None` (unset) selects the deterministic non-interactive
    /// protected-file store so unattended startup (`serve`, launcher scripts,
    /// launchd, cron, containers) never prompts or blocks on the OS keychain;
    /// `keychain` remains an explicit opt-in for intentional interactive use.
    /// Modes are exclusive: `environment` never mounts `MODEL_GATEWAY_SECRET_DIR`.
    fn from_mode(mode: Option<&str>, secret_dir: Option<PathBuf>) -> Self {
        let configured_file_root = secret_dir.unwrap_or_else(default_file_store_root);
        let (files, keychain, initialization_error, mode) = match mode {
            None | Some("file") => (
                Some(Box::new(FileSecretStore::new(&configured_file_root)) as Box<dyn SecretStore>),
                None,
                None,
                SecretStoreMode::File(configured_file_root),
            ),
            Some("keychain") => (
                None,
                Some(Box::new(KeychainSecretStore) as Box<dyn SecretStore>),
                None,
                SecretStoreMode::Keychain,
            ),
            Some("environment") => (None, None, None, SecretStoreMode::Environment),
            Some(value) => (None, None, Some(value.to_owned()), SecretStoreMode::Invalid),
        };
        Self {
            environment: EnvironmentSecretStore,
            files,
            keychain,
            initialization_error,
            mode,
        }
    }

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
            mode: SecretStoreMode::Keychain,
        }
    }

    /// Describes the effective secret store for diagnostics. The description
    /// names the store and its location but never contains secret values.
    pub fn mode(&self) -> &SecretStoreMode {
        &self.mode
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

    use super::{
        FileSecretStore, KeychainSecretStore, SecretError, SecretResolver, SecretStore,
        SecretStoreMode, default_file_store_root, is_unavailable_keychain_error,
        validate_secret_name,
    };

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

    #[test]
    fn resolver_get_returns_none_for_missing_secret() {
        let resolver = SecretResolver::with_stores(None, None);
        assert_eq!(resolver.get("MG_TEST_NONEXISTENT_KEY").expect("get"), None);
    }

    #[test]
    fn resolver_source_returns_none_for_missing_value() {
        let resolver = SecretResolver::with_stores(None, None);
        assert_eq!(
            resolver.source("MG_TEST_NONEXISTENT_KEY").expect("source"),
            None
        );
    }

    #[test]
    fn resolver_set_preferred_fails_in_environment_only_mode() {
        let resolver = SecretResolver::with_stores(None, None);
        let err = resolver
            .set_preferred("MG_TEST_SET_FAIL", "value")
            .expect_err("should fail");
        assert!(matches!(err, SecretError::Keychain(_)));
    }

    #[test]
    fn resolver_remove_clears_both_stores() {
        let directory = tempfile::tempdir().expect("tempdir");
        let files = FileSecretStore::new(directory.path());
        files
            .set("MG_TEST_REMOVE_BOTH", "file-value")
            .expect("file set");
        let keychain = FakeSecretStore::default();
        keychain
            .set("MG_TEST_REMOVE_BOTH", "keychain-value")
            .expect("keychain set");
        let resolver = SecretResolver::with_stores(Some(Box::new(files)), Some(Box::new(keychain)));
        resolver.remove("MG_TEST_REMOVE_BOTH").expect("remove");
        assert_eq!(resolver.get("MG_TEST_REMOVE_BOTH").expect("get"), None);
    }

    #[test]
    fn resolver_operations_fail_with_invalid_store_error() {
        let resolver = SecretResolver {
            environment: super::EnvironmentSecretStore,
            files: None,
            keychain: None,
            initialization_error: Some("unknown-mode".to_owned()),
            mode: super::SecretStoreMode::Invalid,
        };
        let err = resolver.get("MG_TEST_FAIL").expect_err("should fail");
        assert!(matches!(err, SecretError::InvalidStore(_)));
        let err = resolver.source("MG_TEST_FAIL").expect_err("should fail");
        assert!(matches!(err, SecretError::InvalidStore(_)));
        let err = resolver
            .set_preferred("MG_TEST_FAIL", "value")
            .expect_err("should fail");
        assert!(matches!(err, SecretError::InvalidStore(_)));
        let err = resolver.remove("MG_TEST_FAIL").expect_err("should fail");
        assert!(matches!(err, SecretError::InvalidStore(_)));
    }

    #[test]
    fn validate_secret_name_rejects_empty_and_special_chars() {
        assert!(validate_secret_name("").is_err());
        assert!(validate_secret_name("foo.bar").is_err());
        assert!(validate_secret_name("foo/bar").is_err());
        assert!(validate_secret_name("foo bar").is_err());
        assert!(validate_secret_name("foo\nbar").is_err());
    }

    #[test]
    fn validate_secret_name_accepts_valid_identifiers() {
        assert!(validate_secret_name("OPENROUTER_API_KEY").is_ok());
        assert!(validate_secret_name("my-secret-1").is_ok());
        assert!(validate_secret_name("a").is_ok());
        assert!(validate_secret_name("A_Z_99").is_ok());
    }

    #[test]
    fn file_store_get_returns_none_for_absent_key() {
        let store = FileSecretStore::new(tempfile::tempdir().expect("tempdir").path());
        assert_eq!(store.get("MG_TEST_ABSENT").expect("get"), None);
    }

    #[test]
    fn file_store_remove_is_idempotent() {
        let store = FileSecretStore::new(tempfile::tempdir().expect("tempdir").path());
        store.remove("MG_TEST_IDEMPOTENT").expect("first remove");
        store.remove("MG_TEST_IDEMPOTENT").expect("second remove");
    }

    #[test]
    fn resolver_mode_names_the_store_without_values() {
        use super::SecretStoreMode;
        let directory = tempfile::tempdir().expect("tempdir");
        let resolver = SecretResolver::with_stores(
            Some(Box::new(FileSecretStore::new(directory.path()))),
            Some(Box::new(FakeSecretStore::default())),
        );
        let description = resolver.mode().to_string();
        assert!(
            description.starts_with("os-keychain"),
            "mode must describe the effective store, got {description}"
        );
        assert_eq!(SecretStoreMode::Environment.to_string(), "environment");
        assert_eq!(
            SecretStoreMode::File(directory.path().to_path_buf()).to_string(),
            format!("protected-file({})", directory.path().display())
        );
    }

    #[test]
    fn unset_mode_resolves_to_file_store_with_reported_default_root() {
        // Unattended startup (unset MODEL_GATEWAY_SECRET_STORE) must be
        // deterministic and non-interactive: the protected-file store, with
        // the mode diagnostics reporting the actual default root.
        let resolver = SecretResolver::from_mode(None, None);
        assert!(
            resolver.files.is_some(),
            "unset mode must mount the file store"
        );
        assert!(resolver.keychain.is_none());
        assert_eq!(resolver.initialization_error, None);
        assert_eq!(
            resolver.mode(),
            &SecretStoreMode::File(default_file_store_root()),
            "mode must report the actual default root"
        );
    }

    #[test]
    fn file_mode_reports_configured_root_and_mounts_it() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("configured-secrets");
        let resolver = SecretResolver::from_mode(Some("file"), Some(root.clone()));
        assert!(resolver.files.is_some());
        assert_eq!(resolver.mode(), &SecretStoreMode::File(root));
    }

    #[test]
    fn environment_mode_is_exclusively_environment() {
        // environment must never mount MODEL_GATEWAY_SECRET_DIR: a file
        // present in the configured directory must stay invisible and
        // nothing may be persisted.
        let directory = tempfile::tempdir().expect("tempdir");
        let resolver =
            SecretResolver::from_mode(Some("environment"), Some(directory.path().to_path_buf()));
        assert!(
            resolver.files.is_none(),
            "environment mode must not mount the file store"
        );
        assert!(resolver.keychain.is_none());
        assert_eq!(resolver.mode(), &SecretStoreMode::Environment);
        let err = resolver
            .set_preferred("MG_ENV_ONLY_TEST", "value")
            .expect_err("environment mode cannot persist");
        assert!(matches!(err, SecretError::Keychain(_)));
    }

    #[test]
    fn keychain_mode_is_explicit_and_exclusive() {
        let directory = tempfile::tempdir().expect("tempdir");
        let resolver =
            SecretResolver::from_mode(Some("keychain"), Some(directory.path().to_path_buf()));
        assert!(
            resolver.files.is_none(),
            "keychain mode must not mount the file store"
        );
        assert!(resolver.keychain.is_some());
        assert_eq!(resolver.mode(), &SecretStoreMode::Keychain);
    }

    #[test]
    fn invalid_mode_fails_closed_without_touching_a_store() {
        let resolver = SecretResolver::from_mode(Some("bogus"), None);
        assert!(resolver.files.is_none());
        assert!(resolver.keychain.is_none());
        assert_eq!(resolver.initialization_error.as_deref(), Some("bogus"));
        let err = resolver
            .get("MG_INVALID_MODE")
            .expect_err("must fail closed");
        assert!(matches!(err, SecretError::InvalidStore(_)));
    }

    #[test]
    fn environment_secret_store_set_and_remove_return_errors() {
        let store = super::EnvironmentSecretStore;
        let err = store
            .set("MG_TEST_ENV_SET", "value")
            .expect_err("should fail");
        assert!(matches!(err, SecretError::Keychain(_)));
        let err = store.remove("MG_TEST_ENV_REMOVE").expect_err("should fail");
        assert!(matches!(err, SecretError::Keychain(_)));
    }

    #[test]
    fn unavailable_keychain_error_is_classified_for_reads() {
        // Headless Linux: keyring Entry::new fails with this exact message when
        // no Secret Service daemon is available.
        let error = SecretError::Keychain(
            "No default store has been set, so cannot search or create entries".to_owned(),
        );
        assert!(is_unavailable_keychain_error(&error));
        let error = SecretError::Keychain(
            "Cannot search or create entries: platform store unavailable".to_owned(),
        );
        assert!(is_unavailable_keychain_error(&error));
    }

    #[test]
    fn missing_entry_and_unrelated_errors_are_not_unavailable_store_errors() {
        for message in [
            "no entry found",
            "no matching entry",
            "No matching credential found",
            "not found",
            "could not be found",
            "no such file or directory",
            "Couldn't access platform storage: keychain locked",
        ] {
            let error = SecretError::Keychain(message.to_owned());
            assert!(
                !is_unavailable_keychain_error(&error),
                "{message:?} must not be classified as an unavailable store"
            );
        }
        assert!(!is_unavailable_keychain_error(&SecretError::InvalidName(
            "x".to_owned()
        )));
        assert!(!is_unavailable_keychain_error(&SecretError::InvalidStore(
            "x".to_owned()
        )));
    }

    #[test]
    fn keychain_get_returns_none_when_store_unavailable_or_entry_absent() {
        // Passes on headless Linux (Entry::new fails with NoDefaultStore, treated
        // as no credential) and on keychain-backed platforms (missing entry).
        let store = KeychainSecretStore;
        assert_eq!(
            store
                .get("MG_TEST_ABSENT_KEYCHAIN_KEY")
                .expect("read must not fail"),
            None
        );
    }
}
