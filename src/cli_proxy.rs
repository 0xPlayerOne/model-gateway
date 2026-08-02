use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Duration;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::storage::write_atomic;

pub const VERSION: &str = "7.2.103";
pub const PROVIDER_KEY: &str = "cli-proxy";
pub const API_KEY_SECRET: &str = "CLI_PROXY_API_KEY";
pub const DEFAULT_PORT: u16 = 8317;

const RELEASE_BASE_URL: &str =
    "https://github.com/router-for-me/CLIProxyAPI/releases/download/v7.2.103";

#[derive(Debug, Error)]
pub enum CliProxyError {
    #[error(
        "CLIProxyAPI is unsupported on {0}; install v{VERSION} manually and set MODEL_GATEWAY_CLI_PROXY_BINARY"
    )]
    UnsupportedPlatform(String),
    #[error("CLIProxyAPI install directory already exists at {0}; remove it before reinstalling")]
    AlreadyInstalled(String),
    #[error("CLIProxyAPI config already exists at {0}; pass --force to replace it")]
    ConfigExists(String),
    #[error("CLIProxyAPI archive checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("CLIProxyAPI executable is unavailable at {0}; run `model-gateway cli-proxy setup`")]
    BinaryMissing(String),
    #[error("CLIProxyAPI config is unavailable at {0}; run `model-gateway cli-proxy setup`")]
    ConfigMissing(String),
    #[error("CLIProxyAPI command exited with status {0}")]
    CommandFailed(ExitStatus),
    #[error("invalid CLIProxyAPI option: {0}")]
    InvalidOption(String),
    #[error("archive extraction failed with status {0}")]
    ExtractionFailed(ExitStatus),
    #[error("download failed: {0}")]
    Download(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliProxyPaths {
    pub root: PathBuf,
    pub binary: PathBuf,
    pub config: PathBuf,
    pub auth_dir: PathBuf,
}

impl CliProxyPaths {
    pub fn discover(config_path: &Path) -> Self {
        let root = env::var_os("MODEL_GATEWAY_CLI_PROXY_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                config_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("cli-proxy")
            });
        let binary = env::var_os("MODEL_GATEWAY_CLI_PROXY_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("bin").join(VERSION).join("cli-proxy-api"));
        let config = env::var_os("MODEL_GATEWAY_CLI_PROXY_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("config.yaml"));
        let auth_dir = env::var_os("MODEL_GATEWAY_CLI_PROXY_AUTH_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("auth"));
        Self {
            root,
            binary,
            config,
            auth_dir,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthProvider {
    Claude,
    Codex,
}

impl OAuthProvider {
    fn login_flag(self, device: bool) -> Result<&'static str, CliProxyError> {
        match (self, device) {
            (Self::Claude, false) => Ok("-claude-login"),
            (Self::Claude, true) => Err(CliProxyError::InvalidOption(
                "Claude device login is not supported upstream".to_owned(),
            )),
            (Self::Codex, false) => Ok("-codex-login"),
            (Self::Codex, true) => Ok("-codex-device-login"),
        }
    }
}

struct ReleaseAsset {
    name: &'static str,
    sha256: &'static str,
}

pub fn install(paths: &CliProxyPaths) -> Result<(), CliProxyError> {
    let install_dir = paths.binary.parent().ok_or_else(|| {
        CliProxyError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "CLIProxyAPI binary path has no parent",
        ))
    })?;
    if paths.binary.exists() || install_dir.exists() {
        return Err(CliProxyError::AlreadyInstalled(
            install_dir.display().to_string(),
        ));
    }
    let asset = release_asset()?;
    let url = format!("{RELEASE_BASE_URL}/{}", asset.name);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(concat!("model-gateway/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| CliProxyError::Download(error.to_string()))?;
    let archive = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::bytes)
        .map_err(|error| CliProxyError::Download(error.to_string()))?;
    let digest = Sha256::digest(&archive);
    let actual = hex(digest.as_ref());
    if actual != asset.sha256 {
        return Err(CliProxyError::ChecksumMismatch {
            expected: asset.sha256.to_owned(),
            actual,
        });
    }

    let bin_root = install_dir.parent().ok_or_else(|| {
        CliProxyError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "CLIProxyAPI install directory has no parent",
        ))
    })?;
    ensure_private_dir(&paths.root)?;
    ensure_private_dir(bin_root)?;
    let staging = bin_root.join(format!(
        ".{VERSION}.{}.install",
        hex(&random_bytes::<12>()?)
    ));
    fs::create_dir(&staging)?;
    ensure_private_dir(&staging)?;
    let result = (|| {
        let archive_path = staging.join(asset.name);
        write_private_file(&archive_path, &archive)?;
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(&staging)
            .status()?;
        fs::remove_file(&archive_path)?;
        if !status.success() {
            return Err(CliProxyError::ExtractionFailed(status));
        }
        let extracted_binary = staging.join("cli-proxy-api");
        let metadata = fs::symlink_metadata(&extracted_binary)?;
        if !metadata.file_type().is_file() {
            return Err(CliProxyError::BinaryMissing(
                extracted_binary.display().to_string(),
            ));
        }
        let final_name = paths.binary.file_name().ok_or_else(|| {
            CliProxyError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "CLIProxyAPI binary path has no file name",
            ))
        })?;
        let staged_binary = staging.join(final_name);
        if staged_binary != extracted_binary {
            fs::rename(&extracted_binary, &staged_binary)?;
        }
        set_executable(&staged_binary)?;
        fs::rename(&staging, install_dir)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

pub fn initialize(paths: &CliProxyPaths, api_key: &str, force: bool) -> Result<(), CliProxyError> {
    if paths.config.exists() && !force {
        return Err(CliProxyError::ConfigExists(
            paths.config.display().to_string(),
        ));
    }
    ensure_private_dir(&paths.root)?;
    ensure_private_dir(&paths.auth_dir)?;
    let config = generated_config(paths, api_key)?;
    write_private_file(&paths.config, config.as_bytes())?;
    Ok(())
}

/// Resolve the sidecar listener port once, at setup time. The generated
/// upstream config is static, so a runtime-only environment override would
/// otherwise make the launcher probe a different port than CLIProxyAPI binds.
/// An operator can choose a non-default port by setting this before setup;
/// subsequent launchers validate that their environment agrees with the
/// generated config.
pub fn configured_port() -> Result<u16, CliProxyError> {
    match env::var("MODEL_GATEWAY_CLI_PROXY_PORT") {
        Ok(value) => value
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| {
                CliProxyError::InvalidOption(format!(
                    "MODEL_GATEWAY_CLI_PROXY_PORT must be an integer from 1 to 65535, got '{value}'"
                ))
            }),
        Err(env::VarError::NotPresent) => Ok(DEFAULT_PORT),
        Err(env::VarError::NotUnicode(_)) => Err(CliProxyError::InvalidOption(
            "MODEL_GATEWAY_CLI_PROXY_PORT contains non-Unicode data".to_owned(),
        )),
    }
}

pub fn base_url() -> Result<String, CliProxyError> {
    Ok(format!("http://127.0.0.1:{}/v1", configured_port()?))
}

pub fn generate_api_key() -> Result<String, CliProxyError> {
    let bytes = random_bytes::<32>()?;
    Ok(format!("mg-cpa-{}", hex(&bytes)))
}

pub fn login(
    paths: &CliProxyPaths,
    provider: OAuthProvider,
    device: bool,
    no_browser: bool,
) -> Result<(), CliProxyError> {
    validate_paths(paths)?;
    let status = login_command(paths, provider, device, no_browser)?.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(CliProxyError::CommandFailed(status))
    }
}

fn login_command(
    paths: &CliProxyPaths,
    provider: OAuthProvider,
    device: bool,
    no_browser: bool,
) -> Result<Command, CliProxyError> {
    let mut command = Command::new(&paths.binary);
    command.arg("-config").arg(&paths.config);
    if no_browser {
        command.arg("-no-browser");
    }
    command.arg(provider.login_flag(device)?);
    Ok(command)
}

pub fn serve(paths: &CliProxyPaths) -> Result<(), CliProxyError> {
    validate_paths(paths)?;
    let status = Command::new(&paths.binary)
        .arg("-config")
        .arg(&paths.config)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(CliProxyError::CommandFailed(status))
    }
}

pub fn generated_config(paths: &CliProxyPaths, api_key: &str) -> Result<String, CliProxyError> {
    let auth_dir = serde_json::to_string(&paths.auth_dir.to_string_lossy())?;
    let api_key = serde_json::to_string(api_key)?;
    let port = configured_port()?;
    Ok(format!(
        r#"# Generated by model-gateway for CLIProxyAPI v{VERSION}.
host: "127.0.0.1"
port: {port}
tls:
  enable: false
  cert: ""
  key: ""
remote-management:
  allow-remote: false
  secret-key: ""
  disable-control-panel: true
  disable-auto-update-panel: true
auth-dir: {auth_dir}
api-keys:
  - {api_key}
debug: false
pprof:
  enable: false
  addr: "127.0.0.1:8316"
plugins:
  enabled: false
  dir: "plugins"
logging-to-file: false
usage-statistics-enabled: false
passthrough-headers: true
request-retry: 1
max-retry-credentials: 0
max-retry-interval: 5
disable-cooling: false
save-cooldown-status: true
disable-claude-cloak-mode: true
routing:
  strategy: "round-robin"
  session-affinity: true
  session-affinity-ttl: "1h"
ws-auth: true
streaming:
  keepalive-seconds: 15
  bootstrap-retries: 0
"#
    ))
}

fn validate_paths(paths: &CliProxyPaths) -> Result<(), CliProxyError> {
    if !paths.binary.is_file() {
        return Err(CliProxyError::BinaryMissing(
            paths.binary.display().to_string(),
        ));
    }
    if !paths.config.is_file() {
        return Err(CliProxyError::ConfigMissing(
            paths.config.display().to_string(),
        ));
    }
    Ok(())
}

fn release_asset() -> Result<ReleaseAsset, CliProxyError> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok(ReleaseAsset {
            name: "CLIProxyAPI_7.2.103_darwin_aarch64.tar.gz",
            sha256: "2ca390dd6e4daf7b632bacf22a87000e52f3c626b180029827268c9dad240c1f",
        }),
        ("macos", "x86_64") => Ok(ReleaseAsset {
            name: "CLIProxyAPI_7.2.103_darwin_amd64.tar.gz",
            sha256: "5a5cbd7cb5642b863a553ca7ee17c34ca6524bcf1898e0c5f3b1ac12ed68cd1e",
        }),
        ("linux", "aarch64") => Ok(ReleaseAsset {
            name: "CLIProxyAPI_7.2.103_linux_aarch64_no-plugin.tar.gz",
            sha256: "235f37b7ccaecf10e0d31da353241993566b61dd0c2cd53fff32534ba045fdbe",
        }),
        ("linux", "x86_64") => Ok(ReleaseAsset {
            name: "CLIProxyAPI_7.2.103_linux_amd64_no-plugin.tar.gz",
            sha256: "29d078ee2b5d4189cde2113b73e99ccf15c411e8ed9c7ab28a53ec4a42a55293",
        }),
        (os, arch) => Err(CliProxyError::UnsupportedPlatform(format!("{os}/{arch}"))),
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), CliProxyError> {
    write_atomic(path, bytes)?;
    set_private_file(path)?;
    Ok(())
}

fn ensure_private_dir(path: &Path) -> Result<(), CliProxyError> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(CliProxyError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular directory", path.display()),
        )));
    }
    set_private_dir(path)
}

fn random_bytes<const N: usize>() -> Result<[u8; N], CliProxyError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|error| {
        CliProxyError::Io(std::io::Error::other(format!(
            "secure random generation failed: {error}"
        )))
    })?;
    Ok(bytes)
}

#[cfg(unix)]
fn set_private_dir(path: &Path) -> Result<(), CliProxyError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir(_path: &Path) -> Result<(), CliProxyError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), CliProxyError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<(), CliProxyError> {
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), CliProxyError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), CliProxyError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::{
        CliProxyError, CliProxyPaths, OAuthProvider, VERSION, ensure_private_dir, generate_api_key,
        generated_config, hex, initialize, install, login_command, random_bytes, release_asset,
        set_executable, set_private_dir, set_private_file, validate_paths, write_private_file,
    };
    use std::path::Path;

    #[test]
    fn generated_config_is_loopback_only_and_disables_management() {
        let paths = CliProxyPaths {
            root: "/tmp/cpa".into(),
            binary: "/tmp/cpa/bin/cli-proxy-api".into(),
            config: "/tmp/cpa/config.yaml".into(),
            auth_dir: "/tmp/cpa/auth".into(),
        };
        let config = generated_config(&paths, "secret").expect("config");
        assert!(config.contains("host: \"127.0.0.1\""));
        assert!(config.contains("allow-remote: false"));
        assert!(config.contains("disable-control-panel: true"));
        assert!(config.contains("plugins:\n  enabled: false"));
        assert!(config.contains("tls:\n  enable: false"));
        assert!(config.contains("remote-management:\n  allow-remote: false"));
        assert!(config.contains("save-cooldown-status: true"));
        assert!(config.contains("session-affinity: true"));
        assert!(!config.contains("latest"));
        assert!(config.contains(&format!("v{VERSION}")));
    }

    #[test]
    fn oauth_flags_reject_unsupported_claude_device_flow() {
        assert_eq!(
            OAuthProvider::Codex.login_flag(true).expect("codex flag"),
            "-codex-device-login"
        );
        assert!(OAuthProvider::Claude.login_flag(true).is_err());
    }

    #[test]
    fn login_command_keeps_config_and_oauth_flags_separate() {
        let paths = CliProxyPaths {
            root: "/private/root".into(),
            binary: "/private/root/cli proxy".into(),
            config: "/private/root/config file.yaml".into(),
            auth_dir: "/private/root/auth".into(),
        };
        let command = login_command(&paths, OAuthProvider::Codex, true, true).expect("command");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "-config",
                "/private/root/config file.yaml",
                "-no-browser",
                "-codex-device-login"
            ]
        );
    }

    #[test]
    fn hex_encoding_is_stable() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }

    #[test]
    fn release_asset_is_exactly_versioned_and_has_sha256() {
        let asset = release_asset().expect("supported CI platform");
        assert!(asset.name.contains(VERSION));
        assert!(!asset.name.contains("latest"));
        assert_eq!(asset.sha256.len(), 64);
        assert!(asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn installer_refuses_existing_version_directory_before_download() {
        let directory = tempfile::tempdir().expect("tempdir");
        let install_dir = directory.path().join("bin").join(VERSION);
        std::fs::create_dir_all(&install_dir).expect("install dir");
        let paths = CliProxyPaths {
            root: directory.path().to_path_buf(),
            binary: install_dir.join("cli-proxy-api"),
            config: directory.path().join("config.yaml"),
            auth_dir: directory.path().join("auth"),
        };
        assert!(matches!(
            install(&paths),
            Err(CliProxyError::AlreadyInstalled(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn initialize_rejects_symlink_config_destination() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("tempdir");
        let victim = directory.path().join("victim");
        std::fs::write(&victim, "unchanged").expect("victim");
        let config = directory.path().join("config.yaml");
        symlink(&victim, &config).expect("symlink");
        let paths = CliProxyPaths {
            root: directory.path().to_path_buf(),
            binary: directory.path().join("binary"),
            config,
            auth_dir: directory.path().join("auth"),
        };
        assert!(initialize(&paths, "secret", true).is_err());
        assert_eq!(
            std::fs::read_to_string(victim).expect("victim contents"),
            "unchanged"
        );
    }

    #[cfg(unix)]
    #[test]
    fn initialize_rejects_symlink_root_and_auth_directories() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join("target");
        std::fs::create_dir(&target).expect("target");
        let root_link = directory.path().join("root-link");
        symlink(&target, &root_link).expect("root symlink");
        let root_paths = CliProxyPaths {
            root: root_link.clone(),
            binary: root_link.join("binary"),
            config: root_link.join("config.yaml"),
            auth_dir: root_link.join("auth"),
        };
        assert!(initialize(&root_paths, "secret", false).is_err());

        let root = directory.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let auth_link = root.join("auth");
        symlink(&target, &auth_link).expect("auth symlink");
        let auth_paths = CliProxyPaths {
            root: root.clone(),
            binary: root.join("binary"),
            config: root.join("config.yaml"),
            auth_dir: auth_link,
        };
        assert!(initialize(&auth_paths, "secret", false).is_err());
    }

    #[test]
    fn random_bytes_returns_correct_length() {
        let bytes = random_bytes::<16>().expect("16 bytes");
        assert_eq!(bytes.len(), 16);

        let bytes = random_bytes::<32>().expect("32 bytes");
        assert_eq!(bytes.len(), 32);

        // Verify it's not all zeros (astronomically unlikely to fail)
        let bytes = random_bytes::<32>().expect("32 bytes");
        assert!(bytes.iter().any(|&b| b != 0));
    }

    #[test]
    fn generate_api_key_uses_correct_format() {
        let key = generate_api_key().expect("api key");
        // "mg-cpa-" prefix + 64 hex chars (32 bytes)
        assert!(key.starts_with("mg-cpa-"));
        assert_eq!(key.len(), 71);
        // Every character after the prefix must be a hex digit
        let hex_part = &key["mg-cpa-".len()..];
        assert!(hex_part.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn validate_paths_rejects_missing_binary() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = CliProxyPaths {
            root: directory.path().to_path_buf(),
            binary: directory.path().join("missing-binary"),
            config: directory.path().join("config.yaml"),
            auth_dir: directory.path().join("auth"),
        };
        std::fs::write(&paths.config, b"config").expect("write config");
        assert!(matches!(
            validate_paths(&paths),
            Err(CliProxyError::BinaryMissing(_))
        ));
    }

    #[test]
    fn validate_paths_rejects_missing_config() {
        let directory = tempfile::tempdir().expect("tempdir");
        let binary_path = directory.path().join("cli-proxy-api");
        std::fs::write(&binary_path, b"binary").expect("write binary");
        std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o700))
            .expect("set executable");
        let paths = CliProxyPaths {
            root: directory.path().to_path_buf(),
            binary: binary_path,
            config: directory.path().join("missing-config.yaml"),
            auth_dir: directory.path().join("auth"),
        };
        assert!(matches!(
            validate_paths(&paths),
            Err(CliProxyError::ConfigMissing(_))
        ));
    }

    #[test]
    fn validate_paths_accepts_existing_binary_and_config() {
        let directory = tempfile::tempdir().expect("tempdir");
        let binary_path = directory.path().join("cli-proxy-api");
        let config_path = directory.path().join("config.yaml");
        std::fs::write(&binary_path, b"binary").expect("write binary");
        std::fs::write(&config_path, b"config").expect("write config");
        std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o700))
            .expect("set executable");
        let paths = CliProxyPaths {
            root: directory.path().to_path_buf(),
            binary: binary_path,
            config: config_path,
            auth_dir: directory.path().join("auth"),
        };
        assert!(validate_paths(&paths).is_ok());
    }

    #[test]
    fn write_private_file_creates_file_with_content() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("secret.txt");
        write_private_file(&path, b"secret content").expect("write");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "secret content"
        );
    }

    #[test]
    fn ensure_private_dir_creates_missing_directory() {
        let directory = tempfile::tempdir().expect("tempdir");
        let new_dir = directory.path().join("new-dir");
        assert!(!new_dir.exists());
        ensure_private_dir(&new_dir).expect("create dir");
        assert!(new_dir.is_dir());
    }

    #[test]
    fn ensure_private_dir_accepts_existing_directory() {
        let directory = tempfile::tempdir().expect("tempdir");
        ensure_private_dir(directory.path()).expect("existing dir");
        assert!(directory.path().is_dir());
    }

    #[test]
    fn ensure_private_dir_rejects_file_path() {
        let directory = tempfile::tempdir().expect("tempdir");
        let file_path = directory.path().join("not-a-dir");
        std::fs::write(&file_path, b"content").expect("write file");
        assert!(ensure_private_dir(&file_path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn set_private_dir_sets_correct_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join("private-dir");
        std::fs::create_dir(&target).expect("create dir");
        // Set a permissive mode first
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .expect("set permissive");
        set_private_dir(&target).expect("set private");
        let meta = std::fs::symlink_metadata(&target).expect("metadata");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o700,
            "expected 0700 permissions"
        );
    }

    #[cfg(unix)]
    #[test]
    fn set_private_file_and_set_executable_set_correct_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let file_path = directory.path().join("private-file");
        std::fs::write(&file_path, b"data").expect("write file");
        // Set a permissive mode first
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o644))
            .expect("set permissive");
        set_private_file(&file_path).expect("set private");
        let meta = std::fs::symlink_metadata(&file_path).expect("metadata");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "expected 0600 permissions"
        );

        // Now set executable
        set_executable(&file_path).expect("set executable");
        let meta = std::fs::symlink_metadata(&file_path).expect("metadata");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o700,
            "expected 0700 permissions after executable"
        );
    }

    #[test]
    fn cli_proxy_paths_discover_uses_defaults_when_no_env_vars() {
        let paths = CliProxyPaths::discover(Path::new("/tmp/model-gateway/config.toml"));
        assert_eq!(paths.root, Path::new("/tmp/model-gateway/cli-proxy"));
        assert_eq!(
            paths.binary,
            Path::new("/tmp/model-gateway/cli-proxy/bin")
                .join(VERSION)
                .join("cli-proxy-api")
        );
        assert_eq!(
            paths.config,
            Path::new("/tmp/model-gateway/cli-proxy/config.yaml")
        );
        assert_eq!(
            paths.auth_dir,
            Path::new("/tmp/model-gateway/cli-proxy/auth")
        );
    }

    #[test]
    fn hex_handles_empty_and_single_bytes() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00]), "00");
        assert_eq!(hex(&[0x01]), "01");
        assert_eq!(hex(&[0xff]), "ff");
        assert_eq!(hex(&[0xab, 0xcd, 0xef]), "abcdef");
    }
}
