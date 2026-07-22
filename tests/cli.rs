use std::process::Command;

#[test]
fn credential_list_succeeds_before_configuration_exists() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_model-gateway"))
        .args(["credentials", "list"])
        .env(
            "MODEL_GATEWAY_CONFIG",
            directory.path().join("missing.toml"),
        )
        .env("MODEL_GATEWAY_SECRET_STORE", "environment")
        .output()
        .expect("run credentials list");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout"),
        "No configured credentials\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn config_show_prints_canonical_non_secret_configuration() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config_path = directory.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
[providers.local]
adapter = "openai_chat"
base_url = "http://localhost:11434/v1"
api_key_secret = "LOCAL_API_KEY"

[models.local]
[[models.local.targets]]
provider = "local"
model = "llama3.2"
"#,
    )
    .expect("write config");
    let output = Command::new(env!("CARGO_BIN_EXE_model-gateway"))
        .args(["config", "show"])
        .env("MODEL_GATEWAY_CONFIG", &config_path)
        .env("MODEL_GATEWAY_SECRET_STORE", "environment")
        .output()
        .expect("run config show");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    assert!(stdout.contains("LOCAL_API_KEY"));
    assert!(stdout.contains("local"));
    assert!(!stdout.contains("Bearer"));
}
