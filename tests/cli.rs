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

#[test]
fn catalog_status_uses_an_isolated_local_database() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config_path = directory.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
[providers.local]
adapter = "openai_chat"
base_url = "http://localhost:8000/v1"

[models.fixture]
[[models.fixture.targets]]
provider = "local"
model = "fixture"
"#,
    )
    .expect("write config");
    let output = Command::new(env!("CARGO_BIN_EXE_model-gateway"))
        .args(["catalog", "status"])
        .env("MODEL_GATEWAY_CONFIG", &config_path)
        .env(
            "MODEL_GATEWAY_STATE_PATH",
            directory.path().join("routing.sqlite3"),
        )
        .env("MODEL_GATEWAY_SECRET_STORE", "environment")
        .output()
        .expect("run catalog status");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout"),
        "No cached provider catalogs\n"
    );
}

#[test]
fn benchmark_import_and_status_use_validated_local_snapshots() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config_path = directory.path().join("config.toml");
    let state_path = directory.path().join("routing.sqlite3");
    let import_path = directory.path().join("benchmarks.json");
    std::fs::write(
        &config_path,
        r#"
[providers.local]
adapter = "openai_chat"
base_url = "http://localhost:8000/v1"

[models.fixture]
[[models.fixture.targets]]
provider = "local"
model = "fixture"
"#,
    )
    .expect("write config");
    std::fs::write(
        &import_path,
        r#"{
  "source": "fixture",
  "attribution": "Fixture benchmark data",
  "models": [{
    "id": "fixture-model",
    "general_quality": 75.0,
    "input_price_per_million": 1.0,
    "output_price_per_million": 2.0
  }]
}"#,
    )
    .expect("write benchmark import");
    let environment = |command: &mut Command| {
        command
            .env("MODEL_GATEWAY_CONFIG", &config_path)
            .env("MODEL_GATEWAY_STATE_PATH", &state_path)
            .env("MODEL_GATEWAY_SECRET_STORE", "environment");
    };
    let mut import = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    import.args([
        "benchmarks",
        "import",
        "--file",
        import_path.to_str().expect("path"),
    ]);
    environment(&mut import);
    let output = import.output().expect("run benchmark import");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Imported fixture: 1 models"));

    let mut status = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    status.args(["benchmarks", "status"]);
    environment(&mut status);
    let output = status.output().expect("run benchmark status");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    assert!(stdout.contains("fixture: 1 models"));
    assert!(stdout.contains("attribution=Fixture benchmark data"));
}

#[test]
fn benchmark_import_rejects_empty_snapshots_without_replacing_state() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config_path = directory.path().join("config.toml");
    let import_path = directory.path().join("invalid.json");
    std::fs::write(
        &config_path,
        r#"
[providers.local]
adapter = "openai_chat"
base_url = "http://localhost:8000/v1"

[models.fixture]
[[models.fixture.targets]]
provider = "local"
model = "fixture"
"#,
    )
    .expect("write config");
    std::fs::write(
        &import_path,
        r#"{"source":"fixture","attribution":"Fixture","models":[]}"#,
    )
    .expect("write invalid import");
    let output = Command::new(env!("CARGO_BIN_EXE_model-gateway"))
        .args([
            "benchmarks",
            "import",
            "--file",
            import_path.to_str().expect("path"),
        ])
        .env("MODEL_GATEWAY_CONFIG", &config_path)
        .env(
            "MODEL_GATEWAY_STATE_PATH",
            directory.path().join("routing.sqlite3"),
        )
        .env("MODEL_GATEWAY_SECRET_STORE", "environment")
        .output()
        .expect("run invalid import");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("at least one model"));
}
