use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::benchmarks::BenchmarkModel;
use crate::config::{
    BillingMode, ProviderConfig, ProviderProfileId, QuotaBoundary, QuotaKind, QuotaLimit,
};
use crate::identity::IdentityImport;
use crate::pricing::{
    EffectivePrice, PriceObservation, PriceScope, PriceSourceKind, normalize_price_id,
};
use crate::providers::{AccountLimit, is_specialty_model};

#[derive(Debug, Error)]
pub enum RoutingError {
    #[error("routing state I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("routing state database failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("routing state lock was poisoned")]
    Lock,
    #[error("routing state schema version {0} is newer than this gateway supports")]
    UnsupportedSchema(i64),
    #[error("routing background operation failed: {0}")]
    Background(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogOffering {
    pub provider: String,
    pub model: String,
    pub refreshed_at: i64,
    pub access_kind: AccessKind,
    pub context_length: Option<u64>,
    pub supports_tools: Option<bool>,
    pub supports_vision: Option<bool>,
    pub supports_structured_output: Option<bool>,
    pub input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    ZeroPrice,
    QuotaLimitedFreeTier,
    Paid,
    Unknown,
}

impl AccessKind {
    pub const fn is_free(self) -> bool {
        matches!(self, Self::ZeroPrice | Self::QuotaLimitedFreeTier)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ZeroPrice => "zero_price",
            Self::QuotaLimitedFreeTier => "quota_limited_free_tier",
            Self::Paid => "paid",
            Self::Unknown => "unknown",
        }
    }

    fn from_database(value: &str) -> Self {
        match value {
            "zero_price" => Self::ZeroPrice,
            "quota_limited_free_tier" => Self::QuotaLimitedFreeTier,
            "paid" => Self::Paid,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedModelMapping {
    pub provider: String,
    pub catalog_model: String,
    pub benchmark_model: String,
    pub approved_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IdentityAliasEvidence {
    pub source: String,
    pub provider_key: String,
    pub provider_model_id: String,
    pub entity_id: String,
    pub confidence: String,
    pub provenance_url: String,
    pub family: Option<String>,
    pub release_date: Option<String>,
    pub hugging_face_id: Option<String>,
    pub approved_benchmark_id: Option<String>,
}

pub type ApprovedIdentityReference = (String, String, String);

#[derive(Debug, Clone)]
pub struct CatalogRecord {
    pub model: String,
    pub access_kind: AccessKind,
    pub context_length: Option<u64>,
    pub supports_tools: Option<bool>,
    pub supports_vision: Option<bool>,
    pub supports_structured_output: Option<bool>,
    pub input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct QuotaReference {
    pub rules: Vec<QuotaLimit>,
    pub source_url: &'static str,
    pub as_of: &'static str,
    pub scope: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderLimitReference {
    pub profile: ProviderProfileId,
    pub source_url: &'static str,
    pub status: &'static str,
}

pub type AccountLimitStatus = (
    String,
    i64,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<bool>,
);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccountLimitSnapshot {
    pub fetched_at: i64,
    pub remaining: Option<f64>,
    pub is_free_tier: Option<bool>,
}
pub type PricingSnapshotStatus = (String, String, i64, u64, String);

pub const PROVIDER_LIMIT_REFERENCES: &[ProviderLimitReference] = &[
    limit(ProviderProfileId::Custom, "", "user_defined"),
    limit(
        ProviderProfileId::OpenRouter,
        "https://openrouter.ai/docs/api/reference/limits",
        "published_static",
    ),
    limit(
        ProviderProfileId::Ollama,
        "https://github.com/ollama/ollama",
        "local_capacity",
    ),
    limit(
        ProviderProfileId::LmStudio,
        "https://lmstudio.ai/docs",
        "local_capacity",
    ),
    limit(
        ProviderProfileId::OpenaiApi,
        "https://platform.openai.com/docs/guides/rate-limits",
        "account_specific",
    ),
    limit(
        ProviderProfileId::Anthropic,
        "https://docs.anthropic.com/en/api/rate-limits",
        "account_specific",
    ),
    limit(
        ProviderProfileId::Deepseek,
        "https://api-docs.deepseek.com/quick_start/rate_limit",
        "dynamic_concurrency",
    ),
    limit(
        ProviderProfileId::Fireworks,
        "https://docs.fireworks.ai/serverless/rate-limits",
        "adaptive",
    ),
    limit(
        ProviderProfileId::Zai,
        "https://docs.z.ai/devpack/usage-policy",
        "published_partial",
    ),
    limit(
        ProviderProfileId::GoogleGemini,
        "https://ai.google.dev/gemini-api/docs/rate-limits",
        "published_static",
    ),
    limit(
        ProviderProfileId::KiloCode,
        "https://kilo.ai/docs/gateway/usage-and-billing",
        "published_static",
    ),
    limit(
        ProviderProfileId::OpenCode,
        "https://opencode.ai/docs/zen/",
        "subscription_value_windows",
    ),
    limit(
        ProviderProfileId::OpenCodeGo,
        "https://opencode.ai/docs/go/",
        "subscription_value_windows",
    ),
    limit(
        ProviderProfileId::Mistral,
        "https://docs.mistral.ai/admin/billing-usage/usage-limits",
        "account_api",
    ),
    limit(
        ProviderProfileId::NousPortal,
        "https://inference-api.nousresearch.com/v1",
        "published_partial",
    ),
    limit(
        ProviderProfileId::NvidiaNim,
        "https://build.nvidia.com",
        "dashboard_only",
    ),
    limit(
        ProviderProfileId::Groq,
        "https://console.groq.com/docs/rate-limits",
        "published_static",
    ),
    limit(
        ProviderProfileId::OrcaRouter,
        "https://docs.orcarouter.ai/operations/billing-and-usage",
        "account_api",
    ),
    limit(
        ProviderProfileId::OllamaCloud,
        "https://docs.ollama.com/cloud",
        "gpu_time_windows",
    ),
    limit(
        ProviderProfileId::SiliconFlow,
        "https://docs.siliconflow.com/en/userguide/rate-limits/rate-limit-and-upgradation",
        "published_model_tiers",
    ),
];

const fn limit(
    profile: ProviderProfileId,
    source_url: &'static str,
    status: &'static str,
) -> ProviderLimitReference {
    ProviderLimitReference {
        profile,
        source_url,
        status,
    }
}

pub fn provider_limit_reference(
    profile: ProviderProfileId,
) -> Option<&'static ProviderLimitReference> {
    PROVIDER_LIMIT_REFERENCES
        .iter()
        .find(|reference| reference.profile == profile)
}

pub struct RoutingStore {
    connection: Mutex<Connection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationRelease {
    BeforeDispatch,
    KnownFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationOutcome {
    Reserved(ReservationToken),
    Cooldown,
    QuotaExceeded(QuotaKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservationToken {
    pub id: i64,
}

const RESERVATION_TTL_SECONDS: i64 = 3_600;

impl RoutingStore {
    pub fn open(path: Option<&Path>) -> Result<Self, RoutingError> {
        let connection = match path {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                    set_unix_mode(parent, 0o700)?;
                }
                let connection = Connection::open(path)?;
                set_unix_mode(path, 0o600)?;
                connection
            }
            None => Connection::open_in_memory()?,
        };
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > 7 {
            return Err(RoutingError::UnsupportedSchema(version));
        }
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS catalog_models (
                 provider TEXT NOT NULL,
                 model TEXT NOT NULL,
                 is_free INTEGER NOT NULL,
                 refreshed_at INTEGER NOT NULL,
                 context_length INTEGER,
                 supports_tools INTEGER,
                 supports_vision INTEGER,
                  supports_structured_output INTEGER,
                  input_price_per_million REAL,
                 output_price_per_million REAL,
                  access_kind TEXT NOT NULL DEFAULT 'unknown',
                  PRIMARY KEY (provider, model)
             );
             CREATE TABLE IF NOT EXISTS usage_counters (
                 provider TEXT NOT NULL,
                 model TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 window_seconds INTEGER NOT NULL,
                 window_start INTEGER NOT NULL,
                 used INTEGER NOT NULL,
                 PRIMARY KEY (provider, model, kind, window_seconds, window_start)
             );
             CREATE TABLE IF NOT EXISTS cooldowns (
                 provider TEXT NOT NULL,
                 model TEXT NOT NULL,
                 until_epoch INTEGER NOT NULL,
                 failures INTEGER NOT NULL,
                 PRIMARY KEY (provider, model)
             );
             CREATE TABLE IF NOT EXISTS session_pins (
                 session_hash TEXT NOT NULL,
                 route TEXT NOT NULL,
                 provider TEXT NOT NULL,
                 model TEXT NOT NULL,
                 expires_at INTEGER NOT NULL,
                 PRIMARY KEY (session_hash, route)
             );
             CREATE TABLE IF NOT EXISTS routing_meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS benchmark_snapshots (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 source TEXT NOT NULL,
                 fetched_at INTEGER NOT NULL,
                 active INTEGER NOT NULL DEFAULT 0,
                 attribution TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS benchmark_scores (
                 snapshot_id INTEGER NOT NULL,
                 canonical_model TEXT NOT NULL,
                 metric TEXT NOT NULL,
                 score REAL NOT NULL,
                 input_price REAL,
                 output_price REAL,
                 latency_seconds REAL,
                 PRIMARY KEY (snapshot_id, canonical_model, metric),
                 FOREIGN KEY (snapshot_id) REFERENCES benchmark_snapshots(id) ON DELETE CASCADE
             );
              CREATE TABLE IF NOT EXISTS benchmark_models (
                 snapshot_id INTEGER NOT NULL,
                 model_id TEXT NOT NULL,
                 creator TEXT,
                 general_quality REAL,
                 coding_quality REAL,
                 agentic_quality REAL,
                 reasoning_quality REAL,
                 input_price REAL,
                 output_price REAL,
                  latency_seconds REAL,
                  output_tokens_per_task INTEGER,
                  reasoning_effort TEXT,
                  as_of TEXT,
                  harness TEXT,
                  confidence REAL,
                 PRIMARY KEY (snapshot_id, model_id, reasoning_effort),
                  FOREIGN KEY (snapshot_id) REFERENCES benchmark_snapshots(id) ON DELETE CASCADE
              );
              CREATE TABLE IF NOT EXISTS reservations (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   provider TEXT NOT NULL,
                   model TEXT NOT NULL,
                   expires_at INTEGER NOT NULL
               );
              CREATE TABLE IF NOT EXISTS reservation_dimensions (
                   reservation_id INTEGER NOT NULL,
                   kind TEXT NOT NULL,
                   window_seconds INTEGER NOT NULL,
                   window_start INTEGER NOT NULL,
                   amount INTEGER NOT NULL,
                   PRIMARY KEY (reservation_id, kind, window_seconds, window_start),
                   FOREIGN KEY (reservation_id) REFERENCES reservations(id) ON DELETE CASCADE
               );
              CREATE TABLE IF NOT EXISTS provider_account_limits (
                   provider TEXT PRIMARY KEY,
                   fetched_at INTEGER NOT NULL,
                   limit_value REAL,
                   usage REAL,
                   remaining REAL,
                   is_free_tier INTEGER
                );
                CREATE TABLE IF NOT EXISTS pricing_snapshots (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    source TEXT NOT NULL,
                    source_kind TEXT NOT NULL,
                    fetched_at INTEGER NOT NULL,
                    active INTEGER NOT NULL DEFAULT 0,
                    attribution TEXT NOT NULL
                );
                 CREATE TABLE IF NOT EXISTS price_observations (
                    snapshot_id INTEGER NOT NULL,
                    scope TEXT NOT NULL,
                    provider_key TEXT,
                    model_id TEXT NOT NULL,
                    input_price REAL,
                    output_price REAL,
                    cache_read_price REAL,
                    cache_write_price REAL,
                    reasoning_price REAL,
                    input_audio_price REAL,
                    output_audio_price REAL,
                    request_price REAL,
                    as_of TEXT,
                    valid_from INTEGER,
                    valid_until INTEGER,
                    PRIMARY KEY (snapshot_id, scope, provider_key, model_id),
                     FOREIGN KEY (snapshot_id) REFERENCES pricing_snapshots(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS approved_model_mappings (
                     provider TEXT NOT NULL,
                     catalog_model TEXT NOT NULL,
                     benchmark_model TEXT NOT NULL,
                     approved_at INTEGER NOT NULL,
                     PRIMARY KEY (provider, catalog_model)
                 );
                 CREATE TABLE IF NOT EXISTS identity_snapshots (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     source TEXT NOT NULL,
                     fetched_at INTEGER NOT NULL,
                     active INTEGER NOT NULL DEFAULT 0,
                     attribution TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS model_entities (
                     id TEXT PRIMARY KEY,
                     creator TEXT,
                     family TEXT,
                     version TEXT,
                     variant TEXT,
                     release_date TEXT,
                     hugging_face_id TEXT,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS model_identity_aliases (
                     snapshot_id INTEGER NOT NULL,
                     source TEXT NOT NULL,
                     provider_key TEXT NOT NULL,
                     provider_model_id TEXT NOT NULL,
                     entity_id TEXT NOT NULL,
                     confidence TEXT NOT NULL,
                     provenance_url TEXT NOT NULL,
                     observed_at INTEGER NOT NULL,
                     PRIMARY KEY (snapshot_id, provider_key, provider_model_id),
                     FOREIGN KEY (snapshot_id) REFERENCES identity_snapshots(id) ON DELETE CASCADE,
                     FOREIGN KEY (entity_id) REFERENCES model_entities(id)
                 );
                 CREATE TABLE IF NOT EXISTS benchmark_identity_links (
                     entity_id TEXT NOT NULL,
                     benchmark_source TEXT NOT NULL,
                     benchmark_id TEXT NOT NULL,
                     reasoning_effort TEXT NOT NULL DEFAULT '',
                     confidence TEXT NOT NULL,
                     provenance_url TEXT NOT NULL,
                     observed_at INTEGER NOT NULL,
                     approved_at INTEGER,
                     PRIMARY KEY (entity_id, benchmark_source, benchmark_id, reasoning_effort),
                     FOREIGN KEY (entity_id) REFERENCES model_entities(id)
                 );
                 CREATE TABLE IF NOT EXISTS approved_entity_aliases (
                     provider_key TEXT NOT NULL,
                     provider_model_id TEXT NOT NULL,
                     entity_id TEXT NOT NULL,
                     provenance_url TEXT NOT NULL,
                     approved_at INTEGER NOT NULL,
                     PRIMARY KEY (provider_key, provider_model_id),
                     FOREIGN KEY (entity_id) REFERENCES model_entities(id)
                 );",
        )?;
        ensure_catalog_columns(&connection)?;
        connection.execute(
            "UPDATE catalog_models
             SET access_kind = CASE
                 WHEN is_free = 0 THEN 'paid'
                 WHEN input_price_per_million = 0 AND output_price_per_million = 0
                     THEN 'zero_price'
                 ELSE 'quota_limited_free_tier'
             END
             WHERE access_kind = 'unknown'",
            [],
        )?;
        ensure_benchmark_columns(&connection)?;
        connection.execute(
            "DELETE FROM benchmark_snapshots WHERE source = 'pricing-overrides'",
            [],
        )?;
        connection.pragma_update(None, "user_version", 7)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn approve_model_mapping(
        &self,
        provider: &str,
        catalog_model: &str,
        benchmark_model: &str,
    ) -> Result<(), RoutingError> {
        for (label, value) in [
            ("provider", provider),
            ("catalog model", catalog_model),
            ("benchmark model", benchmark_model),
        ] {
            if value.trim().is_empty() || value.len() > 512 {
                return Err(RoutingError::Background(format!(
                    "approved mapping {label} must be 1-512 characters"
                )));
            }
        }
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        connection.execute(
            "INSERT INTO approved_model_mappings(
                provider, catalog_model, benchmark_model, approved_at
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider, catalog_model) DO UPDATE SET
                benchmark_model = excluded.benchmark_model,
                approved_at = excluded.approved_at",
            params![provider, catalog_model, benchmark_model, epoch_seconds()],
        )?;
        Ok(())
    }

    pub fn remove_model_mapping(
        &self,
        provider: &str,
        catalog_model: &str,
    ) -> Result<bool, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        Ok(connection.execute(
            "DELETE FROM approved_model_mappings WHERE provider = ?1 AND catalog_model = ?2",
            params![provider, catalog_model],
        )? > 0)
    }

    pub fn approved_model_mappings(&self) -> Result<Vec<ApprovedModelMapping>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT provider, catalog_model, benchmark_model, approved_at
             FROM approved_model_mappings ORDER BY provider, catalog_model",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(ApprovedModelMapping {
                    provider: row.get(0)?,
                    catalog_model: row.get(1)?,
                    benchmark_model: row.get(2)?,
                    approved_at: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn replace_identity_source(&self, import: &IdentityImport) -> Result<i64, RoutingError> {
        if import.source.trim().is_empty()
            || import.attribution.trim().is_empty()
            || import.entities.is_empty()
            || import.aliases.is_empty()
        {
            return Err(RoutingError::Background(
                "identity snapshot requires source, attribution, entities, and aliases".to_owned(),
            ));
        }
        if import
            .aliases
            .iter()
            .any(|alias| alias.source != import.source)
        {
            return Err(RoutingError::Background(
                "identity alias source must match snapshot source".to_owned(),
            ));
        }
        let mut connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE identity_snapshots SET active = 0 WHERE source = ?1",
            [&import.source],
        )?;
        transaction.execute(
            "INSERT INTO identity_snapshots(source, fetched_at, active, attribution)
             VALUES (?1, ?2, 0, ?3)",
            params![import.source, epoch_seconds(), import.attribution],
        )?;
        let snapshot_id = transaction.last_insert_rowid();
        for entity in &import.entities {
            transaction.execute(
                "INSERT INTO model_entities(
                    id, creator, family, version, variant, release_date,
                    hugging_face_id, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET
                    creator = COALESCE(excluded.creator, model_entities.creator),
                    family = COALESCE(excluded.family, model_entities.family),
                    version = COALESCE(excluded.version, model_entities.version),
                    variant = COALESCE(excluded.variant, model_entities.variant),
                    release_date = COALESCE(excluded.release_date, model_entities.release_date),
                    hugging_face_id = COALESCE(excluded.hugging_face_id, model_entities.hugging_face_id),
                    updated_at = excluded.updated_at",
                params![
                    entity.id,
                    entity.creator,
                    entity.family,
                    entity.version,
                    entity.variant,
                    entity.release_date,
                    entity.hugging_face_id,
                    epoch_seconds(),
                ],
            )?;
        }
        for alias in &import.aliases {
            transaction.execute(
                "INSERT INTO model_identity_aliases(
                    snapshot_id, source, provider_key, provider_model_id, entity_id,
                    confidence, provenance_url, observed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    snapshot_id,
                    alias.source,
                    alias.provider_key,
                    alias.provider_model_id,
                    alias.entity_id,
                    alias.confidence.as_str(),
                    alias.provenance_url,
                    alias.observed_at,
                ],
            )?;
        }
        transaction.execute(
            "UPDATE identity_snapshots SET active = 1 WHERE id = ?1",
            [snapshot_id],
        )?;
        transaction.commit()?;
        Ok(snapshot_id)
    }

    pub fn identity_status(&self) -> Result<Vec<(String, i64, u64, String)>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT s.source, s.fetched_at, COUNT(a.provider_model_id), s.attribution
             FROM identity_snapshots s
             LEFT JOIN model_identity_aliases a ON a.snapshot_id = s.id
             WHERE s.active = 1 GROUP BY s.id ORDER BY s.source",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn active_identity_aliases(&self) -> Result<Vec<IdentityAliasEvidence>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT a.source, a.provider_key, a.provider_model_id, a.entity_id,
                    a.confidence, a.provenance_url, e.family, e.release_date,
                    e.hugging_face_id,
                    (SELECT l.benchmark_id FROM benchmark_identity_links l
                     WHERE l.entity_id = a.entity_id AND l.approved_at IS NOT NULL
                     ORDER BY l.approved_at DESC LIMIT 1)
             FROM model_identity_aliases a
             JOIN identity_snapshots s ON s.id = a.snapshot_id AND s.active = 1
             JOIN model_entities e ON e.id = a.entity_id
             UNION ALL
             SELECT 'operator', a.provider_key, a.provider_model_id, a.entity_id,
                    'approved_alias', a.provenance_url, e.family, e.release_date,
                    e.hugging_face_id,
                    (SELECT l.benchmark_id FROM benchmark_identity_links l
                     WHERE l.entity_id = a.entity_id AND l.approved_at IS NOT NULL
                     ORDER BY l.approved_at DESC LIMIT 1)
             FROM approved_entity_aliases a
             JOIN model_entities e ON e.id = a.entity_id
             ORDER BY 1, 2, 3",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(IdentityAliasEvidence {
                    source: row.get(0)?,
                    provider_key: row.get(1)?,
                    provider_model_id: row.get(2)?,
                    entity_id: row.get(3)?,
                    confidence: row.get(4)?,
                    provenance_url: row.get(5)?,
                    family: row.get(6)?,
                    release_date: row.get(7)?,
                    hugging_face_id: row.get(8)?,
                    approved_benchmark_id: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn approved_identity_references(
        &self,
    ) -> Result<Vec<ApprovedIdentityReference>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT a.provider_key, a.provider_model_id, l.benchmark_id
             FROM model_identity_aliases a
             JOIN identity_snapshots s ON s.id = a.snapshot_id AND s.active = 1
             JOIN benchmark_identity_links l
               ON l.entity_id = a.entity_id AND l.approved_at IS NOT NULL
             WHERE a.confidence = 'canonical_reference'
             UNION ALL
             SELECT a.provider_key, a.provider_model_id, l.benchmark_id
             FROM approved_entity_aliases a
             JOIN benchmark_identity_links l
               ON l.entity_id = a.entity_id AND l.approved_at IS NOT NULL
             ORDER BY 1, 2, 3",
        )?;
        Ok(statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn approve_benchmark_identity_link(
        &self,
        entity_id: &str,
        benchmark_id: &str,
        provenance_url: &str,
    ) -> Result<(), RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        connection.execute(
            "INSERT INTO benchmark_identity_links(
                entity_id, benchmark_source, benchmark_id, reasoning_effort,
                confidence, provenance_url, observed_at, approved_at
             ) VALUES (?1, 'operator', ?2, '', 'approved', ?3, ?4, ?4)
             ON CONFLICT(entity_id, benchmark_source, benchmark_id, reasoning_effort)
             DO UPDATE SET confidence = 'approved', provenance_url = excluded.provenance_url,
                           observed_at = excluded.observed_at, approved_at = excluded.approved_at",
            params![entity_id, benchmark_id, provenance_url, epoch_seconds()],
        )?;
        Ok(())
    }

    pub fn approve_entity_alias(
        &self,
        provider_key: &str,
        provider_model_id: &str,
        entity_id: &str,
        provenance_url: &str,
    ) -> Result<(), RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        connection.execute(
            "INSERT INTO approved_entity_aliases(
                provider_key, provider_model_id, entity_id, provenance_url, approved_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(provider_key, provider_model_id) DO UPDATE SET
                entity_id = excluded.entity_id,
                provenance_url = excluded.provenance_url,
                approved_at = excluded.approved_at",
            params![
                provider_key,
                provider_model_id,
                entity_id,
                provenance_url,
                epoch_seconds()
            ],
        )?;
        Ok(())
    }

    pub fn remove_entity_alias(
        &self,
        provider_key: &str,
        provider_model_id: &str,
    ) -> Result<bool, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        Ok(connection.execute(
            "DELETE FROM approved_entity_aliases
             WHERE provider_key = ?1 AND provider_model_id = ?2",
            params![provider_key, provider_model_id],
        )? > 0)
    }

    pub fn remove_benchmark_identity_link(
        &self,
        entity_id: &str,
        benchmark_id: &str,
    ) -> Result<bool, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        Ok(connection.execute(
            "DELETE FROM benchmark_identity_links
             WHERE entity_id = ?1 AND benchmark_source = 'operator' AND benchmark_id = ?2",
            params![entity_id, benchmark_id],
        )? > 0)
    }

    pub fn replace_pricing(
        &self,
        source: &str,
        source_kind: PriceSourceKind,
        attribution: &str,
        observations: &[PriceObservation],
    ) -> Result<i64, RoutingError> {
        if source.trim().is_empty() || attribution.trim().is_empty() || observations.is_empty() {
            return Err(RoutingError::Background(
                "pricing snapshot requires source, attribution, and observations".to_owned(),
            ));
        }
        for observation in observations {
            observation.validate().map_err(RoutingError::Background)?;
            if observation.source_kind != source_kind {
                return Err(RoutingError::Background(format!(
                    "pricing observation '{}' has source kind inconsistent with snapshot",
                    observation.model_id
                )));
            }
        }
        let mut connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE pricing_snapshots SET active = 0 WHERE source = ?1",
            [source],
        )?;
        transaction.execute(
            "INSERT INTO pricing_snapshots(source, source_kind, fetched_at, active, attribution)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![source, source_kind.as_str(), epoch_seconds(), attribution],
        )?;
        let snapshot_id = transaction.last_insert_rowid();
        for observation in observations {
            transaction.execute(
                "INSERT INTO price_observations(
                    snapshot_id, scope, provider_key, model_id, input_price, output_price,
                    cache_read_price, cache_write_price, reasoning_price, input_audio_price,
                    output_audio_price, request_price, as_of, valid_from, valid_until
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    snapshot_id,
                    observation.scope.as_str(),
                    observation.provider_key.as_deref(),
                    observation.model_id.as_str(),
                    observation.rates.input_price_per_million,
                    observation.rates.output_price_per_million,
                    observation.rates.cache_read_price_per_million,
                    observation.rates.cache_write_price_per_million,
                    observation.rates.reasoning_price_per_million,
                    observation.rates.input_audio_price_per_million,
                    observation.rates.output_audio_price_per_million,
                    observation.rates.request_price,
                    observation.as_of,
                    observation.valid_from,
                    observation.valid_until,
                ],
            )?;
        }
        transaction.execute(
            "UPDATE pricing_snapshots SET active = 1 WHERE id = ?1",
            [snapshot_id],
        )?;
        transaction.commit()?;
        Ok(snapshot_id)
    }

    pub fn pricing_status(&self) -> Result<Vec<PricingSnapshotStatus>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT s.source, s.source_kind, s.fetched_at, COUNT(o.model_id), s.attribution
             FROM pricing_snapshots s
             LEFT JOIN price_observations o ON o.snapshot_id = s.id
             WHERE s.active = 1 GROUP BY s.id ORDER BY s.source",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn effective_price(
        &self,
        runtime_provider: &str,
        profile_key: Option<&str>,
        model: &str,
        canonical_model: Option<&str>,
        max_age_seconds: u64,
    ) -> Result<Option<EffectivePrice>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let now = epoch_seconds();
        let cutoff = now.saturating_sub(i64::try_from(max_age_seconds).unwrap_or(i64::MAX));
        let model_id = normalize_price_id(model);

        let catalog = connection
            .query_row(
                "SELECT input_price_per_million, output_price_per_million, refreshed_at
                 FROM catalog_models WHERE provider = ?1 AND lower(model) = ?2
                   AND refreshed_at >= ?3",
                params![runtime_provider, model_id, cutoff],
                |row| {
                    Ok((
                        row.get::<_, Option<f64>>(0)?,
                        row.get::<_, Option<f64>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;

        let mut observations = Vec::new();
        let mut statement = connection.prepare(
            "SELECT s.source, s.source_kind, o.scope, o.provider_key, o.model_id,
                    o.input_price, o.output_price, o.cache_read_price, o.cache_write_price,
                    o.reasoning_price, o.input_audio_price, o.output_audio_price, o.request_price,
                    o.as_of, o.valid_from, o.valid_until, s.fetched_at
             FROM price_observations o
             JOIN pricing_snapshots s ON s.id = o.snapshot_id
             WHERE s.active = 1 AND (s.source_kind = 'manual' OR s.fetched_at >= ?1)
               AND lower(o.model_id) = ?2
               AND (o.valid_from IS NULL OR o.valid_from <= ?3)
               AND (o.valid_until IS NULL OR o.valid_until > ?3)",
        )?;
        let rows = statement.query_map(params![cutoff, model_id, now], |row| {
            let source_kind = match row.get::<_, String>(1)?.as_str() {
                "manual" => PriceSourceKind::Manual,
                "provider_catalog" => PriceSourceKind::ProviderCatalog,
                "official_api" => PriceSourceKind::OfficialApi,
                "models_dev" => PriceSourceKind::ModelsDev,
                "aggregate" => PriceSourceKind::Aggregate,
                "benchmark" => PriceSourceKind::Benchmark,
                other => return Err(rusqlite::Error::InvalidParameterName(other.to_owned())),
            };
            let scope = match row.get::<_, String>(2)?.as_str() {
                "runtime_provider" => PriceScope::RuntimeProvider,
                "provider_profile" => PriceScope::ProviderProfile,
                "canonical" => PriceScope::Canonical,
                other => return Err(rusqlite::Error::InvalidParameterName(other.to_owned())),
            };
            Ok(PriceObservation {
                source: row.get(0)?,
                source_kind,
                scope,
                provider_key: row.get(3)?,
                model_id: row.get(4)?,
                rates: crate::pricing::PriceRates {
                    input_price_per_million: row.get(5)?,
                    output_price_per_million: row.get(6)?,
                    cache_read_price_per_million: row.get(7)?,
                    cache_write_price_per_million: row.get(8)?,
                    reasoning_price_per_million: row.get(9)?,
                    input_audio_price_per_million: row.get(10)?,
                    output_audio_price_per_million: row.get(11)?,
                    request_price: row.get(12)?,
                    modifiers: BTreeMap::new(),
                },
                fetched_at: row.get(16)?,
                as_of: row.get(13)?,
                valid_from: row.get(14)?,
                valid_until: row.get(15)?,
                attribution: None,
            })
        })?;
        for row in rows {
            observations.push(row?);
        }

        let mut target_candidates = observations
            .into_iter()
            .filter(|observation| {
                (observation.scope == PriceScope::RuntimeProvider
                    && observation
                        .provider_key
                        .as_deref()
                        .is_some_and(|key| key.eq_ignore_ascii_case(runtime_provider)))
                    || (observation.scope == PriceScope::ProviderProfile
                        && profile_key.is_some_and(|key| {
                            observation
                                .provider_key
                                .as_deref()
                                .is_some_and(|value| value.eq_ignore_ascii_case(key))
                        }))
            })
            .collect::<Vec<_>>();

        if let Some((Some(input), Some(output), refreshed_at)) = catalog {
            target_candidates.push(PriceObservation {
                source: format!("catalog:{runtime_provider}"),
                source_kind: PriceSourceKind::ProviderCatalog,
                scope: PriceScope::RuntimeProvider,
                provider_key: Some(runtime_provider.to_owned()),
                model_id: model.to_owned(),
                rates: crate::pricing::PriceRates {
                    input_price_per_million: Some(input),
                    output_price_per_million: Some(output),
                    ..crate::pricing::PriceRates::default()
                },
                fetched_at: Some(refreshed_at),
                as_of: None,
                valid_from: None,
                valid_until: None,
                attribution: None,
            });
        }

        target_candidates.retain(|observation| observation.rates.is_complete());
        target_candidates.sort_by(|left, right| {
            let left_target = u8::from(left.scope != PriceScope::RuntimeProvider);
            let right_target = u8::from(right.scope != PriceScope::RuntimeProvider);
            left_target
                .cmp(&right_target)
                .then_with(|| {
                    left.source_kind
                        .fallback_priority()
                        .cmp(&right.source_kind.fallback_priority())
                })
                .then_with(|| right.fetched_at.cmp(&left.fetched_at))
                .then_with(|| left.source.cmp(&right.source))
        });
        if let Some(observation) = target_candidates.first() {
            return Ok(EffectivePrice::from_observation(observation, false));
        }

        let Some(canonical_model) = canonical_model else {
            return Ok(None);
        };
        let Some((canonical_provider, canonical_id)) = canonical_model.split_once('/') else {
            return Ok(None);
        };
        let canonical_id = normalize_price_id(canonical_id);
        let mut canonical_candidates = statement
            .query_map(params![cutoff, canonical_id, now], |row| {
                let source_kind = match row.get::<_, String>(1)?.as_str() {
                    "manual" => PriceSourceKind::Manual,
                    "provider_catalog" => PriceSourceKind::ProviderCatalog,
                    "official_api" => PriceSourceKind::OfficialApi,
                    "models_dev" => PriceSourceKind::ModelsDev,
                    "aggregate" => PriceSourceKind::Aggregate,
                    "benchmark" => PriceSourceKind::Benchmark,
                    other => return Err(rusqlite::Error::InvalidParameterName(other.to_owned())),
                };
                let scope = match row.get::<_, String>(2)?.as_str() {
                    "runtime_provider" => PriceScope::RuntimeProvider,
                    "provider_profile" => PriceScope::ProviderProfile,
                    "canonical" => PriceScope::Canonical,
                    other => return Err(rusqlite::Error::InvalidParameterName(other.to_owned())),
                };
                Ok(PriceObservation {
                    source: row.get(0)?,
                    source_kind,
                    scope,
                    provider_key: row.get(3)?,
                    model_id: row.get(4)?,
                    rates: crate::pricing::PriceRates {
                        input_price_per_million: row.get(5)?,
                        output_price_per_million: row.get(6)?,
                        cache_read_price_per_million: row.get(7)?,
                        cache_write_price_per_million: row.get(8)?,
                        reasoning_price_per_million: row.get(9)?,
                        input_audio_price_per_million: row.get(10)?,
                        output_audio_price_per_million: row.get(11)?,
                        request_price: row.get(12)?,
                        modifiers: BTreeMap::new(),
                    },
                    fetched_at: row.get(16)?,
                    as_of: row.get(13)?,
                    valid_from: row.get(14)?,
                    valid_until: row.get(15)?,
                    attribution: None,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        canonical_candidates.retain(|observation| {
            observation.rates.is_complete()
                && observation
                    .provider_key
                    .as_deref()
                    .is_some_and(|key| key.eq_ignore_ascii_case(canonical_provider))
        });
        canonical_candidates.sort_by(|left, right| {
            left.source_kind
                .fallback_priority()
                .cmp(&right.source_kind.fallback_priority())
                .then_with(|| right.fetched_at.cmp(&left.fetched_at))
                .then_with(|| left.source.cmp(&right.source))
        });
        Ok(canonical_candidates
            .first()
            .and_then(|observation| EffectivePrice::from_observation(observation, true)))
    }

    pub fn replace_catalog(
        &self,
        provider: &str,
        models: &[CatalogRecord],
    ) -> Result<(), RoutingError> {
        let now = epoch_seconds();
        let mut connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let transaction = connection.transaction()?;
        let version_map = build_version_map(&transaction);
        transaction.execute("DELETE FROM catalog_models WHERE provider = ?1", [provider])?;
        for model in models {
            if is_specialty_model(&model.model) {
                continue;
            }
            if is_stale_generation(&model.model, &version_map) {
                continue;
            }
            transaction.execute(
                "INSERT INTO catalog_models(
                    provider, model, is_free, refreshed_at, context_length,
                     supports_tools, supports_vision, supports_structured_output
                     , input_price_per_million, output_price_per_million, access_kind
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    provider,
                    model.model,
                    i64::from(model.access_kind.is_free()),
                    now,
                    model.context_length,
                    optional_bool(model.supports_tools),
                    optional_bool(model.supports_vision),
                    optional_bool(model.supports_structured_output),
                    model.input_price_per_million,
                    model.output_price_per_million,
                    model.access_kind.as_str(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn upsert_offering(
        &self,
        provider: &str,
        model: &str,
        access_kind: AccessKind,
    ) -> Result<(), RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        connection.execute(
            "INSERT INTO catalog_models(provider, model, is_free, refreshed_at, access_kind)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(provider, model) DO UPDATE SET
                 is_free = excluded.is_free,
                 access_kind = excluded.access_kind,
                 refreshed_at = excluded.refreshed_at",
            params![
                provider,
                model,
                i64::from(access_kind.is_free()),
                epoch_seconds(),
                access_kind.as_str()
            ],
        )?;
        Ok(())
    }

    pub fn free_candidates(
        &self,
        max_age_seconds: u64,
    ) -> Result<Vec<CatalogOffering>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT provider, model, refreshed_at, access_kind, context_length,
                    supports_tools, supports_vision, supports_structured_output,
                    input_price_per_million, output_price_per_million
             FROM catalog_models
             WHERE access_kind IN ('zero_price', 'quota_limited_free_tier')
                 AND refreshed_at >= ?1
             ORDER BY provider, model",
        )?;
        Ok(statement
            .query_map(
                [epoch_seconds()
                    .saturating_sub(i64::try_from(max_age_seconds).unwrap_or(i64::MAX))],
                |row| {
                    Ok(CatalogOffering {
                        provider: row.get(0)?,
                        model: row.get(1)?,
                        refreshed_at: row.get(2)?,
                        access_kind: AccessKind::from_database(&row.get::<_, String>(3)?),
                        context_length: row.get(4)?,
                        supports_tools: database_bool(row.get(5)?),
                        supports_vision: database_bool(row.get(6)?),
                        supports_structured_output: database_bool(row.get(7)?),
                        input_price_per_million: row.get(8)?,
                        output_price_per_million: row.get(9)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn all_candidates(
        &self,
        max_age_seconds: u64,
    ) -> Result<Vec<CatalogOffering>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT provider, model, refreshed_at, access_kind, context_length,
                    supports_tools, supports_vision, supports_structured_output
                     , input_price_per_million, output_price_per_million
              FROM catalog_models WHERE refreshed_at >= ?1 ORDER BY provider, model",
        )?;
        Ok(statement
            .query_map(
                [epoch_seconds()
                    .saturating_sub(i64::try_from(max_age_seconds).unwrap_or(i64::MAX))],
                |row| {
                    Ok(CatalogOffering {
                        provider: row.get(0)?,
                        model: row.get(1)?,
                        refreshed_at: row.get(2)?,
                        access_kind: AccessKind::from_database(&row.get::<_, String>(3)?),
                        context_length: row.get(4)?,
                        supports_tools: database_bool(row.get(5)?),
                        supports_vision: database_bool(row.get(6)?),
                        supports_structured_output: database_bool(row.get(7)?),
                        input_price_per_million: row.get(8)?,
                        output_price_per_million: row.get(9)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn replace_benchmarks(
        &self,
        source: &str,
        attribution: &str,
        models: &[BenchmarkModel],
    ) -> Result<i64, RoutingError> {
        if source.trim().is_empty() || attribution.trim().is_empty() || models.is_empty() {
            return Err(RoutingError::Background(
                "benchmark snapshot requires source, attribution, and models".to_owned(),
            ));
        }
        let mut identities = BTreeSet::new();
        for model in models {
            model
                .validate()
                .map_err(|error| RoutingError::Background(error.to_owned()))?;
            if !identities.insert((
                model.id.as_str(),
                model.reasoning_effort.as_deref().unwrap_or(""),
            )) {
                return Err(RoutingError::Background(format!(
                    "duplicate benchmark model/effort '{}'",
                    model.id
                )));
            }
        }
        let mut connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE benchmark_snapshots SET active = 0 WHERE source = ?1",
            [source],
        )?;
        transaction.execute(
            "INSERT INTO benchmark_snapshots(source, fetched_at, active, attribution)
             VALUES (?1, ?2, 0, ?3)",
            params![source, epoch_seconds(), attribution],
        )?;
        let snapshot_id = transaction.last_insert_rowid();
        for model in models {
            transaction.execute(
                "INSERT INTO benchmark_models(
                    snapshot_id, model_id, creator, general_quality, coding_quality,
                    agentic_quality, input_price, output_price,
                    latency_seconds, output_tokens_per_task, reasoning_effort,
                    as_of, release_date
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    snapshot_id,
                    model.id,
                    model.creator,
                    model.intelligence,
                    model.coding_quality,
                    model.agentic_quality,
                    model.input_price_per_million,
                    model.output_price_per_million,
                    model.latency_seconds,
                    model.output_tokens_per_task,
                    model.reasoning_effort.as_deref().unwrap_or(""),
                    model.as_of,
                    model.release_date,
                ],
            )?;
            for (metric, score) in [
                ("general_quality", model.intelligence),
                ("coding_quality", model.coding_quality),
                ("agentic_quality", model.agentic_quality),
            ] {
                if let Some(score) = score {
                    let metric = model
                        .reasoning_effort
                        .as_deref()
                        .map_or_else(|| metric.to_owned(), |effort| format!("{metric}@{effort}"));
                    transaction.execute(
                        "INSERT INTO benchmark_scores(
                            snapshot_id, canonical_model, metric, score,
                            input_price, output_price, latency_seconds
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            snapshot_id,
                            model.id,
                            metric,
                            score,
                            model.input_price_per_million,
                            model.output_price_per_million,
                            model.latency_seconds
                        ],
                    )?;
                }
            }
        }
        transaction.execute(
            "UPDATE benchmark_snapshots SET active = 1 WHERE id = ?1",
            [snapshot_id],
        )?;
        transaction.commit()?;
        Ok(snapshot_id)
    }

    pub fn benchmark_models(
        &self,
        max_age_seconds: u64,
    ) -> Result<Vec<BenchmarkModel>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT m.model_id, m.creator, m.general_quality, m.coding_quality,
                     m.agentic_quality, m.input_price,
                     m.output_price, m.latency_seconds, m.output_tokens_per_task,
                     NULLIF(m.reasoning_effort, ''), m.as_of,
                     m.release_date
             FROM benchmark_models m
             JOIN benchmark_snapshots s ON s.id = m.snapshot_id
             WHERE s.active = 1 AND s.fetched_at >= ?1
             ORDER BY m.model_id, s.source",
        )?;
        let rows = statement
            .query_map(
                [epoch_seconds()
                    .saturating_sub(i64::try_from(max_age_seconds).unwrap_or(i64::MAX))],
                |row| {
                    Ok(BenchmarkModel {
                        id: row.get(0)?,
                        creator: row.get(1)?,
                        intelligence: row.get(2)?,
                        coding_quality: row.get(3)?,
                        agentic_quality: row.get(4)?,
                        input_price_per_million: row.get(5)?,
                        output_price_per_million: row.get(6)?,
                        latency_seconds: row.get(7)?,
                        output_tokens_per_task: row.get(8)?,
                        reasoning_effort: row.get(9)?,
                        as_of: row.get(10)?,
                        release_date: row.get(11)?,
                        raw_metrics: BTreeMap::new(),
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn benchmark_status(&self) -> Result<Vec<(String, i64, u64, String)>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT s.source, s.fetched_at, COUNT(m.model_id), s.attribution
             FROM benchmark_snapshots s
             LEFT JOIN benchmark_models m ON m.snapshot_id = s.id
             WHERE s.active = 1 GROUP BY s.id ORDER BY s.source",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn remove_benchmark_source(&self, source: &str) -> Result<(), RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let deleted = connection.execute(
            "DELETE FROM benchmark_snapshots WHERE source = ?1",
            [source],
        )?;
        if deleted == 0 {
            return Err(RoutingError::Background(format!(
                "no active snapshot for source '{source}'"
            )));
        }
        Ok(())
    }

    pub fn active_benchmark_snapshot(
        &self,
        max_age_seconds: u64,
    ) -> Result<Option<(i64, i64)>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        connection
            .query_row(
                "SELECT id, fetched_at FROM benchmark_snapshots
                 WHERE active = 1 AND fetched_at >= ?1
                 ORDER BY fetched_at DESC, id DESC LIMIT 1",
                [epoch_seconds()
                    .saturating_sub(i64::try_from(max_age_seconds).unwrap_or(i64::MAX))],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(RoutingError::from)
    }

    pub fn catalog_summary(&self) -> Result<Vec<(String, u64, i64)>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT provider, COUNT(*), MAX(refreshed_at)
             FROM catalog_models GROUP BY provider ORDER BY provider",
        )?;
        Ok(statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn record_account_limit(
        &self,
        provider: &str,
        account: &AccountLimit,
    ) -> Result<(), RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        connection.execute(
            "INSERT INTO provider_account_limits(
                provider, fetched_at, limit_value, usage, remaining, is_free_tier
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(provider) DO UPDATE SET
                fetched_at = excluded.fetched_at,
                limit_value = excluded.limit_value,
                usage = excluded.usage,
                remaining = excluded.remaining,
                is_free_tier = excluded.is_free_tier",
            params![
                provider,
                epoch_seconds(),
                account.limit,
                account.usage,
                account.remaining,
                account.is_free_tier.map(i64::from)
            ],
        )?;
        Ok(())
    }

    pub fn account_limit_status(&self) -> Result<Vec<AccountLimitStatus>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT provider, fetched_at, limit_value, usage, remaining, is_free_tier
             FROM provider_account_limits ORDER BY provider",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    database_bool(row.get(5)?),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn account_limits(&self) -> Result<BTreeMap<String, AccountLimitSnapshot>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT provider, fetched_at, remaining, is_free_tier
             FROM provider_account_limits",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    AccountLimitSnapshot {
                        fetched_at: row.get(1)?,
                        remaining: row.get(2)?,
                        is_free_tier: database_bool(row.get(3)?),
                    },
                ))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?)
    }

    pub fn reserve(
        &self,
        provider: &str,
        model: &str,
        estimated_tokens: u64,
        expected_cost_microusd: u64,
        quotas: &[QuotaLimit],
    ) -> Result<ReservationOutcome, RoutingError> {
        let now = epoch_seconds();
        let mut connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let transaction = connection.transaction()?;
        expire_reservations(&transaction, now)?;
        let cooldown: Option<i64> = transaction
            .query_row(
                "SELECT until_epoch FROM cooldowns WHERE provider = ?1 AND model = ?2",
                params![provider, model],
                |row| row.get(0),
            )
            .optional()?;
        if cooldown.is_some_and(|until| until > now) {
            return Ok(ReservationOutcome::Cooldown);
        }
        for quota in quotas {
            let amount = quota_amount(quota.kind, estimated_tokens, expected_cost_microusd);
            let window_start = quota_window_start(now, quota);
            let used: u64 = transaction
                .query_row(
                    "SELECT used FROM usage_counters
                     WHERE provider = ?1 AND model = ?2 AND kind = ?3
                       AND window_seconds = ?4 AND window_start = ?5",
                    params![
                        provider,
                        model,
                        quota_kind(quota.kind),
                        quota.window_seconds,
                        window_start
                    ],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0);
            if used.saturating_add(amount) > quota.limit {
                return Ok(ReservationOutcome::QuotaExceeded(quota.kind));
            }
        }
        for quota in quotas {
            let amount = quota_amount(quota.kind, estimated_tokens, expected_cost_microusd);
            let window_start = quota_window_start(now, quota);
            transaction.execute(
                "INSERT INTO usage_counters(
                    provider, model, kind, window_seconds, window_start, used
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(provider, model, kind, window_seconds, window_start)
                 DO UPDATE SET used = used + excluded.used",
                params![
                    provider,
                    model,
                    quota_kind(quota.kind),
                    quota.window_seconds,
                    window_start,
                    amount
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO reservations(provider, model, expires_at)
             VALUES (?1, ?2, ?3)",
            params![provider, model, now.saturating_add(RESERVATION_TTL_SECONDS)],
        )?;
        let reservation_id = transaction.last_insert_rowid();
        for quota in quotas {
            let window = i64::try_from(quota.window_seconds).unwrap_or(i64::MAX);
            let window_start = now - now.rem_euclid(window);
            transaction.execute(
                "INSERT INTO reservation_dimensions(
                    reservation_id, kind, window_seconds, window_start, amount
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    reservation_id,
                    quota_kind(quota.kind),
                    quota.window_seconds,
                    window_start,
                    quota_amount(quota.kind, estimated_tokens, expected_cost_microusd)
                ],
            )?;
        }
        transaction.execute(
            "DELETE FROM usage_counters WHERE window_start + window_seconds < ?1",
            [now],
        )?;
        transaction.execute("DELETE FROM session_pins WHERE expires_at < ?1", [now])?;
        transaction.execute("DELETE FROM cooldowns WHERE until_epoch < ?1", [now])?;
        transaction.commit()?;
        Ok(ReservationOutcome::Reserved(ReservationToken {
            id: reservation_id,
        }))
    }

    pub fn apply_cooldown(
        &self,
        provider: &str,
        model: &str,
        retry_after_seconds: Option<u64>,
    ) -> Result<(), RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let failures: u32 = connection
            .query_row(
                "SELECT failures FROM cooldowns WHERE provider = ?1 AND model = ?2",
                params![provider, model],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let next_failures = failures.saturating_add(1);
        let backoff = retry_after_seconds
            .unwrap_or_else(|| 2_u64.saturating_pow(next_failures.min(8)).clamp(2, 300));
        connection.execute(
            "INSERT INTO cooldowns(provider, model, until_epoch, failures)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider, model) DO UPDATE SET
                 until_epoch = excluded.until_epoch,
                 failures = excluded.failures",
            params![
                provider,
                model,
                epoch_seconds().saturating_add(i64::try_from(backoff).unwrap_or(300)),
                next_failures
            ],
        )?;
        Ok(())
    }

    pub fn clear_cooldown(&self, provider: &str, model: &str) -> Result<(), RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        connection.execute(
            "DELETE FROM cooldowns WHERE provider = ?1 AND model = ?2",
            params![provider, model],
        )?;
        Ok(())
    }

    pub fn release_reservation(
        &self,
        token: ReservationToken,
        release: ReservationRelease,
    ) -> Result<(), RoutingError> {
        let mut connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let transaction = connection.transaction()?;
        let reservation = transaction
            .query_row(
                "SELECT provider, model FROM reservations WHERE id = ?1",
                [token.id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((provider, model)) = reservation else {
            return Ok(());
        };
        let release_requests = matches!(release, ReservationRelease::BeforeDispatch);
        let dimensions = {
            let mut statement = transaction.prepare(
                "SELECT kind, window_seconds, window_start, amount
                 FROM reservation_dimensions WHERE reservation_id = ?1",
            )?;
            statement
                .query_map([token.id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, u64>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (kind, window_seconds, window_start, amount) in dimensions {
            if kind == "requests" && !release_requests {
                continue;
            }
            decrement_counter_at(
                &transaction,
                &provider,
                &model,
                &kind,
                window_seconds,
                window_start,
                amount,
            )?;
        }
        transaction.execute(
            "DELETE FROM reservation_dimensions WHERE reservation_id = ?1",
            [token.id],
        )?;
        transaction.execute("DELETE FROM reservations WHERE id = ?1", [token.id])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finalize_reservation(
        &self,
        token: ReservationToken,
        actual_tokens: Option<u64>,
        actual_cost_microusd: Option<u64>,
    ) -> Result<(), RoutingError> {
        let mut connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let transaction = connection.transaction()?;
        let reservation = transaction
            .query_row(
                "SELECT provider, model FROM reservations WHERE id = ?1",
                [token.id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((provider, model)) = reservation else {
            return Ok(());
        };
        let dimensions = {
            let mut statement = transaction.prepare(
                "SELECT kind, window_seconds, window_start, amount
                 FROM reservation_dimensions WHERE reservation_id = ?1",
            )?;
            statement
                .query_map([token.id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, u64>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (kind, window_seconds, window_start, reserved) in dimensions {
            let actual = match kind.as_str() {
                "tokens" => actual_tokens,
                "cost_microusd" => actual_cost_microusd,
                "concurrency" => Some(0),
                _ => None,
            };
            if let Some(actual) = actual {
                adjust_counter_at(
                    &transaction,
                    &provider,
                    &model,
                    &kind,
                    window_seconds,
                    window_start,
                    reserved,
                    actual,
                )?;
            }
        }
        transaction.execute(
            "DELETE FROM reservation_dimensions WHERE reservation_id = ?1",
            [token.id],
        )?;
        transaction.execute("DELETE FROM reservations WHERE id = ?1", [token.id])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn remove_session_pin(&self, session_hash: &str, route: &str) -> Result<(), RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        connection.execute(
            "DELETE FROM session_pins WHERE session_hash = ?1 AND route = ?2",
            params![session_hash, route],
        )?;
        Ok(())
    }

    pub fn session_hash(&self, material: &str) -> Result<String, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let salt: Option<String> = connection
            .query_row(
                "SELECT value FROM routing_meta WHERE key = 'session_salt'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let salt = match salt {
            Some(salt) => salt,
            None => {
                let seed = format!(
                    "{}:{}:{:p}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos(),
                    std::process::id(),
                    &connection
                );
                let salt = Sha256::digest(seed.as_bytes())
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<String>();
                connection.execute(
                    "INSERT OR IGNORE INTO routing_meta(key, value) VALUES ('session_salt', ?1)",
                    [&salt],
                )?;
                connection.query_row(
                    "SELECT value FROM routing_meta WHERE key = 'session_salt'",
                    [],
                    |row| row.get(0),
                )?
            }
        };
        let mut digest = Sha256::new();
        digest.update(salt.as_bytes());
        digest.update(material.as_bytes());
        Ok(digest
            .finalize()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>())
    }

    pub fn session_pin(
        &self,
        session_hash: &str,
        route: &str,
    ) -> Result<Option<(String, String)>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        Ok(connection
            .query_row(
                "SELECT provider, model FROM session_pins
                 WHERE session_hash = ?1 AND route = ?2 AND expires_at > ?3",
                params![session_hash, route, epoch_seconds()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn set_session_pin(
        &self,
        session_hash: &str,
        route: &str,
        provider: &str,
        model: &str,
        ttl_seconds: u64,
    ) -> Result<(), RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        connection.execute(
            "INSERT INTO session_pins(session_hash, route, provider, model, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_hash, route) DO UPDATE SET
                 provider = excluded.provider,
                 model = excluded.model,
                 expires_at = excluded.expires_at",
            params![
                session_hash,
                route,
                provider,
                model,
                epoch_seconds().saturating_add(i64::try_from(ttl_seconds).unwrap_or(1_800))
            ],
        )?;
        Ok(())
    }
}

pub fn classify_access(provider: &ProviderConfig, model: &str, zero_priced: bool) -> AccessKind {
    let explicitly_free = provider.free_models.iter().any(|free| free == model);
    let lower = model.to_ascii_lowercase();
    if provider.profile == Some(ProviderProfileId::OpenCodeGo) {
        return if explicitly_free {
            AccessKind::ZeroPrice
        } else {
            AccessKind::Paid
        };
    }
    if provider.profile == Some(ProviderProfileId::KiloCode) {
        return if explicitly_free || lower.contains("free") {
            AccessKind::ZeroPrice
        } else {
            AccessKind::Paid
        };
    }
    if zero_priced || explicitly_free {
        return AccessKind::ZeroPrice;
    }
    match provider.profile {
        Some(ProviderProfileId::OpenRouter)
        | Some(ProviderProfileId::NousPortal)
        | Some(ProviderProfileId::OrcaRouter) => {
            if lower.contains("free") {
                return AccessKind::ZeroPrice;
            }
        }
        Some(ProviderProfileId::OpenCode) if lower.contains("free") || lower == "big-pickle" => {
            return AccessKind::ZeroPrice;
        }
        _ => {}
    }
    if provider.billing_mode != BillingMode::Free {
        return AccessKind::Paid;
    }
    if matches!(
        provider.profile,
        Some(ProviderProfileId::GoogleGemini)
            | Some(ProviderProfileId::Groq)
            | Some(ProviderProfileId::Mistral)
            | Some(ProviderProfileId::NvidiaNim)
            | Some(ProviderProfileId::OllamaCloud)
            | Some(ProviderProfileId::SiliconFlow)
    ) {
        AccessKind::QuotaLimitedFreeTier
    } else {
        AccessKind::Paid
    }
}

pub fn is_verified_free(provider: &ProviderConfig, model: &str, zero_priced: bool) -> bool {
    classify_access(provider, model, zero_priced).is_free()
}

pub fn quota_reference(provider: &ProviderConfig, model: &str) -> Option<QuotaReference> {
    if !provider.quotas.is_empty() {
        return Some(QuotaReference {
            rules: provider.quotas.clone(),
            source_url: "user-configured",
            as_of: "runtime",
            scope: provider
                .account_scope
                .clone()
                .unwrap_or_else(|| "provider".to_owned()),
        });
    }
    if provider.billing_mode != BillingMode::Free {
        return None;
    }
    let (rules, source_url, as_of, scope) = match provider.profile {
        Some(ProviderProfileId::OpenRouter) => (
            vec![requests(20, 60), requests(1_000, 86_400)],
            "https://openrouter.ai/docs/api/reference/limits",
            "user_specified_$10_spent",
            "account",
        ),
        Some(ProviderProfileId::KiloCode) => (
            vec![requests(200, 3_600)],
            "https://kilo.ai/docs/gateway/usage-and-billing",
            "published_static",
            "ip",
        ),
        Some(ProviderProfileId::Groq) => (
            vec![
                requests(30, 60),
                requests(14_400, 86_400),
                tokens(6_000, 60),
            ],
            "https://console.groq.com/docs/rate-limits",
            "published_static",
            "organization",
        ),
        Some(ProviderProfileId::GoogleGemini) => {
            let lower = model.to_ascii_lowercase();
            let (rpm, rpd) = if lower.contains("pro") {
                (5, 100)
            } else if lower.contains("flash-lite") {
                (30, 1_500)
            } else {
                (10, 1_500)
            };
            (
                vec![
                    requests(rpm, 60),
                    requests(rpd, 86_400),
                    tokens(1_000_000, 60),
                ],
                "https://ai.google.dev/gemini-api/docs/rate-limits",
                "published_static",
                "project_model",
            )
        }
        Some(ProviderProfileId::OpenCodeGo) => (
            vec![
                cost_microusd(12_000_000, 18_000, QuotaBoundary::Rolling),
                cost_microusd(30_000_000, 604_800, QuotaBoundary::UtcWeek),
                cost_microusd(60_000_000, 2_592_000, QuotaBoundary::UtcMonth),
            ],
            "https://opencode.ai/docs/go/",
            "published_static",
            "workspace",
        ),
        Some(ProviderProfileId::Zai) => (
            vec![requests(1, 1)],
            "https://docs.z.ai/guides/overview/pricing",
            "best_effort",
            "account_model",
        ),
        Some(ProviderProfileId::Mistral) => (
            vec![requests(188, 60), tokens(625_000, 60)],
            "https://docs.mistral.ai/admin/billing-usage/usage-limits",
            "probe_verified_2026-07-23",
            "organization_model",
        ),
        Some(ProviderProfileId::NvidiaNim) => (
            vec![requests(40, 60)],
            "https://build.nvidia.com",
            "user_reported_40_rpm",
            "account",
        ),
        Some(ProviderProfileId::OllamaCloud) => (
            vec![requests(30, 60)],
            "https://docs.ollama.com/cloud",
            "best_effort",
            "account",
        ),
        Some(ProviderProfileId::NousPortal) => (
            vec![
                requests(50, 60),
                tokens(500_000, 60),
                requests(2_100, 3_600),
            ],
            "https://inference-api.nousresearch.com/v1",
            "probe_verified_2026-07-23",
            "account_model",
        ),
        Some(ProviderProfileId::SiliconFlow) => (
            vec![requests(1_000, 60), tokens(40_000, 60)],
            "https://docs.siliconflow.com/en/userguide/rate-limits/rate-limit-and-upgradation",
            "published_static",
            "account_model",
        ),
        Some(ProviderProfileId::OrcaRouter) => (
            vec![requests(10, 60)],
            "https://docs.orcarouter.ai/operations/billing-and-usage",
            "account_api",
            "account",
        ),
        Some(ProviderProfileId::OpenCode) => (
            vec![requests(50, 60), tokens(10_000, 60)],
            "https://opencode.ai/docs/zen/",
            "best_effort",
            "account",
        ),
        _ => return None,
    };
    Some(QuotaReference {
        rules,
        source_url,
        as_of,
        scope: scope.to_owned(),
    })
}

fn requests(limit: u64, window_seconds: u64) -> QuotaLimit {
    QuotaLimit {
        kind: QuotaKind::Requests,
        limit,
        window_seconds,
        boundary: QuotaBoundary::Rolling,
    }
}

fn tokens(limit: u64, window_seconds: u64) -> QuotaLimit {
    QuotaLimit {
        kind: QuotaKind::Tokens,
        limit,
        window_seconds,
        boundary: QuotaBoundary::Rolling,
    }
}

fn cost_microusd(limit: u64, window_seconds: u64, boundary: QuotaBoundary) -> QuotaLimit {
    QuotaLimit {
        kind: QuotaKind::CostMicrousd,
        limit,
        window_seconds,
        boundary,
    }
}

fn quota_amount(kind: QuotaKind, estimated_tokens: u64, expected_cost_microusd: u64) -> u64 {
    match kind {
        QuotaKind::Requests => 1,
        QuotaKind::Tokens => estimated_tokens.max(1),
        QuotaKind::CostMicrousd => expected_cost_microusd,
        QuotaKind::Concurrency => 1,
    }
}

fn quota_window_start(now: i64, quota: &QuotaLimit) -> i64 {
    let rolling = || now - now.rem_euclid(i64::try_from(quota.window_seconds).unwrap_or(i64::MAX));
    match quota.boundary {
        QuotaBoundary::Rolling => rolling(),
        QuotaBoundary::UtcMinute => now - now.rem_euclid(60),
        QuotaBoundary::UtcHour => now - now.rem_euclid(3_600),
        QuotaBoundary::UtcDay => now - now.rem_euclid(86_400),
        QuotaBoundary::UtcWeek => {
            let days = now.div_euclid(86_400);
            let weekday_from_monday = (days + 3).rem_euclid(7);
            (days - weekday_from_monday) * 86_400
        }
        QuotaBoundary::UtcMonth => {
            let days = now.div_euclid(86_400);
            let (year, month, _) = civil_from_days(days);
            days_from_civil(year, month, 1) * 86_400
        }
    }
}

pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096).div_euclid(365);
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2).div_euclid(153);
    let day = doy - (153 * mp + 2).div_euclid(5) + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 }.div_euclid(400);
    let yoe = year - era * 400;
    let month = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * month + 2).div_euclid(5) + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn quota_kind(kind: QuotaKind) -> &'static str {
    match kind {
        QuotaKind::Requests => "requests",
        QuotaKind::Tokens => "tokens",
        QuotaKind::CostMicrousd => "cost_microusd",
        QuotaKind::Concurrency => "concurrency",
    }
}

fn optional_bool(value: Option<bool>) -> Option<i64> {
    value.map(i64::from)
}

fn database_bool(value: Option<i64>) -> Option<bool> {
    value.map(|value| value != 0)
}

fn expire_reservations(
    transaction: &rusqlite::Transaction<'_>,
    now: i64,
) -> Result<(), rusqlite::Error> {
    let expired = {
        let mut statement = transaction
            .prepare("SELECT id, provider, model FROM reservations WHERE expires_at <= ?1")?;
        statement
            .query_map([now], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (id, provider, model) in expired {
        let dimensions = {
            let mut statement = transaction.prepare(
                "SELECT kind, window_seconds, window_start, amount
                 FROM reservation_dimensions WHERE reservation_id = ?1",
            )?;
            statement
                .query_map([id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, u64>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (kind, window_seconds, window_start, amount) in dimensions {
            decrement_counter_at(
                transaction,
                &provider,
                &model,
                &kind,
                window_seconds,
                window_start,
                amount,
            )?;
        }
        transaction.execute("DELETE FROM reservations WHERE id = ?1", [id])?;
    }
    Ok(())
}

fn decrement_counter_at(
    transaction: &rusqlite::Transaction<'_>,
    provider: &str,
    model: &str,
    kind: &str,
    window_seconds: u64,
    window_start: i64,
    amount: u64,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "UPDATE usage_counters SET used = MAX(0, used - ?1)
         WHERE provider = ?2 AND model = ?3 AND kind = ?4
           AND window_seconds = ?5 AND window_start = ?6",
        params![amount, provider, model, kind, window_seconds, window_start],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn adjust_counter_at(
    transaction: &rusqlite::Transaction<'_>,
    provider: &str,
    model: &str,
    kind: &str,
    window_seconds: u64,
    window_start: i64,
    reserved: u64,
    actual: u64,
) -> Result<(), rusqlite::Error> {
    if actual >= reserved {
        transaction.execute(
            "UPDATE usage_counters SET used = used + ?1
             WHERE provider = ?2 AND model = ?3 AND kind = ?4
               AND window_seconds = ?5 AND window_start = ?6",
            params![
                actual - reserved,
                provider,
                model,
                kind,
                window_seconds,
                window_start
            ],
        )?;
    } else {
        decrement_counter_at(
            transaction,
            provider,
            model,
            kind,
            window_seconds,
            window_start,
            reserved - actual,
        )?;
    }
    Ok(())
}

fn ensure_catalog_columns(connection: &Connection) -> Result<(), rusqlite::Error> {
    let mut statement = connection.prepare("PRAGMA table_info(catalog_models)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (name, sql_type) in [
        ("context_length", "INTEGER"),
        ("supports_tools", "INTEGER"),
        ("supports_vision", "INTEGER"),
        ("supports_structured_output", "INTEGER"),
        ("input_price_per_million", "REAL"),
        ("output_price_per_million", "REAL"),
        ("access_kind", "TEXT NOT NULL DEFAULT 'unknown'"),
    ] {
        if !columns.iter().any(|column| column == name) {
            connection.execute(
                &format!("ALTER TABLE catalog_models ADD COLUMN {name} {sql_type}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn ensure_benchmark_columns(connection: &Connection) -> Result<(), rusqlite::Error> {
    let mut statement = connection.prepare("PRAGMA table_info(benchmark_models)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (name, sql_type) in [("as_of", "TEXT"), ("release_date", "TEXT")] {
        if !columns.iter().any(|column| column == name) {
            connection.execute(
                &format!("ALTER TABLE benchmark_models ADD COLUMN {name} {sql_type}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// Extracts (family_name, version_number) from a normalized model ID.
/// Handles both embedded versions (gemma4 → gemma+4) and tokenized versions
/// (mistral-3 → mistral+3, mistral-medium-3-5 → mistral+3).
fn extract_model_family_version(normalized: &str) -> Option<(String, u64)> {
    let tokens: Vec<&str> = normalized.split('-').collect();

    // Case 1: letter-digit boundary within a single token (gemma4, qwen3)
    for token in &tokens {
        let bytes = token.as_bytes();
        for i in 0..bytes.len().saturating_sub(1) {
            // Family prefix must be at least 2 chars to avoid matching r1, a100, etc.
            if i >= 1 && bytes[i].is_ascii_alphabetic() && bytes[i + 1].is_ascii_digit() {
                let family = std::str::from_utf8(&bytes[..=i]).ok()?.to_lowercase();
                let mut version_end = i + 1;
                while version_end < bytes.len() && bytes[version_end].is_ascii_digit() {
                    version_end += 1;
                }
                let version_str = std::str::from_utf8(&bytes[i + 1..version_end]).ok()?;
                let version: u64 = version_str.parse().ok()?;
                return Some((family, version));
            }
        }
    }

    // Case 2: find first all-alpha family token where the NEXT token
    // is a numeric version (handles google/gemma-3 → gemma+3,
    // skips provider prefixes like "google" since the next token isn't a number)
    for i in 0..tokens.len() {
        let token = tokens[i];
        if token.is_empty() || !token.chars().all(|c| c.is_ascii_alphabetic()) || token.len() < 2 {
            continue;
        }
        if i + 1 < tokens.len() {
            if let Ok(version) = tokens[i + 1].parse::<u64>() {
                return Some((token.to_lowercase(), version));
            }
        }
    }

    None
}

/// Builds a map of model family → max version from active AA benchmarks.
/// Used to detect stale catalog models from older generations.
fn build_version_map(connection: &Connection) -> BTreeMap<String, u64> {
    let mut map = BTreeMap::new();
    if let Ok(mut statement) =
        connection.prepare("SELECT model_id FROM benchmark_models WHERE snapshot_id IN (SELECT id FROM benchmark_snapshots WHERE active = 1)")
    {
        if let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(0)) {
            for row in rows.flatten() {
                if let Some((family, version)) = extract_model_family_version(&row) {
                    let entry = map.entry(family).or_insert(0);
                    if version > *entry {
                        *entry = version;
                    }
                }
            }
        }
    }
    map
}

/// Returns true if a catalog model is from an older generation than what AA benchmarks.
fn is_stale_generation(model: &str, version_map: &BTreeMap<String, u64>) -> bool {
    let normalized = model
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if let Some((family, cat_version)) = extract_model_family_version(&normalized) {
        if let Some(&aa_max_version) = version_map.get(&family) {
            return cat_version < aa_max_version;
        }
    }
    false
}

fn set_unix_mode(path: &Path, mode: u32) -> Result<(), std::io::Error> {
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
    use std::fs;
    use std::sync::Arc;

    use crate::config::{
        BillingMode, ProviderConfig, ProviderProfileId, QuotaBoundary, QuotaKind, QuotaLimit,
    };

    use crate::benchmarks::BenchmarkModel;
    use crate::identity::{
        IdentityAliasRecord, IdentityConfidence, IdentityEntityRecord, IdentityImport,
    };
    use crate::pricing::{PriceObservation, PriceRates, PriceScope, PriceSourceKind};
    use crate::providers::AccountLimit;

    use super::{AccessKind, CatalogRecord, ReservationOutcome, RoutingStore};

    #[test]
    fn catalog_replacement_is_atomic_per_provider() {
        let store = RoutingStore::open(None).expect("store");
        store
            .replace_catalog("one", &[catalog("free-a", true)])
            .expect("first catalog");
        store
            .replace_catalog("one", &[catalog("paid-b", false)])
            .expect("second catalog");
        assert!(
            store
                .free_candidates(86_400)
                .expect("candidates")
                .is_empty()
        );
    }

    #[test]
    fn schema_v7_backfills_free_access_kinds() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("routing.sqlite3");
        let connection = rusqlite::Connection::open(&path).expect("legacy database");
        connection
            .execute_batch(
                "CREATE TABLE catalog_models (
                    provider TEXT NOT NULL,
                    model TEXT NOT NULL,
                    is_free INTEGER NOT NULL,
                    refreshed_at INTEGER NOT NULL,
                    context_length INTEGER,
                    supports_tools INTEGER,
                    supports_vision INTEGER,
                    supports_structured_output INTEGER,
                    input_price_per_million REAL,
                    output_price_per_million REAL,
                    PRIMARY KEY (provider, model)
                );
                INSERT INTO catalog_models VALUES
                    ('p', 'zero', 1, 9999999999, NULL, NULL, NULL, NULL, 0, 0),
                    ('p', 'quota', 1, 9999999999, NULL, NULL, NULL, NULL, 1, 5),
                    ('p', 'paid', 0, 9999999999, NULL, NULL, NULL, NULL, 1, 5);
                PRAGMA user_version = 6;",
            )
            .expect("legacy schema");
        drop(connection);

        let store = RoutingStore::open(Some(&path)).expect("migrated store");
        let offerings = store.all_candidates(u64::MAX).expect("offerings");
        let access = offerings
            .into_iter()
            .map(|offering| (offering.model, offering.access_kind))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(access["zero"], AccessKind::ZeroPrice);
        assert_eq!(access["quota"], AccessKind::QuotaLimitedFreeTier);
        assert_eq!(access["paid"], AccessKind::Paid);
        let connection = rusqlite::Connection::open(path).expect("migrated database");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 7);
    }

    #[test]
    fn quota_reservations_are_atomic_across_threads() {
        let store = Arc::new(RoutingStore::open(None).expect("store"));
        let quota = vec![QuotaLimit {
            kind: QuotaKind::Requests,
            limit: 1,
            window_seconds: 60,
            boundary: QuotaBoundary::Rolling,
        }];
        let handles = (0..4)
            .map(|_| {
                let store = store.clone();
                let quota = quota.clone();
                std::thread::spawn(move || store.reserve("p", "m", 1, 0, &quota).expect("reserve"))
            })
            .collect::<Vec<_>>();
        let accepted = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .filter(|outcome| matches!(outcome, ReservationOutcome::Reserved(_)))
            .count();
        assert_eq!(accepted, 1);
    }

    #[cfg(unix)]
    #[test]
    fn file_store_is_protected() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state").join("routing.sqlite3");
        let _store = RoutingStore::open(Some(&path)).expect("store");
        assert_eq!(
            fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn configured_free_override_is_required_for_custom_provider() {
        let mut provider = ProviderConfig::default();
        assert_eq!(
            super::classify_access(&provider, "model", false),
            AccessKind::Paid
        );
        provider.free_models.push("model".to_owned());
        assert_eq!(
            super::classify_access(&provider, "model", false),
            AccessKind::ZeroPrice
        );
    }

    #[test]
    fn provider_specific_free_tier_rules_are_explicit() {
        let mut provider = ProviderConfig {
            profile: Some(ProviderProfileId::KiloCode),
            billing_mode: BillingMode::Paid,
            ..ProviderConfig::default()
        };
        assert!(!super::is_verified_free(
            &provider,
            "provider/preview",
            true
        ));
        assert!(super::is_verified_free(&provider, "provider/free", false));

        provider.profile = Some(ProviderProfileId::OpenCode);
        assert!(super::is_verified_free(&provider, "big-pickle", false));
        assert!(super::is_verified_free(&provider, "mimo-v2.5-free", false));

        provider.profile = Some(ProviderProfileId::OpenCodeGo);
        assert!(!super::is_verified_free(&provider, "mimo-v2.5", true));
        assert!(!super::is_verified_free(&provider, "mimo-v2.5-free", false));

        provider.profile = Some(ProviderProfileId::Mistral);
        provider.billing_mode = BillingMode::Free;
        assert_eq!(
            super::classify_access(&provider, "mistral-small-latest", false),
            AccessKind::QuotaLimitedFreeTier
        );
        provider.billing_mode = BillingMode::Paid;
        assert_eq!(
            super::classify_access(&provider, "mistral-small-latest", false),
            AccessKind::Paid
        );

        provider.profile = Some(ProviderProfileId::Zai);
        provider.billing_mode = BillingMode::Free;
        assert!(!super::is_verified_free(&provider, "glm-flash", false));
    }

    fn catalog(model: &str, is_free: bool) -> CatalogRecord {
        CatalogRecord {
            model: model.to_owned(),
            access_kind: if is_free {
                AccessKind::ZeroPrice
            } else {
                AccessKind::Paid
            },
            context_length: None,
            supports_tools: None,
            supports_vision: None,
            supports_structured_output: None,
            input_price_per_million: None,
            output_price_per_million: None,
        }
    }

    fn profile_price(provider: &str, model: &str, input: f64, output: f64) -> PriceObservation {
        PriceObservation {
            source: "models.dev".to_owned(),
            source_kind: PriceSourceKind::ModelsDev,
            scope: PriceScope::ProviderProfile,
            provider_key: Some(provider.to_owned()),
            model_id: model.to_owned(),
            rates: PriceRates {
                input_price_per_million: Some(input),
                output_price_per_million: Some(output),
                ..PriceRates::default()
            },
            fetched_at: Some(1),
            as_of: None,
            valid_from: None,
            valid_until: None,
            attribution: None,
        }
    }

    #[test]
    fn approved_model_mappings_persist_update_and_remove() {
        let directory = tempfile::tempdir().expect("state directory");
        let path = directory.path().join("routing.sqlite3");
        let store = RoutingStore::open(Some(&path)).expect("store");
        store
            .approve_model_mapping("provider-a", "catalog-model", "benchmark-v1")
            .expect("approve");
        let mappings = store.approved_model_mappings().expect("mappings");
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].provider, "provider-a");
        assert_eq!(mappings[0].catalog_model, "catalog-model");
        assert_eq!(mappings[0].benchmark_model, "benchmark-v1");
        drop(store);

        let store = RoutingStore::open(Some(&path)).expect("reopen store");
        store
            .approve_model_mapping("provider-a", "catalog-model", "benchmark-v2")
            .expect("update");
        let mappings = store.approved_model_mappings().expect("updated mappings");
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].benchmark_model, "benchmark-v2");
        assert!(
            store
                .remove_model_mapping("provider-a", "catalog-model")
                .expect("remove")
        );
        assert!(
            store
                .approved_model_mappings()
                .expect("empty mappings")
                .is_empty()
        );
        assert!(
            !store
                .remove_model_mapping("provider-a", "catalog-model")
                .expect("idempotent remove")
        );
    }

    #[test]
    fn identity_sources_persist_canonical_aliases_and_approved_links() {
        let store = RoutingStore::open(None).expect("store");
        let entity_id = "hf:xiaomimimo/mimo-v2.5";
        let import = IdentityImport {
            source: "models.dev".to_owned(),
            attribution: "fixture".to_owned(),
            entities: vec![IdentityEntityRecord {
                id: entity_id.to_owned(),
                creator: Some("xiaomimimo".to_owned()),
                family: Some("mimo".to_owned()),
                version: Some("v2.5".to_owned()),
                variant: None,
                release_date: Some("2026-04-22".to_owned()),
                hugging_face_id: Some("XiaomiMiMo/MiMo-V2.5".to_owned()),
            }],
            aliases: ["provider-a", "provider-b"]
                .into_iter()
                .map(|provider| IdentityAliasRecord {
                    source: "models.dev".to_owned(),
                    provider_key: provider.to_owned(),
                    provider_model_id: "XiaomiMiMo/MiMo-V2.5".to_owned(),
                    entity_id: entity_id.to_owned(),
                    confidence: IdentityConfidence::CanonicalReference,
                    provenance_url: "fixture".to_owned(),
                    observed_at: 100,
                })
                .collect(),
        };
        store
            .replace_identity_source(&import)
            .expect("identity snapshot");
        assert_eq!(store.identity_status().expect("status")[0].2, 2);
        let aliases = store.active_identity_aliases().expect("aliases");
        assert_eq!(aliases.len(), 2);
        assert!(
            aliases
                .iter()
                .all(|alias| alias.approved_benchmark_id.is_none())
        );

        store
            .approve_entity_alias("provider-c", "bare-model", entity_id, "fixture")
            .expect("approve alias");
        assert!(
            store
                .active_identity_aliases()
                .expect("operator alias")
                .iter()
                .any(|alias| {
                    alias.source == "operator"
                        && alias.provider_key == "provider-c"
                        && alias.provider_model_id == "bare-model"
                        && alias.entity_id == entity_id
                })
        );

        store
            .approve_benchmark_identity_link(entity_id, "mimo-v2-5-0424", "fixture")
            .expect("approve entity link");
        let aliases = store.active_identity_aliases().expect("linked aliases");
        assert!(
            aliases
                .iter()
                .all(|alias| { alias.approved_benchmark_id.as_deref() == Some("mimo-v2-5-0424") })
        );
        let references = store
            .approved_identity_references()
            .expect("approved references");
        assert_eq!(references.len(), 3);
        assert!(references.iter().any(|reference| {
            reference
                == &(
                    "provider-c".to_owned(),
                    "bare-model".to_owned(),
                    "mimo-v2-5-0424".to_owned(),
                )
        }));
        assert!(
            store
                .remove_benchmark_identity_link(entity_id, "mimo-v2-5-0424")
                .expect("remove entity link")
        );
        assert!(
            store
                .remove_entity_alias("provider-c", "bare-model")
                .expect("remove alias")
        );
        assert!(
            store
                .active_identity_aliases()
                .expect("unlinked aliases")
                .iter()
                .all(|alias| alias.approved_benchmark_id.is_none())
        );

        let invalid = IdentityImport {
            source: "models.dev".to_owned(),
            attribution: "fixture".to_owned(),
            entities: Vec::new(),
            aliases: Vec::new(),
        };
        assert!(store.replace_identity_source(&invalid).is_err());
        assert_eq!(
            store
                .active_identity_aliases()
                .expect("preserved aliases")
                .len(),
            2
        );
    }

    #[test]
    fn target_catalog_price_beats_profile_fallback_including_zero() {
        let store = RoutingStore::open(None).expect("store");
        store
            .replace_catalog(
                "kilocode",
                &[CatalogRecord {
                    model: "mimo-v2-pro".to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: None,
                    supports_tools: None,
                    supports_vision: None,
                    supports_structured_output: None,
                    input_price_per_million: Some(0.0),
                    output_price_per_million: Some(0.0),
                }],
            )
            .expect("catalog");
        store
            .replace_pricing(
                "models.dev",
                PriceSourceKind::ModelsDev,
                "fixture",
                &[profile_price("kilo", "mimo-v2-pro", 1.0, 3.0)],
            )
            .expect("pricing");
        let catalog_price = store
            .effective_price("kilocode", Some("kilo"), "mimo-v2-pro", None, 60)
            .expect("resolve catalog")
            .expect("catalog price");
        assert_eq!(catalog_price.input_price_per_million, 0.0);
        assert_eq!(catalog_price.output_price_per_million, 0.0);
        assert_eq!(catalog_price.source, "catalog:kilocode");
        store
            .replace_pricing(
                "manual-overrides",
                PriceSourceKind::Manual,
                "fixture",
                &[PriceObservation {
                    source: "manual-overrides".to_owned(),
                    source_kind: PriceSourceKind::Manual,
                    scope: PriceScope::RuntimeProvider,
                    provider_key: Some("kilocode".to_owned()),
                    model_id: "mimo-v2-pro".to_owned(),
                    rates: PriceRates {
                        input_price_per_million: Some(2.0),
                        output_price_per_million: Some(4.0),
                        ..PriceRates::default()
                    },
                    fetched_at: Some(1),
                    as_of: None,
                    valid_from: None,
                    valid_until: None,
                    attribution: None,
                }],
            )
            .expect("manual pricing");
        let price = store
            .effective_price("kilocode", Some("kilo"), "mimo-v2-pro", None, 60)
            .expect("resolve")
            .expect("price");
        assert_eq!(price.input_price_per_million, 2.0);
        assert_eq!(price.output_price_per_million, 4.0);
        assert_eq!(price.source, "manual-overrides");
    }

    #[test]
    fn profile_price_fills_missing_target_catalog_price() {
        let store = RoutingStore::open(None).expect("store");
        store
            .replace_pricing(
                "models.dev",
                PriceSourceKind::ModelsDev,
                "fixture",
                &[profile_price("opencode-go", "mimo-v2-pro", 1.0, 3.0)],
            )
            .expect("pricing");
        let price = store
            .effective_price("opencode-go", Some("opencode-go"), "mimo-v2-pro", None, 60)
            .expect("resolve")
            .expect("price");
        assert_eq!(price.input_price_per_million, 1.0);
        assert_eq!(price.output_price_per_million, 3.0);
        assert!(!price.estimated);
    }

    #[test]
    fn cooldown_prevents_a_new_reservation() {
        let store = RoutingStore::open(None).expect("store");
        store
            .apply_cooldown("provider", "model", Some(60))
            .expect("cooldown");
        assert_eq!(
            store
                .reserve("provider", "model", 1, 0, &[])
                .expect("reserve"),
            ReservationOutcome::Cooldown
        );
    }

    #[test]
    fn failed_attempt_can_release_tokens_without_refunding_requests() {
        let store = RoutingStore::open(None).expect("store");
        let quotas = vec![
            QuotaLimit {
                kind: QuotaKind::Requests,
                limit: 2,
                window_seconds: 60,
                boundary: QuotaBoundary::Rolling,
            },
            QuotaLimit {
                kind: QuotaKind::Tokens,
                limit: 100,
                window_seconds: 60,
                boundary: QuotaBoundary::Rolling,
            },
        ];
        let first = match store.reserve("p", "m", 100, 0, &quotas).expect("first") {
            ReservationOutcome::Reserved(token) => token,
            outcome => panic!("expected reservation, got {outcome:?}"),
        };
        store
            .release_reservation(first, super::ReservationRelease::KnownFailure)
            .expect("release tokens");
        assert!(matches!(
            store.reserve("p", "m", 100, 0, &quotas).expect("second"),
            ReservationOutcome::Reserved(_)
        ));
        assert_eq!(
            store.reserve("p", "m", 1, 0, &quotas).expect("third"),
            ReservationOutcome::QuotaExceeded(QuotaKind::Requests)
        );
    }

    #[test]
    fn stale_catalog_entries_are_not_candidates() {
        let store = RoutingStore::open(None).expect("store");
        store
            .replace_catalog("provider", &[catalog("free", true)])
            .expect("catalog");
        store
            .connection
            .lock()
            .expect("connection")
            .execute("UPDATE catalog_models SET refreshed_at = 0", [])
            .expect("age catalog");
        assert!(store.free_candidates(60).expect("candidates").is_empty());
    }

    #[test]
    fn session_hashes_and_pins_persist_in_the_store() {
        let store = RoutingStore::open(None).expect("store");
        let hash = store.session_hash("private session").expect("hash");
        assert!(!hash.contains("private"));
        store
            .set_session_pin(&hash, "auto-free", "provider", "model", 60)
            .expect("pin");
        assert_eq!(
            store.session_pin(&hash, "auto-free").expect("read pin"),
            Some(("provider".to_owned(), "model".to_owned()))
        );
    }

    #[test]
    fn every_provider_profile_has_a_limit_reference() {
        for definition in crate::providers::PROFILE_DEFINITIONS {
            assert!(super::provider_limit_reference(definition.id).is_some());
        }
    }

    #[test]
    fn explicit_zero_price_is_free_even_on_a_paid_account() {
        let provider = ProviderConfig {
            billing_mode: crate::config::BillingMode::Paid,
            ..ProviderConfig::default()
        };
        assert!(super::is_verified_free(&provider, "zero-price", true));
        assert!(!super::is_verified_free(&provider, "unknown-price", false));
    }

    #[test]
    fn invalid_benchmark_refresh_preserves_last_known_good_snapshot() {
        let store = RoutingStore::open(None).expect("store");
        let valid = BenchmarkModel::fixture("valid", 70.0, 70.0, 70.0, 1.0, 1.0);
        store
            .replace_benchmarks("fixture", "Fixture", &[valid])
            .expect("valid snapshot");
        let invalid = BenchmarkModel::fixture("invalid", 101.0, 70.0, 70.0, 1.0, 1.0);
        assert!(
            store
                .replace_benchmarks("fixture", "Fixture", &[invalid])
                .is_err()
        );
        let models = store.benchmark_models(60).expect("active snapshot");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "valid");
    }

    #[test]
    fn cost_reservations_enforce_configured_spend_windows() {
        let store = RoutingStore::open(None).expect("store");
        let quotas = [QuotaLimit {
            kind: QuotaKind::CostMicrousd,
            limit: 100,
            window_seconds: 86_400,
            boundary: QuotaBoundary::Rolling,
        }];
        assert!(matches!(
            store.reserve("p", "m", 1, 60, &quotas).expect("first"),
            ReservationOutcome::Reserved(_)
        ));
        assert_eq!(
            store.reserve("p", "m", 1, 60, &quotas).expect("second"),
            ReservationOutcome::QuotaExceeded(QuotaKind::CostMicrousd)
        );
    }

    #[test]
    fn known_failures_refund_cost_but_not_request_usage() {
        let store = RoutingStore::open(None).expect("store");
        let cost_quota = [QuotaLimit {
            kind: QuotaKind::CostMicrousd,
            limit: 100,
            window_seconds: 86_400,
            boundary: QuotaBoundary::Rolling,
        }];
        let cost_reservation = match store
            .reserve("p", "m", 1, 60, &cost_quota)
            .expect("reserve")
        {
            ReservationOutcome::Reserved(token) => token,
            outcome => panic!("expected reservation, got {outcome:?}"),
        };
        store
            .release_reservation(cost_reservation, super::ReservationRelease::KnownFailure)
            .expect("release cost");
        assert!(matches!(
            store
                .reserve("p", "m", 1, 60, &cost_quota)
                .expect("cost refunded"),
            ReservationOutcome::Reserved(_)
        ));

        let request_store = RoutingStore::open(None).expect("request store");
        let request_quota = [QuotaLimit {
            kind: QuotaKind::Requests,
            limit: 1,
            window_seconds: 86_400,
            boundary: QuotaBoundary::Rolling,
        }];
        let request_reservation = match request_store
            .reserve("p", "m", 1, 0, &request_quota)
            .expect("reserve")
        {
            ReservationOutcome::Reserved(token) => token,
            outcome => panic!("expected reservation, got {outcome:?}"),
        };
        request_store
            .release_reservation(request_reservation, super::ReservationRelease::KnownFailure)
            .expect("release known failure");
        assert_eq!(
            request_store
                .reserve("p", "m", 1, 0, &request_quota)
                .expect("request retained"),
            ReservationOutcome::QuotaExceeded(QuotaKind::Requests)
        );
    }

    #[test]
    fn finalization_reconciles_actual_tokens_and_cost() {
        let store = RoutingStore::open(None).expect("store");
        let quotas = [
            QuotaLimit {
                kind: QuotaKind::Tokens,
                limit: 100,
                window_seconds: 86_400,
                boundary: QuotaBoundary::Rolling,
            },
            QuotaLimit {
                kind: QuotaKind::CostMicrousd,
                limit: 100,
                window_seconds: 86_400,
                boundary: QuotaBoundary::Rolling,
            },
        ];
        let token = match store.reserve("p", "m", 80, 80, &quotas).expect("reserve") {
            ReservationOutcome::Reserved(token) => token,
            outcome => panic!("expected reservation, got {outcome:?}"),
        };
        store
            .finalize_reservation(token, Some(20), Some(20))
            .expect("finalize");
        assert!(matches!(
            store
                .reserve("p", "m", 80, 80, &quotas)
                .expect("reconciled reserve"),
            ReservationOutcome::Reserved(_)
        ));
    }

    #[test]
    fn expired_reservations_release_reserved_dimensions() {
        let store = RoutingStore::open(None).expect("store");
        let quota = [QuotaLimit {
            kind: QuotaKind::Tokens,
            limit: 100,
            window_seconds: 86_400,
            boundary: QuotaBoundary::Rolling,
        }];
        let token = match store.reserve("p", "m", 80, 0, &quota).expect("reserve") {
            ReservationOutcome::Reserved(token) => token,
            outcome => panic!("expected reservation, got {outcome:?}"),
        };
        store
            .connection
            .lock()
            .expect("connection")
            .execute(
                "UPDATE reservations SET expires_at = 0 WHERE id = ?1",
                [token.id],
            )
            .expect("expire reservation");
        assert!(matches!(
            store
                .reserve("p", "m", 80, 0, &quota)
                .expect("expired reserve"),
            ReservationOutcome::Reserved(_)
        ));
    }

    #[test]
    fn concurrency_reservations_release_on_finalization() {
        let store = RoutingStore::open(None).expect("store");
        let quota = [QuotaLimit {
            kind: QuotaKind::Concurrency,
            limit: 1,
            window_seconds: 60,
            boundary: QuotaBoundary::Rolling,
        }];
        let token = match store.reserve("p", "m", 1, 0, &quota).expect("reserve") {
            ReservationOutcome::Reserved(token) => token,
            outcome => panic!("expected reservation, got {outcome:?}"),
        };
        assert_eq!(
            store.reserve("p", "m", 1, 0, &quota).expect("busy reserve"),
            ReservationOutcome::QuotaExceeded(QuotaKind::Concurrency)
        );
        store
            .finalize_reservation(token, None, None)
            .expect("finalize concurrency");
        assert!(matches!(
            store
                .reserve("p", "m", 1, 0, &quota)
                .expect("released reserve"),
            ReservationOutcome::Reserved(_)
        ));
    }

    #[test]
    fn calendar_boundaries_align_to_utc_periods() {
        let week = QuotaLimit {
            kind: QuotaKind::Requests,
            limit: 1,
            window_seconds: 604_800,
            boundary: QuotaBoundary::UtcWeek,
        };
        assert_eq!(super::quota_window_start(0, &week), -259_200);
        let month = QuotaLimit {
            kind: QuotaKind::Requests,
            limit: 1,
            window_seconds: 2_592_000,
            boundary: QuotaBoundary::UtcMonth,
        };
        assert_eq!(super::quota_window_start(0, &month), 0);
    }

    #[test]
    fn account_limit_snapshots_are_persisted_without_credentials() {
        let store = RoutingStore::open(None).expect("store");
        store
            .record_account_limit(
                "openrouter",
                &AccountLimit {
                    limit: Some(10.0),
                    usage: Some(2.0),
                    remaining: Some(8.0),
                    is_free_tier: Some(true),
                },
            )
            .expect("account limit");
        let status = store.account_limit_status().expect("status");
        assert_eq!(status[0].0, "openrouter");
        assert_eq!(status[0].4, Some(8.0));
        assert_eq!(status[0].5, Some(true));
    }

    #[test]
    fn extract_model_family_version_handles_embedded_versions() {
        use super::extract_model_family_version;
        assert_eq!(
            extract_model_family_version("gemma4"),
            Some(("gemma".to_owned(), 4))
        );
        assert_eq!(
            extract_model_family_version("qwen3"),
            Some(("qwen".to_owned(), 3))
        );
    }

    #[test]
    fn extract_model_family_version_handles_separated_versions() {
        use super::extract_model_family_version;
        assert_eq!(
            extract_model_family_version("gemma-3-12b"),
            Some(("gemma".to_owned(), 3))
        );
        assert_eq!(
            extract_model_family_version("phi-3-vision"),
            Some(("phi".to_owned(), 3))
        );
        // With j=1 only, mistral-medium-3-5 picks "medium" as the
        // closest alpha token with a number as the next token
        assert_eq!(
            extract_model_family_version("mistral-medium-3-5"),
            Some(("medium".to_owned(), 3))
        );
        assert_eq!(
            extract_model_family_version("gemini-3-5-flash"),
            Some(("gemini".to_owned(), 3))
        );
        assert_eq!(
            extract_model_family_version("llama-3-1-8b"),
            Some(("llama".to_owned(), 3))
        );
    }

    #[test]
    fn extract_model_family_version_returns_none_for_unversioned() {
        use super::extract_model_family_version;
        assert!(extract_model_family_version("deepseek-r1").is_none());
        assert!(extract_model_family_version("kilo-auto/free").is_none());
        assert!(extract_model_family_version("codestral").is_none());
    }

    #[test]
    fn is_stale_generation_detects_older_versions() {
        use super::is_stale_generation;
        use std::collections::BTreeMap;

        let mut versions = BTreeMap::new();
        versions.insert("gemma".to_owned(), 4u64);
        versions.insert("phi".to_owned(), 4u64);

        assert!(is_stale_generation("google/gemma-3-12b-it", &versions));
        assert!(!is_stale_generation("gemma-4-12b-it", &versions));
        assert!(is_stale_generation(
            "microsoft/phi-3-vision-128k",
            &versions
        ));
        assert!(!is_stale_generation("kilo-auto/free", &versions));
    }
}
