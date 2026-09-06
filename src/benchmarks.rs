use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pricing::fmt_number;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RawBenchmarkMetric {
    pub value: f64,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkModel {
    pub id: String,
    #[serde(default)]
    pub creator: Option<String>,
    #[serde(default)]
    pub intelligence: Option<f64>,
    #[serde(default)]
    pub coding_quality: Option<f64>,
    #[serde(default)]
    pub agentic_quality: Option<f64>,
    #[serde(default)]
    pub input_price_per_million: Option<f64>,
    #[serde(default)]
    pub output_price_per_million: Option<f64>,
    #[serde(default)]
    pub cache_read_price_per_million: Option<f64>,
    #[serde(default)]
    pub cache_write_price_per_million: Option<f64>,
    #[serde(default)]
    pub cost_per_task_usd: Option<f64>,
    #[serde(default)]
    pub latency_seconds: Option<f64>,
    #[serde(default)]
    pub time_to_first_answer_seconds: Option<f64>,
    #[serde(default)]
    pub end_to_end_response_seconds: Option<f64>,
    #[serde(default)]
    pub output_tokens_per_second: Option<f64>,
    #[serde(default)]
    pub output_tokens_per_task: Option<u64>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub raw_metrics: BTreeMap<String, RawBenchmarkMetric>,
}

impl BenchmarkModel {
    pub fn fixture(
        id: &str,
        intelligence: f64,
        coding: f64,
        agentic: f64,
        input_price: f64,
        output_price: f64,
    ) -> Self {
        Self {
            id: id.to_owned(),
            creator: None,
            intelligence: Some(intelligence),
            coding_quality: Some(coding),
            agentic_quality: Some(agentic),
            input_price_per_million: Some(input_price),
            output_price_per_million: Some(output_price),
            cache_read_price_per_million: None,
            cache_write_price_per_million: None,
            cost_per_task_usd: None,
            latency_seconds: Some(1.0),
            time_to_first_answer_seconds: None,
            end_to_end_response_seconds: None,
            output_tokens_per_second: None,
            output_tokens_per_task: Some(1_024),
            reasoning_effort: None,
            as_of: None,
            release_date: None,
            raw_metrics: BTreeMap::new(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.id.len() > 512 {
            return Err("benchmark model ID must be 1-512 characters".to_owned());
        }
        for score in [self.intelligence, self.coding_quality, self.agentic_quality]
            .into_iter()
            .flatten()
        {
            if !score.is_finite() || !(0.0..=100.0).contains(&score) {
                return Err(format!(
                    "benchmark score for '{}' must be between 0 and 100",
                    self.id
                ));
            }
        }
        for value in [
            self.input_price_per_million,
            self.output_price_per_million,
            self.cache_read_price_per_million,
            self.cache_write_price_per_million,
            self.cost_per_task_usd,
            self.latency_seconds,
            self.time_to_first_answer_seconds,
            self.end_to_end_response_seconds,
            self.output_tokens_per_second,
        ]
        .into_iter()
        .flatten()
        {
            if !value.is_finite() || value < 0.0 {
                return Err(format!(
                    "benchmark cost/latency for '{}' must be finite and non-negative",
                    self.id
                ));
            }
        }
        if self
            .output_tokens_per_task
            .is_some_and(|value| value > i64::MAX as u64)
        {
            return Err(format!(
                "benchmark output size for '{}' exceeds the storage limit",
                self.id
            ));
        }
        if self
            .as_of
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > 64)
        {
            return Err(format!("benchmark provenance for '{}' is invalid", self.id));
        }
        for (metric, raw) in &self.raw_metrics {
            if metric.trim().is_empty()
                || !raw.value.is_finite()
                || raw.min.is_some_and(|value| !value.is_finite())
                || raw.max.is_some_and(|value| !value.is_finite())
                || raw.min.zip(raw.max).is_some_and(|(min, max)| max <= min)
            {
                return Err(format!(
                    "raw benchmark metric '{metric}' for '{}' is invalid",
                    self.id
                ));
            }
        }
        Ok(())
    }

    pub fn cost_per_task_microusd(&self) -> Option<u64> {
        self.cost_per_task_usd.map(|cost| {
            if !cost.is_finite() || cost <= 0.0 {
                0
            } else if cost >= u64::MAX as f64 / 1_000_000.0 {
                u64::MAX
            } else {
                (cost * 1_000_000.0).ceil() as u64
            }
        })
    }

    pub fn frontier_latency_seconds(&self) -> Option<f64> {
        self.end_to_end_response_seconds.or(self.latency_seconds)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkImport {
    pub source: String,
    pub attribution: String,
    pub models: Vec<BenchmarkModel>,
}

impl BenchmarkImport {
    pub fn normalize(mut self) -> Result<Self, String> {
        for model in &mut self.models {
            for (metric, raw) in &model.raw_metrics {
                let normalized = match (raw.min, raw.max) {
                    (Some(min), Some(max)) => {
                        ((raw.value - min) / (max - min) * 100.0).clamp(0.0, 100.0)
                    }
                    (None, None) if (0.0..=100.0).contains(&raw.value) => raw.value,
                    _ => {
                        return Err(format!(
                            "raw benchmark metric '{metric}' for '{}' needs a complete comparable min/max range",
                            model.id
                        ));
                    }
                };
                match metric.to_ascii_lowercase().as_str() {
                    "general" | "general_quality" | "intelligence" => {
                        model.intelligence.get_or_insert(normalized);
                    }
                    "coding" | "coding_quality" => {
                        model.coding_quality.get_or_insert(normalized);
                    }
                    "agentic" | "agentic_quality" | "tool_use" => {
                        model.agentic_quality.get_or_insert(normalized);
                    }
                    _ => {
                        return Err(format!(
                            "raw benchmark metric '{metric}' for '{}' has no curated mapping",
                            model.id
                        ));
                    }
                }
            }
            model.validate()?;
        }
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.source.trim().is_empty() || self.source.len() > 128 {
            return Err("benchmark source must be 1-128 characters".to_owned());
        }
        if self.attribution.trim().is_empty() || self.attribution.len() > 1_024 {
            return Err("benchmark attribution must be 1-1024 characters".to_owned());
        }
        if self.models.is_empty() {
            return Err("benchmark import must contain at least one model".to_owned());
        }
        let mut identities = std::collections::BTreeSet::new();
        for model in &self.models {
            model.validate()?;
            let identity = (
                model.id.as_str(),
                model.reasoning_effort.as_deref().unwrap_or(""),
            );
            if !identities.insert(identity) {
                return Err(format!(
                    "benchmark import contains duplicate model/effort '{}'",
                    model.id
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskKind {
    General,
    Coding,
    Agentic,
}

impl TaskKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Coding => "coding",
            Self::Agentic => "agentic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    Simple,
    Medium,
    Complex,
    VeryComplex,
}

impl Complexity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Medium => "medium",
            Self::Complex => "complex",
            Self::VeryComplex => "very_complex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification {
    pub task: TaskKind,
    pub complexity: Complexity,
    pub version: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredCandidate<T> {
    pub value: T,
    pub quality: f64,
    pub expected_cost_microusd: u64,
    pub latency_seconds: f64,
}

pub fn pareto_rank<T>(mut candidates: Vec<ScoredCandidate<T>>) -> Vec<ScoredCandidate<T>> {
    let mut dominated = vec![false; candidates.len()];
    for left in 0..candidates.len() {
        for right in 0..candidates.len() {
            if left == right {
                continue;
            }
            let candidate = &candidates[left];
            let other = &candidates[right];
            let no_worse = other.quality >= candidate.quality
                && other.expected_cost_microusd <= candidate.expected_cost_microusd
                && other.latency_seconds <= candidate.latency_seconds;
            let strictly_better = other.quality > candidate.quality
                || other.expected_cost_microusd < candidate.expected_cost_microusd
                || other.latency_seconds < candidate.latency_seconds;
            if no_worse && strictly_better {
                dominated[left] = true;
                break;
            }
        }
    }
    let mut index = 0usize;
    candidates.retain(|_| {
        let keep = !dominated[index];
        index += 1;
        keep
    });
    candidates.sort_by(|left, right| {
        left.expected_cost_microusd
            .cmp(&right.expected_cost_microusd)
            .then_with(|| left.latency_seconds.total_cmp(&right.latency_seconds))
            .then_with(|| right.quality.total_cmp(&left.quality))
    });
    candidates
}

pub fn classify(request: &Value) -> Classification {
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let text = messages
        .iter()
        .filter_map(|message| message.get("content"))
        .filter_map(|content| match content {
            Value::String(content) => Some(content.clone()),
            other => serde_json::to_string(other).ok(),
        })
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let has_tools = request
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    let coding = contains_any(
        &text,
        &[
            "code",
            "implement",
            "debug",
            "refactor",
            "test",
            "rust",
            "python",
            "typescript",
            "repository",
            "compile",
        ],
    );
    let agentic = has_tools
        || contains_any(
            &text,
            &[
                "multi-step",
                "agent",
                "tool call",
                "edit files",
                "run commands",
            ],
        );
    let task = if agentic {
        TaskKind::Agentic
    } else if coding {
        TaskKind::Coding
    } else {
        TaskKind::General
    };
    let mut complexity = 0u8;
    complexity += u8::from(has_tools) * 2;
    complexity += u8::from(text.len() > 600);
    complexity += u8::from(messages.len() > 4) * 2;
    complexity += u8::from(contains_any(
        &text,
        &[
            "multi-step",
            "comprehensive",
            "concurrency",
            "architecture",
            "production",
            "formal proof",
        ],
    )) * 2;
    complexity += u8::from(request.get("response_format").is_some());
    let complexity = match complexity {
        0 | 1 => Complexity::Simple,
        2 | 3 => Complexity::Medium,
        4 | 5 => Complexity::Complex,
        _ => Complexity::VeryComplex,
    };
    Classification {
        task,
        complexity,
        version: "rules-v1",
    }
}

pub fn quality_for(model: &BenchmarkModel, task: TaskKind) -> Option<f64> {
    match task {
        TaskKind::General => model.intelligence,
        TaskKind::Coding => model.coding_quality.or(model.intelligence),
        TaskKind::Agentic => model.agentic_quality.or(model.intelligence),
    }
}

pub fn composite_quality(model: &BenchmarkModel) -> Option<f64> {
    let intelligence = model.intelligence?;
    let coding = model.coding_quality.unwrap_or(intelligence);
    let agentic = model.agentic_quality.unwrap_or(intelligence);
    Some(0.80 * intelligence + 0.10 * coding + 0.10 * agentic)
}

pub fn parse_artificial_analysis(body: &Value) -> Result<Vec<BenchmarkModel>, String> {
    let items = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "Artificial Analysis response did not contain data".to_owned())?;
    items
        .iter()
        .map(|item| {
            let evaluations = item.get("evaluations").unwrap_or(&Value::Null);
            let pricing = item.get("pricing").unwrap_or(&Value::Null);
            let performance = item.get("performance").unwrap_or(&Value::Null);
            let id = item
                .get("slug")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .ok_or_else(|| "Artificial Analysis model lacked an ID".to_owned())?
                .to_owned();
            let output_tokens_per_task = aa_output_tokens_per_task(performance, &id)?;
            let model = BenchmarkModel {
                id,
                creator: item
                    .get("model_creator")
                    .and_then(|creator| creator.get("name"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                intelligence: scaled_number(
                    evaluations,
                    &[
                        ("artificial_analysis_intelligence_index", 1.0),
                        ("mmlu_pro", 100.0),
                        ("mmlu", 100.0),
                        ("gpqa", 100.0),
                        ("gpqa_diamond", 100.0),
                        ("aime_25", 100.0),
                        ("aime_2025", 100.0),
                        ("math_500", 100.0),
                    ],
                ),
                coding_quality: scaled_number(
                    evaluations,
                    &[
                        ("artificial_analysis_coding_index", 1.0),
                        ("livecodebench", 100.0),
                        ("scicode", 100.0),
                        ("swe_bench_verified", 100.0),
                        ("swe_bench", 100.0),
                    ],
                ),
                agentic_quality: scaled_number(
                    evaluations,
                    &[
                        ("artificial_analysis_agentic_index", 1.0),
                        ("tau2", 100.0),
                        ("terminalbench_v2_1", 100.0),
                        ("terminalbench_hard", 100.0),
                        ("lcr", 100.0),
                        ("tau_banking", 100.0),
                        ("bfcl", 100.0),
                        ("browsecomp", 100.0),
                    ],
                ),
                input_price_per_million: number(pricing, "price_1m_input_tokens"),
                output_price_per_million: number(pricing, "price_1m_output_tokens"),
                cache_read_price_per_million: number(pricing, "price_1m_cache_hit_tokens"),
                cache_write_price_per_million: number(pricing, "price_1m_cache_write_tokens"),
                cost_per_task_usd: item
                    .get("artificial_analysis_intelligence_index_cost")
                    .and_then(|cost| cost.get("cost_per_task"))
                    .and_then(|cost| number(cost, "total_cost")),
                latency_seconds: number(performance, "median_time_to_first_token_seconds")
                    .or_else(|| number(item, "median_time_to_first_token_seconds")),
                time_to_first_answer_seconds: number(
                    performance,
                    "median_time_to_first_answer_token_seconds",
                ),
                end_to_end_response_seconds: number(
                    performance,
                    "median_end_to_end_response_time_seconds",
                ),
                output_tokens_per_second: number(performance, "median_output_tokens_per_second"),
                output_tokens_per_task,
                reasoning_effort: aa_reasoning_effort(item),
                // Provenance comes only from the source payload. Never invent a
                // per-model revision date from the local fetch time: an invented
                // as_of makes unchanged rows look freshly published forever.
                as_of: aa_source_revision(item),
                release_date: item
                    .get("release_date")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                raw_metrics: BTreeMap::new(),
            };
            model.validate()?;
            Ok(model)
        })
        .collect()
}

fn contains_any(text: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| text.contains(term))
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(parse_number)
}

fn scaled_number(value: &Value, keys: &[(&str, f64)]) -> Option<f64> {
    for (key, multiplier) in keys {
        if let Some(n) = value.get(*key).and_then(parse_number) {
            return Some((n * multiplier * 100.0).round() / 100.0);
        }
    }
    None
}

fn parse_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())
}

/// Reads an explicitly published output-size measurement. Do not infer this
/// from latency and throughput: Artificial Analysis documents its end-to-end
/// latency convention separately, and that inference would turn a source
/// convention into a model-specific measurement.
fn aa_output_tokens_per_task(value: &Value, model_id: &str) -> Result<Option<u64>, String> {
    let Some(raw) = value.get("output_tokens_per_task") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let parsed = raw
        .as_u64()
        .or_else(|| {
            let number = raw.as_f64()?;
            (number.is_finite()
                && number >= 0.0
                && number <= i64::MAX as f64
                && number.fract() == 0.0)
                .then_some(number as u64)
        })
        .or_else(|| raw.as_str()?.trim().parse::<u64>().ok())
        .ok_or_else(|| {
            format!(
                "Artificial Analysis output_tokens_per_task for '{model_id}' must be a non-negative integer"
            )
        })?;
    if parsed > i64::MAX as u64 {
        return Err(format!(
            "Artificial Analysis output_tokens_per_task for '{model_id}' exceeds the storage limit"
        ));
    }
    Ok(Some(parsed))
}

/// Reads the source-published revision marker for a benchmark row.
/// Artificial Analysis items may expose `last_updated`, `updated_at`, or an
/// `as_of` date; the first present value is preserved verbatim. Rows without
/// any source revision are left with `as_of: None` (observed-only) so callers
/// can distinguish source-verified revisions from mere fetch observations.
fn aa_source_revision(item: &Value) -> Option<String> {
    ["last_updated", "updated_at", "as_of"]
        .iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn aa_reasoning_effort(item: &Value) -> Option<String> {
    let name = item.get("name").and_then(Value::as_str)?;
    let lower = name.to_ascii_lowercase();
    if lower.contains("non-reasoning") || lower.contains("(non") {
        return None;
    }
    if lower.contains("(xhigh") || lower.ends_with("-xhigh") {
        return Some("xhigh".to_owned());
    }
    if lower.contains("(max") {
        return Some("max".to_owned());
    }
    if lower.contains("(high") || lower.ends_with("-high") {
        return Some("high".to_owned());
    }
    if lower.contains("(medium") || lower.ends_with("-medium") {
        return Some("medium".to_owned());
    }
    if lower.contains("(low") || lower.ends_with("-low") {
        return Some("low".to_owned());
    }
    None
}

/// Deterministic content fingerprint for a benchmark import. Order-insensitive
/// and stable across refreshes so ingestion can skip re-storing a snapshot
/// when the source published no new revision, while any score, price, effort,
/// or provenance change alters the fingerprint.
pub fn fingerprint_benchmark_models(models: &[BenchmarkModel]) -> String {
    let lines = models
        .iter()
        .map(|model| {
            format!(
                "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{:?}",
                model.id,
                model.reasoning_effort.as_deref().unwrap_or(""),
                fmt_score(model.intelligence),
                fmt_score(model.coding_quality),
                fmt_score(model.agentic_quality),
                fmt_number(model.input_price_per_million),
                fmt_number(model.output_price_per_million),
                fmt_number(model.cache_read_price_per_million),
                fmt_number(model.cache_write_price_per_million),
                fmt_number(model.cost_per_task_usd),
                fmt_number(model.latency_seconds),
                fmt_number(model.time_to_first_answer_seconds),
                fmt_number(model.end_to_end_response_seconds),
                fmt_number(model.output_tokens_per_second),
                model
                    .output_tokens_per_task
                    .map_or_else(String::new, |value| value.to_string()),
                model.as_of.as_deref().unwrap_or(""),
                model.release_date.as_deref().unwrap_or(""),
                model.creator.as_deref().unwrap_or(""),
                model.raw_metrics,
            )
        })
        .collect::<Vec<_>>();
    crate::storage::fingerprint_lines(lines)
}

fn fmt_score(value: Option<f64>) -> String {
    fmt_number(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        BenchmarkImport, BenchmarkModel, Complexity, ScoredCandidate, TaskKind, classify,
        composite_quality, pareto_rank, parse_artificial_analysis,
    };

    #[test]
    fn classifier_distinguishes_simple_and_complex_agentic_work() {
        let simple = classify(&json!({"messages": [{"role": "user", "content": "hello"}]}));
        assert_eq!(simple.task, TaskKind::General);
        assert_eq!(simple.complexity, Complexity::Simple);
        let complex = classify(&json!({
            "messages": [{"role": "user", "content": "Implement a comprehensive multi-step concurrency fix"}],
            "tools": [{"type": "function"}]
        }));
        assert_eq!(complex.task, TaskKind::Agentic);
        assert_eq!(complex.complexity, Complexity::Complex);
    }

    #[test]
    fn parses_artificial_analysis_primary_metrics() {
        let models = parse_artificial_analysis(&json!({"data": [{
            "slug": "fixture",
            "model_creator": {"name": "Fixture Labs"},
            "evaluations": {
                "artificial_analysis_intelligence_index": 70.0,
                "artificial_analysis_coding_index": 80.0,
                "artificial_analysis_math_index": 60.0,
                "tau2": 0.55
            },
            "pricing": {"price_1m_input_tokens": 1.0, "price_1m_output_tokens": 2.0},
            "median_time_to_first_token_seconds": 0.5,
            "artificial_analysis_intelligence_index_cost": {
                "cost_per_task": {"total_cost": 0.1678}
            },
            "performance": {
                "median_output_tokens_per_second": 296.47,
                "median_time_to_first_answer_token_seconds": 7.4,
                "median_end_to_end_response_time_seconds": 9.09,
                "output_tokens_per_task": 1024
            }
        }]}))
        .expect("Artificial Analysis fixture");
        assert_eq!(models[0].id, "fixture");
        assert_eq!(models[0].coding_quality, Some(80.0));
        assert_eq!(models[0].agentic_quality, Some(55.0));
        assert_eq!(models[0].cost_per_task_microusd(), Some(167_800));
        assert_eq!(models[0].frontier_latency_seconds(), Some(9.09));
        assert_eq!(models[0].output_tokens_per_second, Some(296.47));
        assert_eq!(models[0].output_tokens_per_task, Some(1024));
    }

    #[test]
    fn aa_parser_keeps_missing_response_size_unobserved() {
        let models = parse_artificial_analysis(&json!({"data": [{
            "slug": "without-response-size",
            "performance": {
                "median_output_tokens_per_second": 296.47,
                "median_end_to_end_response_time_seconds": 9.09
            }
        }]}))
        .expect("Artificial Analysis fixture");
        assert_eq!(models[0].output_tokens_per_task, None);
    }

    #[test]
    fn aa_parser_rejects_invalid_response_size_measurements() {
        for value in [json!(-1), json!(1.5), json!("not-a-count")] {
            assert!(
                parse_artificial_analysis(&json!({"data": [{
                    "slug": "invalid-response-size",
                    "performance": {"output_tokens_per_task": value}
                }]}))
                .is_err(),
                "invalid output size should fail closed: {value}"
            );
        }
    }

    #[test]
    fn rejects_response_sizes_that_cannot_fit_storage() {
        let mut model = BenchmarkModel::fixture("oversized-response", 50.0, 50.0, 50.0, 1.0, 1.0);
        model.output_tokens_per_task = Some(i64::MAX as u64 + 1);
        assert!(model.validate().is_err());
    }

    #[test]
    fn aa_fallback_scales_zero_to_one_benchmarks_to_zero_to_one_hundred() {
        let models = parse_artificial_analysis(&json!({"data": [{
            "slug": "fallback-model",
            "model_creator": {"name": "Test Labs"},
            "evaluations": {
                "livecodebench": 0.75,
                "aime_25": 0.623,
                "tau2": 0.45,
                "gpqa": 0.78
            },
            "pricing": {"price_1m_input_tokens": 1.0, "price_1m_output_tokens": 2.0},
            "median_time_to_first_token_seconds": 0.3
        }]}))
        .expect("fallback fixture");
        assert_eq!(models[0].coding_quality, Some(75.0));
        assert_eq!(models[0].agentic_quality, Some(45.0));
        assert_eq!(models[0].intelligence, Some(78.0));
    }

    #[test]
    fn aa_parser_accepts_string_scores_and_expanded_metric_fallbacks() {
        let models = parse_artificial_analysis(&json!({"data": [{
            "slug": "string-values",
            "evaluations": {
                "swe_bench_verified": "0.81",
                "aime_2025": "0.72",
                "browsecomp": "0.64"
            },
            "pricing": {
                "price_1m_input_tokens": "1.25",
                "price_1m_output_tokens": "4.5",
                "price_1m_cache_hit_tokens": "0.12",
                "price_1m_cache_write_tokens": "0.6"
            },
            "performance": {"median_time_to_first_token_seconds": "0.4"}
        }]}))
        .expect("string-valued Artificial Analysis fixture");
        assert_eq!(models[0].coding_quality, Some(81.0));
        assert_eq!(models[0].intelligence, Some(72.0));
        assert_eq!(models[0].agentic_quality, Some(64.0));
        assert_eq!(models[0].input_price_per_million, Some(1.25));
        assert_eq!(models[0].latency_seconds, Some(0.4));
        assert_eq!(models[0].cache_read_price_per_million, Some(0.12));
        assert_eq!(models[0].cache_write_price_per_million, Some(0.6));
    }

    #[test]
    fn aa_parser_rejects_missing_ids_and_invalid_shapes() {
        assert!(parse_artificial_analysis(&json!({"data": [{"evaluations": {}}]})).is_err());
        assert!(
            parse_artificial_analysis(&json!({"data": [{
                "slug": "invalid-score",
                "evaluations": {"gpqa": 101.0}
            }]}))
            .is_err()
        );
        assert!(parse_artificial_analysis(&json!({"data": {}})).is_err());
    }

    #[test]
    fn rejects_empty_and_duplicate_imports() {
        let empty = BenchmarkImport {
            source: "fixture".to_owned(),
            attribution: "Fixture data".to_owned(),
            models: Vec::new(),
        };
        assert!(empty.validate().is_err());
        let model = BenchmarkModel::fixture("same", 50.0, 50.0, 50.0, 1.0, 1.0);
        let duplicate = BenchmarkImport {
            source: "fixture".to_owned(),
            attribution: "Fixture data".to_owned(),
            models: vec![model.clone(), model],
        };
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn normalizes_raw_metrics_only_with_explicit_comparable_ranges() {
        let mut model = BenchmarkModel::fixture("raw", 0.0, 0.0, 0.0, 0.0, 0.0);
        model.intelligence = None;
        model.raw_metrics.insert(
            "general".to_owned(),
            super::RawBenchmarkMetric {
                value: 50.0,
                min: Some(0.0),
                max: Some(100.0),
            },
        );
        let import = BenchmarkImport {
            source: "fixture".to_owned(),
            attribution: "Fixture data".to_owned(),
            models: vec![model],
        };
        let normalized = import.normalize().expect("normalize");
        assert_eq!(normalized.models[0].intelligence, Some(50.0));

        let mut incomparable = BenchmarkModel::fixture("bad", 0.0, 0.0, 0.0, 0.0, 0.0);
        incomparable.raw_metrics.insert(
            "general".to_owned(),
            super::RawBenchmarkMetric {
                value: 500.0,
                min: None,
                max: None,
            },
        );
        let import = BenchmarkImport {
            source: "fixture".to_owned(),
            attribution: "Fixture data".to_owned(),
            models: vec![incomparable],
        };
        assert!(import.normalize().is_err());
    }

    #[test]
    fn aa_parser_preserves_source_revision_or_none() {
        let models = parse_artificial_analysis(&json!({"data": [
            {
                "slug": "with-last-updated",
                "evaluations": {"gpqa": 0.8},
                "last_updated": "2026-07-09"
            },
            {
                "slug": "with-updated-at",
                "evaluations": {"gpqa": 0.8},
                "updated_at": "2026-07-15"
            },
            {
                "slug": "observed-only",
                "evaluations": {"gpqa": 0.8}
            }
        ]}))
        .expect("Artificial Analysis fixture");
        assert_eq!(models[0].as_of.as_deref(), Some("2026-07-09"));
        assert_eq!(models[1].as_of.as_deref(), Some("2026-07-15"));
        // Never invent a per-model revision from the fetch date.
        assert_eq!(models[2].as_of, None);
        assert!(models[0].validate().is_ok());
        assert!(models[2].validate().is_ok());
    }

    #[test]
    fn benchmark_fingerprint_is_stable_and_revision_sensitive() {
        use super::fingerprint_benchmark_models;
        let mut model = BenchmarkModel::fixture("gpt-5-6-luna", 40.0, 50.0, 45.0, 0.2, 1.2);
        model.reasoning_effort = Some("high".to_owned());
        model.as_of = Some("2026-07-09".to_owned());
        let original = fingerprint_benchmark_models(&[model.clone()]);
        assert_eq!(
            original,
            fingerprint_benchmark_models(&[model.clone()]),
            "unchanged rows must keep the fingerprint"
        );
        let mut reordered = vec![model.clone()];
        reordered.push(BenchmarkModel::fixture(
            "gpt-5-6-sol",
            90.0,
            90.0,
            90.0,
            5.0,
            30.0,
        ));
        let forward = fingerprint_benchmark_models(&reordered);
        reordered.reverse();
        assert_eq!(forward, fingerprint_benchmark_models(&reordered));
        let mut revised = model.clone();
        revised.intelligence = Some(51.2);
        assert_ne!(original, fingerprint_benchmark_models(&[revised]));
        let mut cache_revised = model;
        cache_revised.cache_read_price_per_million = Some(0.1);
        assert_ne!(
            original,
            fingerprint_benchmark_models(&[cache_revised]),
            "cache pricing revisions must change the fingerprint"
        );
    }

    #[test]
    fn aa_max_maps_to_max_not_high() {
        use serde_json::json;

        let max = json!({"name": "GPT-5.6 Terra (max)"});
        assert_eq!(super::aa_reasoning_effort(&max), Some("max".to_owned()));

        let high = json!({"name": "GPT-5.6 Sol (high)"});
        assert_eq!(super::aa_reasoning_effort(&high), Some("high".to_owned()));

        let xhigh = json!({"name": "GPT-5.6 Sol (xhigh)"});
        assert_eq!(super::aa_reasoning_effort(&xhigh), Some("xhigh".to_owned()));

        let low = json!({"name": "GPT-5.6 Sol (low)"});
        assert_eq!(super::aa_reasoning_effort(&low), Some("low".to_owned()));

        let none = json!({"name": "GPT-5.6 Terra (Non-reasoning)"});
        assert_eq!(super::aa_reasoning_effort(&none), None);
    }

    #[test]
    fn pareto_rank_removes_dominated_candidates_and_prefers_cost() {
        let ranked = pareto_rank(vec![
            ScoredCandidate {
                value: "dominated",
                quality: 50.0,
                expected_cost_microusd: 20,
                latency_seconds: 2.0,
            },
            ScoredCandidate {
                value: "cheap",
                quality: 60.0,
                expected_cost_microusd: 10,
                latency_seconds: 1.0,
            },
            ScoredCandidate {
                value: "strong",
                quality: 90.0,
                expected_cost_microusd: 30,
                latency_seconds: 1.0,
            },
        ]);
        assert_eq!(
            ranked
                .into_iter()
                .map(|candidate| candidate.value)
                .collect::<Vec<_>>(),
            vec!["cheap", "strong"]
        );
    }

    #[test]
    fn pareto_rank_keeps_tradeoffs_and_removes_only_strictly_dominated_candidates() {
        let ranked = pareto_rank(vec![
            ScoredCandidate {
                value: "equal",
                quality: 70.0,
                expected_cost_microusd: 10,
                latency_seconds: 1.0,
            },
            ScoredCandidate {
                value: "same-frontier-point",
                quality: 70.0,
                expected_cost_microusd: 10,
                latency_seconds: 1.0,
            },
            ScoredCandidate {
                value: "quality-tradeoff",
                quality: 90.0,
                expected_cost_microusd: 20,
                latency_seconds: 2.0,
            },
            ScoredCandidate {
                value: "speed-tradeoff",
                quality: 60.0,
                expected_cost_microusd: 5,
                latency_seconds: 0.5,
            },
            ScoredCandidate {
                value: "dominated",
                quality: 65.0,
                expected_cost_microusd: 15,
                latency_seconds: 1.5,
            },
        ]);

        assert_eq!(
            ranked
                .into_iter()
                .map(|candidate| candidate.value)
                .collect::<Vec<_>>(),
            vec![
                "speed-tradeoff",
                "equal",
                "same-frontier-point",
                "quality-tradeoff"
            ]
        );
    }

    #[test]
    fn benchmark_efficiency_metrics_convert_and_fallback_without_fuzzy_behavior() {
        let mut benchmark = BenchmarkModel::fixture("measured", 80.0, 80.0, 80.0, 1.0, 2.0);
        benchmark.cost_per_task_usd = Some(0.1234567);
        benchmark.latency_seconds = Some(0.8);
        assert_eq!(benchmark.cost_per_task_microusd(), Some(123_457));
        assert_eq!(benchmark.frontier_latency_seconds(), Some(0.8));

        benchmark.end_to_end_response_seconds = Some(12.5);
        assert_eq!(benchmark.frontier_latency_seconds(), Some(12.5));
    }

    #[test]
    fn classifier_covers_coding_reasoning_and_followups() {
        assert_eq!(
            classify(&json!({"messages": [{"role": "user", "content": "debug this Rust test"}]}))
                .task,
            TaskKind::Coding
        );
        assert_eq!(
            classify(&json!({"messages": [{"role": "user", "content": "derive this equation"}]}))
                .task,
            TaskKind::General
        );
        assert_eq!(
            classify(&json!({"messages": [
                {"role": "user", "content": "question"},
                {"role": "assistant", "content": "answer"},
                {"role": "user", "content": "follow up"},
                {"role": "assistant", "content": "answer"},
                {"role": "user", "content": "one more"}
            ]}))
            .complexity,
            Complexity::Medium
        );
    }

    #[test]
    fn composite_quality_uses_weighted_average() {
        let model = BenchmarkModel::fixture("test", 80.0, 70.0, 60.0, 0.0, 0.0);
        let quality = composite_quality(&model).expect("quality");
        assert!((quality - (0.80 * 80.0 + 0.10 * 70.0 + 0.10 * 60.0)).abs() < 0.01);
    }

    #[test]
    fn composite_quality_falls_back_to_intelligence() {
        let mut model = BenchmarkModel::fixture("test", 80.0, 0.0, 0.0, 0.0, 0.0);
        model.coding_quality = None;
        model.agentic_quality = None;
        let quality = composite_quality(&model).expect("quality");
        assert!((quality - 80.0).abs() < 0.01);
    }

    #[test]
    fn composite_quality_returns_none_without_intelligence() {
        let mut model = BenchmarkModel::fixture("test", 0.0, 0.0, 0.0, 0.0, 0.0);
        model.intelligence = None;
        assert!(composite_quality(&model).is_none());
    }

    #[test]
    fn composite_quality_redistributes_partial_fallback() {
        let mut model = BenchmarkModel::fixture("test", 80.0, 70.0, 0.0, 0.0, 0.0);
        model.agentic_quality = None;
        let quality = composite_quality(&model).expect("quality");
        let expected = 0.80 * 80.0 + 0.10 * 70.0 + 0.10 * 80.0;
        assert!((quality - expected).abs() < 0.01);
    }
}
