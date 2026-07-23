#!/usr/bin/env bash
# Probe actual provider rate limits by making real requests.
# Makes ONE request per provider to check response headers and rate limit info.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SCRIPT_DIR"
set -a && source .env.local 2>/dev/null && set +a || true

echo "=== Provider Rate Limit Probe ==="
echo "Date: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo ""

probe() {
  local name="$1" url="$2" api_key="$3" data="$4"
  local HF="/tmp/ratelimit_probe_${name}_$$.txt"

  echo "[$name] $url"

  local http_code
  http_code=$(curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer $api_key" \
    -H "Content-Type: application/json" \
    -d "$data" "$url" -D "$HF" 2>/dev/null || echo "000")
  echo "  HTTP $http_code"

  if [ -f "$HF" ]; then
    local found=false
    while IFS= read -r line; do
      local lc=$(echo "$line" | tr '[:upper:]' '[:lower:]' | tr -d '\r')
      if echo "$lc" | grep -qE 'x-ratelimit|retry-after|x-rate-limit'; then
        echo "  $line" | sed 's/^[[:space:]]*//'
        found=true
      fi
    done < "$HF"
    $found || echo "  (no rate limit headers)"
    rm -f "$HF"
  fi
  echo ""
}

BODY='{"model":"MODEL","messages":[{"role":"user","content":"hello"}],"max_tokens":10}'

# OpenRouter
KEY="${OPENROUTER_API_KEY:-}"
[ -n "$KEY" ] && probe "openrouter" "https://openrouter.ai/api/v1/chat/completions" "$KEY" \
  "$(echo "$BODY" | sed 's/MODEL/openai\/gpt-4o-mini:free/')"

# Groq
KEY="${GROQ_API_KEY:-}"
[ -n "$KEY" ] && probe "groq" "https://api.groq.com/openai/v1/chat/completions" "$KEY" \
  "$(echo "$BODY" | sed 's/MODEL/llama-3.1-8b-instant/')"

# Google Gemini (OpenAI-compatible endpoint)
KEY="${GOOGLE_API_KEY:-}"
[ -n "$KEY" ] && probe "gemini" "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions" "$KEY" \
  "$(echo "$BODY" | sed 's/MODEL/gemini-2.0-flash/')"

# Mistral
KEY="${MISTRAL_API_KEY:-}"
[ -n "$KEY" ] && probe "mistral" "https://api.mistral.ai/v1/chat/completions" "$KEY" \
  "$(echo "$BODY" | sed 's/MODEL/mistral-tiny/')"

# Novita
KEY="${NOVITA_INFRA_KEY:-}"
[ -n "$KEY" ] && probe "novita" "https://api.novita.ai/openai/v1/chat/completions" "$KEY" \
  "$(echo "$BODY" | sed 's/MODEL/meta-llama\/llama-3.2-1b-instruct/')"

# NVIDIA NIM
KEY="${NVIDIA_NIM_API_KEY:-}"
[ -n "$KEY" ] && probe "nvidia-nim" "https://integrate.api.nvidia.com/v1/chat/completions" "$KEY" \
  "$(echo "$BODY" | sed 's/MODEL/meta\/llama-3.3-70b-instruct/')"

# SiliconFlow
KEY="${SILICON_FLOW_KEY:-}"
[ -n "$KEY" ] && probe "siliconflow" "https://api.siliconflow.cn/v1/chat/completions" "$KEY" \
  "$(echo "$BODY" | sed 's/MODEL/Qwen\/Qwen3-8B/')"

# Nous Portal
KEY="${NOUS_PORTAL_API_KEY:-}"
[ -n "$KEY" ] && probe "nous-portal" "https://inference-api.nousresearch.com/v1/chat/completions" "$KEY" \
  "$(echo "$BODY" | sed 's/MODEL/poolside\/laguna-xs-latest/')"

echo "=== Done ==="
echo "Compare observed rate limit headers against: bash scripts/verify-provider-limits.sh"
echo "If you saw 429 errors, those confirm your actual per-minute/day caps."
echo "To stress-test limits safely, run this script in a loop: for i in {1..60}; do bash \$0; done"
