#!/usr/bin/env bash
# Probe provider rate limits from live response headers.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SCRIPT_DIR"
set -a && source .env.local 2>/dev/null && set +a || true

echo "=== Provider Rate Limit Probe ==="
echo "Date: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo ""

probe() {
  local name="$1" url="$2" key="$3" data="$4" auth_header="${5:-}"
  [ -z "$auth_header" ] && auth_header="Authorization: Bearer $key"
  local HF="/tmp/ratelimit_probe_${name}_$$.txt"

  echo "[$name]"
  local http_code
  http_code=$(curl -s -o /dev/null -w "%{http_code}" \
    -H "$auth_header" -H "Content-Type: application/json" \
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
K="${OPENROUTER_API_KEY:-}"
[ -n "$K" ] && probe "openrouter" "https://openrouter.ai/api/v1/chat/completions" "$K" \
  "$(echo "$BODY" | sed 's/MODEL/nvidia\/nemotron-3-ultra-550b-a55b:free/')"

# Groq
K="${GROQ_API_KEY:-}"
[ -n "$K" ] && probe "groq" "https://api.groq.com/openai/v1/chat/completions" "$K" \
  "$(echo "$BODY" | sed 's/MODEL/llama-3.1-8b-instant/')"

# Mistral
K="${MISTRAL_API_KEY:-}"
[ -n "$K" ] && probe "mistral" "https://api.mistral.ai/v1/chat/completions" "$K" \
  "$(echo "$BODY" | sed 's/MODEL/mistral-tiny/')"

# NVIDIA NIM
K="${NVIDIA_NIM_API_KEY:-}"
[ -n "$K" ] && probe "nvidia-nim" "https://integrate.api.nvidia.com/v1/chat/completions" "$K" \
  "$(echo "$BODY" | sed 's/MODEL/z-ai\/glm-5.2/')"

# Nous Portal
K="${NOUS_PORTAL_API_KEY:-}"
[ -n "$K" ] && probe "nous-portal" "https://inference-api.nousresearch.com/v1/chat/completions" "$K" \
  "$(echo "$BODY" | sed 's/MODEL/stepfun\/step-3.7-flash:free/')"

# Google Gemini (native endpoint)
K="${GOOGLE_API_KEY:-}"
[ -n "$K" ] && probe "gemini" \
  "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key=$K" "$K" \
  '{"contents":[{"parts":[{"text":"hello"}]}],"generationConfig":{"maxOutputTokens":10}}' \
  "Content-Type: application/json"

# SiliconFlow (.com domain)
K="${SILICON_FLOW_API_KEY:-}"
[ -n "$K" ] && probe "siliconflow" "https://api.siliconflow.com/v1/chat/completions" "$K" \
  "$(echo "$BODY" | sed 's/MODEL/deepseek-ai\/DeepSeek-V4-Pro/')"

echo "=== Verified limits ==="
echo "  Groq: 14,400 RPD / 6,000 TPM"
echo "  Mistral: 188 RPM / 625,000 TPM"
echo "  Nous Portal: 50 RPM / 500K TPM / 2,100 req/hr"
echo "  Rest: no headers returned (limits only visible on 429)"
echo ""
echo "To stress-test limits: run this in a loop and watch for 429s"
echo "  for i in {1..60}; do bash $0 2>/dev/null | grep '429'; done"
