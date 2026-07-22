use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub general_quality: Option<f64>,
    #[serde(default)]
    pub coding_quality: Option<f64>,
    #[serde(default)]
    pub agentic_quality: Option<f64>,
    #[serde(default)]
    pub reasoning_quality: Option<f64>,
    #[serde(default)]
    pub input_price_per_million: Option<f64>,
    #[serde(default)]
    pub output_price_per_million: Option<f64>,
    #[serde(default)]
    pub latency_seconds: Option<f64>,
    #[serde(default)]
    pub output_tokens_per_task: Option<u64>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub raw_metrics: BTreeMap<String, RawBenchmarkMetric>,
}

impl BenchmarkModel {
    pub fn fixture(
        id: &str,
        general: f64,
        coding: f64,
        agentic: f64,
        reasoning: f64,
        input_price: f64,
        output_price: f64,
    ) -> Self {
        Self {
            id: id.to_owned(),
            creator: None,
            general_quality: Some(general),
            coding_quality: Some(coding),
            agentic_quality: Some(agentic),
            reasoning_quality: Some(reasoning),
            input_price_per_million: Some(input_price),
            output_price_per_million: Some(output_price),
            latency_seconds: Some(1.0),
            output_tokens_per_task: Some(1_024),
            reasoning_effort: None,
            as_of: None,
            harness: Some("fixture".to_owned()),
            confidence: Some(1.0),
            raw_metrics: BTreeMap::new(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.id.len() > 512 {
            return Err("benchmark model ID must be 1-512 characters".to_owned());
        }
        for score in [
            self.general_quality,
            self.coding_quality,
            self.agentic_quality,
            self.reasoning_quality,
        ]
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
            self.latency_seconds,
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
            .as_of
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > 64)
            || self
                .harness
                .as_ref()
                .is_some_and(|value| value.trim().is_empty() || value.len() > 128)
        {
            return Err(format!("benchmark provenance for '{}' is invalid", self.id));
        }
        if self
            .confidence
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(format!(
                "benchmark confidence for '{}' must be between 0 and 1",
                self.id
            ));
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
                        model.general_quality.get_or_insert(normalized);
                    }
                    "coding" | "coding_quality" => {
                        model.coding_quality.get_or_insert(normalized);
                    }
                    "agentic" | "agentic_quality" | "tool_use" => {
                        model.agentic_quality.get_or_insert(normalized);
                    }
                    "reasoning" | "reasoning_quality" | "math" => {
                        model.reasoning_quality.get_or_insert(normalized);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    General,
    Coding,
    Agentic,
    Reasoning,
}

impl TaskKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Coding => "coding",
            Self::Agentic => "agentic",
            Self::Reasoning => "reasoning",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Complexity {
    Simple,
    Medium,
    Complex,
}

impl Complexity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Medium => "medium",
            Self::Complex => "complex",
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
    let reasoning = contains_any(
        &text,
        &[
            "prove",
            "equation",
            "mathemat",
            "derive",
            "logic",
            "reason step",
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
    } else if reasoning {
        TaskKind::Reasoning
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
        _ => Complexity::Complex,
    };
    Classification {
        task,
        complexity,
        version: "rules-v1",
    }
}

pub fn quality_for(model: &BenchmarkModel, task: TaskKind) -> Option<f64> {
    match task {
        TaskKind::General => model.general_quality,
        TaskKind::Coding => model.coding_quality.or(model.general_quality),
        TaskKind::Agentic => model.agentic_quality.or(model.coding_quality),
        TaskKind::Reasoning => model.reasoning_quality.or(model.general_quality),
    }
}

pub fn is_frontier_model(creator: Option<&str>, canonical_model: &str) -> bool {
    let Some(creator) = creator.map(str::trim) else {
        return false;
    };
    let model = canonical_model
        .rsplit('/')
        .next()
        .unwrap_or(canonical_model)
        .to_ascii_lowercase();
    if creator.eq_ignore_ascii_case("anthropic") {
        return model == "claude" || model.starts_with("claude-");
    }
    if !creator.eq_ignore_ascii_case("openai") {
        return false;
    }
    if model == "gpt" || model.starts_with("gpt-") {
        return true;
    }
    let mut characters = model.chars();
    characters.next() == Some('o')
        && characters
            .next()
            .is_some_and(|value| value.is_ascii_digit())
        && characters
            .next()
            .is_none_or(|value| matches!(value, '-' | '_' | '.'))
}

pub fn is_preview_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    ["preview", "beta", "experimental"].iter().any(|marker| {
        model
            .split(['/', '-', ':', '_'])
            .any(|part| part == *marker)
    })
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
            let model = BenchmarkModel {
                id: item
                    .get("slug")
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Artificial Analysis model lacked an ID".to_owned())?
                    .to_owned(),
                creator: item
                    .get("model_creator")
                    .and_then(|creator| creator.get("name"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                general_quality: number(evaluations, "artificial_analysis_intelligence_index"),
                coding_quality: number(evaluations, "artificial_analysis_coding_index"),
                agentic_quality: None,
                reasoning_quality: number(evaluations, "artificial_analysis_math_index"),
                input_price_per_million: number(pricing, "price_1m_input_tokens"),
                output_price_per_million: number(pricing, "price_1m_output_tokens"),
                latency_seconds: number(item, "median_time_to_first_token_seconds"),
                output_tokens_per_task: None,
                reasoning_effort: None,
                as_of: item
                    .get("as_of")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                harness: item
                    .get("harness")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                confidence: item.get("confidence").and_then(Value::as_f64),
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
    value.get(key).and_then(Value::as_f64)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        BenchmarkImport, BenchmarkModel, Complexity, ScoredCandidate, TaskKind, classify,
        is_frontier_model, is_preview_model, pareto_rank, parse_artificial_analysis,
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
                "artificial_analysis_math_index": 60.0
            },
            "pricing": {"price_1m_input_tokens": 1.0, "price_1m_output_tokens": 2.0},
            "median_time_to_first_token_seconds": 0.5
        }]}))
        .expect("Artificial Analysis fixture");
        assert_eq!(models[0].id, "fixture");
        assert_eq!(models[0].coding_quality, Some(80.0));
    }

    #[test]
    fn rejects_empty_and_duplicate_imports() {
        let empty = BenchmarkImport {
            source: "fixture".to_owned(),
            attribution: "Fixture data".to_owned(),
            models: Vec::new(),
        };
        assert!(empty.validate().is_err());
        let model = BenchmarkModel::fixture("same", 50.0, 50.0, 50.0, 50.0, 1.0, 1.0);
        let duplicate = BenchmarkImport {
            source: "fixture".to_owned(),
            attribution: "Fixture data".to_owned(),
            models: vec![model.clone(), model],
        };
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn normalizes_raw_metrics_only_with_explicit_comparable_ranges() {
        let mut model = BenchmarkModel::fixture("raw", 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        model.general_quality = None;
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
        assert_eq!(normalized.models[0].general_quality, Some(50.0));

        let mut incomparable = BenchmarkModel::fixture("bad", 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
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
    fn classifier_covers_coding_reasoning_and_followups() {
        assert_eq!(
            classify(&json!({"messages": [{"role": "user", "content": "debug this Rust test"}]}))
                .task,
            TaskKind::Coding
        );
        assert_eq!(
            classify(&json!({"messages": [{"role": "user", "content": "derive this equation"}]}))
                .task,
            TaskKind::Reasoning
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
    fn frontier_family_requires_canonical_gpt_reasoning_or_claude_identity() {
        assert!(is_frontier_model(Some("OpenAI"), "gpt-5"));
        assert!(is_frontier_model(Some("OpenAI"), "openai/o3-mini"));
        assert!(is_frontier_model(Some("Anthropic"), "claude-sonnet-4-6"));
        assert!(!is_frontier_model(Some("OpenAI"), "text-embedding-3"));
        assert!(!is_frontier_model(Some("Anthropic"), "not-claude"));
        assert!(!is_frontier_model(Some("Other"), "gpt-5"));
    }

    #[test]
    fn preview_detection_uses_model_id_segments() {
        assert!(is_preview_model("openai/gpt-5-preview"));
        assert!(is_preview_model("claude_beta"));
        assert!(!is_preview_model("previewlabs/model"));
        assert!(!is_preview_model("openai/gpt-5"));
    }
}
