use std::process::Command;

/// Strip environment variables that would trigger automatic provider discovery
/// via `discover_environment_providers` in the gateway's config loader.
/// Without this, the user's shell environment can leak into CI tests and
/// produce non-deterministic provider counts and quota-reference output.
fn strip_provider_env_vars(mut cmd: Command) -> Command {
    for var in [
        "OPENROUTER_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "DEEPSEEK_API_KEY",
        "FIREWORKS_API_KEY",
        "ZAI_API_KEY",
        "GOOGLE_API_KEY",
        "KILOCODE_API_KEY",
        "OPENCODE_API_KEY",
        "MISTRAL_API_KEY",
        "NOUS_PORTAL_API_KEY",
        "NVIDIA_NIM_API_KEY",
        "GROQ_API_KEY",
        "ORCAROUTER_API_KEY",
        "OLLAMA_API_KEY",
        "SILICON_FLOW_API_KEY",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

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
fn config_check_discovers_environment_providers_without_setup() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = strip_provider_env_vars(Command::new(env!("CARGO_BIN_EXE_model-gateway")))
        .args(["config", "check"])
        .env(
            "MODEL_GATEWAY_CONFIG",
            directory.path().join("missing.toml"),
        )
        .env(
            "MODEL_GATEWAY_STATE_PATH",
            directory.path().join("routing.sqlite3"),
        )
        .env("MODEL_GATEWAY_SECRET_STORE", "environment")
        .env("OPENROUTER_API_KEY", "test-openrouter-key")
        .env("MODEL_GATEWAY_OPENROUTER_BILLING_MODE", "paid")
        .env(
            "MODEL_GATEWAY_OPENROUTER_MODEL_ALLOWLIST",
            "openai/gpt-4o-mini",
        )
        .output()
        .expect("run config check");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    assert!(stdout.contains("Providers: 1"));
    assert!(stdout.contains("Aliases: 0"));
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
    let output = strip_provider_env_vars(Command::new(env!("CARGO_BIN_EXE_model-gateway")))
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
     "intelligence": 75.0,
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

#[test]
fn pricing_import_and_explain_use_provider_scoped_overrides() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config_path = directory.path().join("config.toml");
    let state_path = directory.path().join("routing.sqlite3");
    let pricing_path = directory.path().join("pricing.jsonl");
    std::fs::write(
        &config_path,
        r#"
[providers.fixture]
adapter = "openai_chat"
base_url = "http://localhost:8000/v1"
billing_mode = "paid"
"#,
    )
    .expect("write config");
    std::fs::write(
        &pricing_path,
        r#"{"provider":"fixture","model":"mimo-v2-pro","input_price_per_million":1.2,"output_price_per_million":3.4}"#,
    )
    .expect("write pricing import");
    let environment = |command: &mut Command| {
        command
            .env("MODEL_GATEWAY_CONFIG", &config_path)
            .env("MODEL_GATEWAY_STATE_PATH", &state_path)
            .env("MODEL_GATEWAY_SECRET_STORE", "environment");
    };

    let mut import = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    import.args([
        "pricing",
        "import",
        "--file",
        pricing_path.to_str().expect("path"),
    ]);
    environment(&mut import);
    let output = import.output().expect("run pricing import");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut explain = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    explain.args(["pricing", "explain", "fixture", "mimo-v2-pro"]);
    environment(&mut explain);
    let output = explain.output().expect("run pricing explain");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    assert!(stdout.contains("1.2"));
    assert!(stdout.contains("3.4"));
    assert!(stdout.contains("manual-overrides"));
}
