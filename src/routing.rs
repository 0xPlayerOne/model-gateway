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
use crate::providers::AccountLimit;

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
    pub is_free: bool,
    pub context_length: Option<u64>,
    pub supports_tools: Option<bool>,
    pub supports_vision: Option<bool>,
    pub supports_structured_output: Option<bool>,
    pub input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct CatalogRecord {
    pub model: String,
    pub is_free: bool,
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
        ProviderProfileId::Novita,
        "https://novita.ai/docs/api-reference/quota-list",
        "account_api",
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
        "https://opencode.ai/docs/go/",
        "subscription_value_windows",
    ),
    limit(
        ProviderProfileId::Cerebras,
        "https://inference-docs.cerebras.ai/support/rate-limits",
        "published_static",
    ),
    limit(
        ProviderProfileId::Mistral,
        "https://docs.mistral.ai/admin/billing-usage/usage-limits",
        "account_api",
    ),
    limit(
        ProviderProfileId::NousPortal,
        "https://portal.nousresearch.com/",
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
        ProviderProfileId::Cline,
        "https://docs.cline.bot/getting-started/clinepass",
        "account_specific",
    ),
    limit(
        ProviderProfileId::Gitlawb,
        "https://gitlawb.com/opengateway",
        "undocumented",
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
        if version > 3 {
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
               );",
        )?;
        ensure_catalog_columns(&connection)?;
        ensure_benchmark_columns(&connection)?;
        connection.pragma_update(None, "user_version", 3)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn replace_catalog(
        &self,
        provider: &str,
        models: &[CatalogRecord],
    ) -> Result<(), RoutingError> {
        let now = epoch_seconds();
        let mut connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM catalog_models WHERE provider = ?1", [provider])?;
        for model in models {
            transaction.execute(
                "INSERT INTO catalog_models(
                    provider, model, is_free, refreshed_at, context_length,
                     supports_tools, supports_vision, supports_structured_output
                     , input_price_per_million, output_price_per_million
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    provider,
                    model.model,
                    i64::from(model.is_free),
                    now,
                    model.context_length,
                    optional_bool(model.supports_tools),
                    optional_bool(model.supports_vision),
                    optional_bool(model.supports_structured_output),
                    model.input_price_per_million,
                    model.output_price_per_million
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
        is_free: bool,
    ) -> Result<(), RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        connection.execute(
            "INSERT INTO catalog_models(provider, model, is_free, refreshed_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider, model) DO UPDATE SET
                 is_free = excluded.is_free,
                 refreshed_at = excluded.refreshed_at",
            params![provider, model, i64::from(is_free), epoch_seconds()],
        )?;
        Ok(())
    }

    pub fn free_candidates(
        &self,
        max_age_seconds: u64,
    ) -> Result<Vec<CatalogOffering>, RoutingError> {
        let connection = self.connection.lock().map_err(|_| RoutingError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT provider, model, refreshed_at, is_free, context_length,
                    supports_tools, supports_vision, supports_structured_output,
                    input_price_per_million, output_price_per_million
             FROM catalog_models
             WHERE is_free = 1 AND refreshed_at >= ?1
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
                        is_free: row.get::<_, i64>(3)? != 0,
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
            "SELECT provider, model, refreshed_at, is_free, context_length,
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
                        is_free: row.get::<_, i64>(3)? != 0,
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
                    as_of, harness, release_date
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
                    model.harness,
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
                     NULLIF(m.reasoning_effort, ''), m.as_of, m.harness,
                     m.release_date
             FROM benchmark_models m
             JOIN benchmark_snapshots s ON s.id = m.snapshot_id
             WHERE s.active = 1 AND s.fetched_at >= ?1
             ORDER BY m.model_id, s.source",
        )?;
        Ok(statement
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
                        harness: row.get(11)?,
                        release_date: row.get(12)?,
                        raw_metrics: BTreeMap::new(),
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?)
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
                let salt = format!("{:x}", Sha256::digest(seed.as_bytes()));
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
        Ok(format!("{:x}", digest.finalize()))
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

pub fn is_verified_free(provider: &ProviderConfig, model: &str, zero_priced: bool) -> bool {
    if zero_priced || provider.free_models.iter().any(|free| free == model) {
        return true;
    }
    if provider.billing_mode != BillingMode::Free {
        return false;
    }
    match provider.profile {
        Some(ProviderProfileId::OpenRouter) | Some(ProviderProfileId::KiloCode) => {
            model.ends_with(":free")
        }
        Some(ProviderProfileId::GoogleGemini) | Some(ProviderProfileId::Groq) => true,
        Some(ProviderProfileId::Zai) => model.to_ascii_lowercase().contains("flash"),
        _ => false,
    }
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
    let (rules, source_url, scope) = match provider.profile {
        Some(ProviderProfileId::OpenRouter) => (
            vec![requests(20, 60), requests(50, 86_400)],
            "https://openrouter.ai/docs/api/reference/limits",
            "account",
        ),
        Some(ProviderProfileId::KiloCode) => (
            vec![requests(200, 3_600)],
            "https://kilo.ai/docs/gateway/usage-and-billing",
            "ip",
        ),
        Some(ProviderProfileId::Groq) => (
            vec![requests(30, 60), requests(1_000, 86_400), tokens(6_000, 60)],
            "https://console.groq.com/docs/rate-limits",
            "organization_model",
        ),
        Some(ProviderProfileId::Cerebras) => (
            vec![
                requests(5, 60),
                tokens(30_000, 60),
                tokens(1_000_000, 86_400),
            ],
            "https://inference-docs.cerebras.ai/support/rate-limits",
            "organization_model",
        ),
        Some(ProviderProfileId::GoogleGemini) => {
            let lower = model.to_ascii_lowercase();
            let (rpm, rpd) = if lower.contains("pro") {
                (5, 100)
            } else if lower.contains("flash-lite") {
                (15, 1_000)
            } else {
                (10, 250)
            };
            (
                vec![
                    requests(rpm, 60),
                    requests(rpd, 86_400),
                    tokens(250_000, 60),
                ],
                "https://ai.google.dev/gemini-api/docs/rate-limits",
                "project_model",
            )
        }
        Some(ProviderProfileId::Zai) => (
            vec![requests(1, 1)],
            "https://docs.z.ai/guides/overview/pricing",
            "account_model",
        ),
        _ => return None,
    };
    Some(QuotaReference {
        rules,
        source_url,
        as_of: "2026-07-22",
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

fn civil_from_days(days: i64) -> (i64, i64, i64) {
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

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
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
    for (name, sql_type) in [
        ("as_of", "TEXT"),
        ("harness", "TEXT"),
        ("release_date", "TEXT"),
    ] {
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
    use std::fs;
    use std::sync::Arc;

    use crate::config::{ProviderConfig, QuotaBoundary, QuotaKind, QuotaLimit};

    use crate::benchmarks::BenchmarkModel;
    use crate::providers::AccountLimit;

    use super::{CatalogRecord, ReservationOutcome, RoutingStore};

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
        assert!(!super::is_verified_free(&provider, "model", false));
        provider.free_models.push("model".to_owned());
        assert!(super::is_verified_free(&provider, "model", false));
    }

    fn catalog(model: &str, is_free: bool) -> CatalogRecord {
        CatalogRecord {
            model: model.to_owned(),
            is_free,
            context_length: None,
            supports_tools: None,
            supports_vision: None,
            supports_structured_output: None,
            input_price_per_million: None,
            output_price_per_million: None,
        }
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
}
