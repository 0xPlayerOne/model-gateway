use std::process::Command;

use model_gateway::benchmarks::BenchmarkModel;
use model_gateway::identity::{
    IdentityAliasRecord, IdentityConfidence, IdentityEntityRecord, IdentityImport,
};
use model_gateway::pricing::{PriceObservation, PriceRates, PriceScope, PriceSourceKind};
use model_gateway::routing::{AccessKind, CatalogRecord, RoutingStore};

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
        "CLI_PROXY_API_KEY",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

#[test]
fn cli_proxy_environment_discovery_uses_subscription_billing() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = strip_provider_env_vars(Command::new(env!("CARGO_BIN_EXE_model-gateway")))
        .args(["config", "check"])
        .env(
            "MODEL_GATEWAY_CONFIG",
            directory.path().join("missing.toml"),
        )
        .env("MODEL_GATEWAY_SECRET_STORE", "environment")
        .env(
            "MODEL_GATEWAY_STATE_PATH",
            directory.path().join("routing.sqlite3"),
        )
        .env("CLI_PROXY_API_KEY", "test-sidecar-key")
        .output()
        .expect("run config show");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    assert!(stdout.contains("Providers: 1"));
    assert!(!stdout.contains("test-sidecar-key"));
}

#[test]
fn cli_proxy_setup_uses_environment_frontend_key_consistently() {
    let directory = tempfile::tempdir().expect("tempdir");
    let home = directory.path().join("cli-proxy");
    let binary = home.join("bin").join("7.2.103").join("cli-proxy-api");
    std::fs::create_dir_all(binary.parent().expect("binary parent")).expect("binary dir");
    std::fs::write(&binary, "fixture").expect("binary fixture");
    let config_path = directory.path().join("config.toml");
    let output = strip_provider_env_vars(Command::new(env!("CARGO_BIN_EXE_model-gateway")))
        .args(["cli-proxy", "setup"])
        .env("MODEL_GATEWAY_CONFIG", &config_path)
        .env("MODEL_GATEWAY_SECRET_STORE", "environment")
        .env("MODEL_GATEWAY_CLI_PROXY_HOME", &home)
        .env("CLI_PROXY_API_KEY", "environment-sidecar-key")
        .output()
        .expect("run cli-proxy setup");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    assert!(stdout.contains("Stored the CLIProxyAPI frontend key"));
    assert!(!stdout.contains("environment-sidecar-key"));
    let sidecar_config = std::fs::read_to_string(home.join("config.yaml")).expect("sidecar config");
    assert!(sidecar_config.contains("environment-sidecar-key"));
    let gateway_config = std::fs::read_to_string(config_path).expect("gateway config");
    assert!(gateway_config.contains("billing_mode = \"subscription\""));
    assert!(!gateway_config.contains("environment-sidecar-key"));
}

#[test]
fn cli_proxy_setup_fails_before_config_write_when_secret_cannot_persist() {
    let directory = tempfile::tempdir().expect("tempdir");
    let home = directory.path().join("cli-proxy");
    let binary = home.join("bin").join("7.2.103").join("cli-proxy-api");
    std::fs::create_dir_all(binary.parent().expect("binary parent")).expect("binary dir");
    std::fs::write(binary, "fixture").expect("binary fixture");
    let output = strip_provider_env_vars(Command::new(env!("CARGO_BIN_EXE_model-gateway")))
        .args(["cli-proxy", "setup"])
        .env("MODEL_GATEWAY_CONFIG", directory.path().join("config.toml"))
        .env("MODEL_GATEWAY_SECRET_STORE", "environment")
        .env("MODEL_GATEWAY_CLI_PROXY_HOME", &home)
        .output()
        .expect("run cli-proxy setup");
    assert!(!output.status.success());
    assert!(!home.join("config.yaml").exists());
}

#[test]
fn cli_proxy_force_setup_preserves_existing_frontend_key() {
    let directory = tempfile::tempdir().expect("tempdir");
    let home = directory.path().join("cli-proxy");
    let binary = home.join("bin").join("7.2.103").join("cli-proxy-api");
    std::fs::create_dir_all(binary.parent().expect("binary parent")).expect("binary dir");
    std::fs::write(binary, "fixture").expect("binary fixture");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join("config.yaml"), "old-config").expect("old config");
    let secret_dir = directory.path().join("secrets");
    std::fs::create_dir(&secret_dir).expect("secret dir");
    std::fs::write(secret_dir.join("CLI_PROXY_API_KEY"), "stable-sidecar-key").expect("secret");
    let output = strip_provider_env_vars(Command::new(env!("CARGO_BIN_EXE_model-gateway")))
        .args(["cli-proxy", "setup", "--force"])
        .env("MODEL_GATEWAY_CONFIG", directory.path().join("config.toml"))
        .env("MODEL_GATEWAY_SECRET_STORE", "file")
        .env("MODEL_GATEWAY_SECRET_DIR", &secret_dir)
        .env("MODEL_GATEWAY_CLI_PROXY_HOME", &home)
        .output()
        .expect("run forced setup");
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(secret_dir.join("CLI_PROXY_API_KEY")).expect("secret"),
        "stable-sidecar-key"
    );
    assert!(
        std::fs::read_to_string(home.join("config.yaml"))
            .expect("config")
            .contains("stable-sidecar-key")
    );
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
fn healthcheck_fails_closed_when_the_gateway_is_unreachable() {
    let output = strip_provider_env_vars(Command::new(env!("CARGO_BIN_EXE_model-gateway")))
        .args(["healthcheck", "http://127.0.0.1:1"])
        .output()
        .expect("run healthcheck");

    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
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
        r#"{"provider":"fixture","model":"mimo-v2-pro","input_price_per_million":1.2,"output_price_per_million":3.4,"cache_read_price_per_million":0.3,"cache_write_price_per_million":4.5}"#,
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
    assert!(stdout.contains("0.3"));
    assert!(stdout.contains("4.5"));
    assert!(stdout.contains("manual-overrides"));
}

#[test]
fn pricing_coverage_reports_direct_profile_and_missing_prices() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config_path = directory.path().join("config.toml");
    let state_path = directory.path().join("routing.sqlite3");
    std::fs::write(
        &config_path,
        r#"
[providers.fixture]
adapter = "openai_chat"
base_url = "http://localhost:8000/v1"
pricing_profile = "fixture"
"#,
    )
    .expect("write config");
    let store = RoutingStore::open(Some(&state_path)).expect("store");
    store
        .replace_catalog(
            "fixture",
            &[
                CatalogRecord {
                    model: "direct-complete".to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: None,
                    supports_tools: None,
                    supports_vision: None,
                    supports_structured_output: None,
                    input_price_per_million: Some(1.0),
                    output_price_per_million: Some(2.0),
                },
                CatalogRecord {
                    model: "direct-incomplete".to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: None,
                    supports_tools: None,
                    supports_vision: None,
                    supports_structured_output: None,
                    input_price_per_million: Some(1.0),
                    output_price_per_million: None,
                },
                CatalogRecord {
                    model: "profile-covered".to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: None,
                    supports_tools: None,
                    supports_vision: None,
                    supports_structured_output: None,
                    input_price_per_million: None,
                    output_price_per_million: None,
                },
                CatalogRecord {
                    model: "missing".to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: None,
                    supports_tools: None,
                    supports_vision: None,
                    supports_structured_output: None,
                    input_price_per_million: None,
                    output_price_per_million: None,
                },
                CatalogRecord {
                    model: "profile-incomplete".to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: None,
                    supports_tools: None,
                    supports_vision: None,
                    supports_structured_output: None,
                    input_price_per_million: None,
                    output_price_per_million: None,
                },
            ],
        )
        .expect("catalog");
    store
        .replace_pricing(
            "models.dev",
            PriceSourceKind::ModelsDev,
            "Fixture",
            &[
                PriceObservation {
                    source: "models.dev".to_owned(),
                    source_kind: PriceSourceKind::ModelsDev,
                    scope: PriceScope::ProviderProfile,
                    provider_key: Some("fixture".to_owned()),
                    model_id: "profile-covered".to_owned(),
                    rates: PriceRates {
                        input_price_per_million: Some(3.0),
                        output_price_per_million: Some(4.0),
                        ..PriceRates::default()
                    },
                    fetched_at: Some(1),
                    as_of: None,
                    valid_from: None,
                    valid_until: None,
                    attribution: None,
                },
                PriceObservation {
                    source: "models.dev".to_owned(),
                    source_kind: PriceSourceKind::ModelsDev,
                    scope: PriceScope::ProviderProfile,
                    provider_key: Some("fixture".to_owned()),
                    model_id: "profile-incomplete".to_owned(),
                    rates: PriceRates {
                        input_price_per_million: Some(3.0),
                        ..PriceRates::default()
                    },
                    fetched_at: Some(1),
                    as_of: None,
                    valid_from: None,
                    valid_until: None,
                    attribution: None,
                },
            ],
        )
        .expect("profile pricing");
    drop(store);

    let output = Command::new(env!("CARGO_BIN_EXE_model-gateway"))
        .args(["pricing", "coverage", "--provider", "fixture", "--json"])
        .env("MODEL_GATEWAY_CONFIG", &config_path)
        .env("MODEL_GATEWAY_STATE_PATH", &state_path)
        .env("MODEL_GATEWAY_SECRET_STORE", "environment")
        .output()
        .expect("pricing coverage");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report JSON");
    assert_eq!(report["summary"]["complete"], 2);
    assert_eq!(report["summary"]["incomplete"], 2);
    assert_eq!(report["summary"]["missing"], 1);
    let models = report["models"].as_array().expect("models");
    let profile = models
        .iter()
        .find(|model| model["catalog_model"] == "profile-covered")
        .expect("profile-covered model");
    assert_eq!(profile["status"], "complete");
    assert_eq!(profile["effective_source"], "models.dev");
    assert_eq!(profile["effective_scope"], "provider_profile");
    assert_eq!(profile["estimated"], false);
}

#[test]
fn matching_reconcile_approve_explain_and_remove_are_provider_scoped() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config_path = directory.path().join("config.toml");
    let state_path = directory.path().join("routing.sqlite3");
    std::fs::write(
        &config_path,
        r#"
[providers.fixture]
adapter = "openai_chat"
base_url = "http://localhost:8000/v1"
billing_mode = "paid"
pricing_profile = "fixture"
"#,
    )
    .expect("write config");
    let store = RoutingStore::open(Some(&state_path)).expect("store");
    store
        .replace_catalog(
            "fixture",
            &[CatalogRecord {
                model: "model-family".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(1.0),
                output_price_per_million: Some(2.0),
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "model-family-2025",
                50.0,
                50.0,
                50.0,
                1.0,
                2.0,
            )],
        )
        .expect("benchmarks");
    store
        .replace_identity_source(&IdentityImport {
            source: "models.dev".to_owned(),
            attribution: "Fixture".to_owned(),
            entities: vec![IdentityEntityRecord {
                id: "hf:fixture/model-family".to_owned(),
                creator: Some("fixture".to_owned()),
                family: Some("model-family".to_owned()),
                version: None,
                variant: None,
                release_date: None,
                hugging_face_id: Some("Fixture/Model-Family".to_owned()),
            }],
            aliases: vec![IdentityAliasRecord {
                source: "models.dev".to_owned(),
                provider_key: "fixture".to_owned(),
                provider_model_id: "model-family".to_owned(),
                entity_id: "hf:fixture/model-family".to_owned(),
                confidence: IdentityConfidence::CanonicalReference,
                provenance_url: "fixture".to_owned(),
                observed_at: 100,
            }],
        })
        .expect("identities");
    drop(store);

    let environment = |command: &mut Command| {
        command
            .env("MODEL_GATEWAY_CONFIG", &config_path)
            .env("MODEL_GATEWAY_STATE_PATH", &state_path)
            .env("MODEL_GATEWAY_SECRET_STORE", "environment");
    };

    let mut reconcile = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    reconcile.args(["matching", "reconcile", "--provider", "fixture", "--json"]);
    environment(&mut reconcile);
    let output = reconcile.output().expect("reconcile");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report JSON");
    assert_eq!(report["summary"]["suggested"], 1);
    assert_eq!(report["models"][0]["benchmark_model"], "model-family-2025");

    let mut approve = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    approve.args([
        "matching",
        "approve",
        "fixture",
        "model-family",
        "MODEL.FAMILY.2025",
    ]);
    environment(&mut approve);
    assert!(approve.status().expect("approve").success());

    let mut explain = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    explain.args(["matching", "explain", "fixture", "model-family"]);
    environment(&mut explain);
    let output = explain.output().expect("explain");
    assert!(output.status.success());
    let explained: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("explain JSON");
    assert_eq!(explained["status"], "approved");
    assert_eq!(explained["source"], "registry");

    let mut status = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    status.args(["matching", "status"]);
    environment(&mut status);
    let output = status.output().expect("identity status");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("models.dev: 1 aliases"));

    let mut remove = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    remove.args(["matching", "remove", "fixture", "model-family"]);
    environment(&mut remove);
    assert!(remove.status().expect("remove direct mapping").success());

    let mut approve_entity = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    approve_entity.args([
        "matching",
        "approve-entity",
        "hf:fixture/model-family",
        "model-family-2025",
    ]);
    environment(&mut approve_entity);
    assert!(approve_entity.status().expect("approve entity").success());

    let mut link_alias = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    link_alias.args([
        "matching",
        "link-alias",
        "fixture",
        "model-family",
        "hf:fixture/model-family",
    ]);
    environment(&mut link_alias);
    assert!(link_alias.status().expect("link alias").success());

    let mut explain_entity = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    explain_entity.args(["matching", "explain", "fixture", "model-family"]);
    environment(&mut explain_entity);
    let output = explain_entity.output().expect("explain entity");
    assert!(output.status.success());
    let explained: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("entity explain JSON");
    assert_eq!(explained["status"], "approved");
    assert_eq!(explained["source"], "canonical_entity");
    assert!(
        explained["identity_evidence"]
            .as_array()
            .is_some_and(|evidence| evidence.iter().any(|item| item["source"] == "operator"))
    );

    let store = RoutingStore::open(Some(&state_path)).expect("reopen store");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "replacement-model",
                50.0,
                50.0,
                50.0,
                1.0,
                2.0,
            )],
        )
        .expect("replace benchmarks");
    drop(store);
    let mut check = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    check.args(["matching", "reconcile", "--provider", "fixture", "--check"]);
    environment(&mut check);
    assert!(!check.status().expect("drift check").success());

    let mut unlink_alias = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    unlink_alias.args(["matching", "unlink-alias", "fixture", "model-family"]);
    environment(&mut unlink_alias);
    assert!(unlink_alias.status().expect("unlink alias").success());

    let mut remove_entity = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    remove_entity.args([
        "matching",
        "remove-entity",
        "hf:fixture/model-family",
        "model-family-2025",
    ]);
    environment(&mut remove_entity);
    assert!(remove_entity.status().expect("remove entity").success());

    let mut invalid = Command::new(env!("CARGO_BIN_EXE_model-gateway"));
    invalid.args([
        "matching",
        "approve",
        "fixture",
        "model-family",
        "missing-benchmark",
    ]);
    environment(&mut invalid);
    assert!(!invalid.status().expect("invalid approval").success());
}
