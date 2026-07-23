#!/usr/bin/env bash
# Verify hardcoded provider rate limits against current documentation.
# Run this periodically (every 2-4 weeks) to check if limits changed.
set -euo pipefail

echo "=== Provider Rate Limit Hardcoded Values ==="
echo "Last verified: $(date '+%Y-%m-%d')"
echo ""
echo "Format: RPM (requests/min), RPD (requests/day), TPM (tokens/min)"
echo ""

cat <<TABLE
| Provider           | RPM | RPD   | TPM      | Scope              | Status              |
|--------------------|----:|------:|---------:|--------------------|---------------------|
| OpenRouter         |  20 |    50 |       —  | account            | published_static    |
| Kilo Code          | 200 |     — |       —  | ip                 | published_static    |
| Groq               |  30 | 14400 |    6,000 | organization       | published_static    |
| Google Gemini Pro  |   5 |   100 |1,000,000 | project_model      | published_static    |
| Google Gemini Flash|  10 |  1500 |1,000,000 | project_model      | published_static    |
| Google Flash-Lite  |  30 |  1500 |1,000,000 | project_model      | published_static    |
| Mistral            |   1 |     — |  500,000 | organization_model | published_partial   |
| Novita             |  60 |     — |       —   | account_model      | published_partial   |
| NVIDIA NIM         |  10 |     — |       —   | account            | dashboard_only      |
| Ollama Cloud       |  30 |     — |       —   | account            | best_effort         |
| Nous Portal        |  10 |     — |       —   | account            | published_partial   |
| SiliconFlow        |1000 |     — |   40,000 | account_model      | published_static    |
| OrcaRouter         |  10 |     — |       —   | account            | account_api         |
| OpenCode           |  50 |     — |   10,000 | account            | best_effort         |
| OpenCode Go        |  —  |     — |       —   | workspace          | published_static    |
| Z.AI (1 req/s)    |  —  |     — |       —   | account_model      | best_effort         |
TABLE

echo ""
echo "=== Reference Documentation URLs ==="
echo ""

declare -A URLS=(
  ["OpenRouter"]="https://openrouter.ai/docs/api/reference/limits"
  ["Kilo Code"]="https://kilo.ai/docs/gateway/usage-and-billing"
  ["Groq"]="https://console.groq.com/docs/rate-limits"
  ["Google Gemini"]="https://ai.google.dev/gemini-api/docs/rate-limits"
  ["OpenCode Go"]="https://opencode.ai/docs/go/"
  ["Z.AI"]="https://docs.z.ai/guides/overview/pricing"
  ["Mistral"]="https://docs.mistral.ai/admin/billing-usage/usage-limits"
  ["Novita"]="https://novita.ai/docs/guides/llm-rate-limits"
  ["NVIDIA NIM"]="https://build.nvidia.com"
  ["Ollama Cloud"]="https://docs.ollama.com/cloud"
  ["Nous Portal"]="https://inference-api.nousresearch.com/v1"
  ["SiliconFlow"]="https://docs.siliconflow.com/en/userguide/rate-limits/rate-limit-and-upgradation"
  ["OrcaRouter"]="https://docs.orcarouter.ai/operations/billing-and-usage"
  ["OpenCode"]="https://opencode.ai/docs/zen/"
)

for provider in "${!URLS[@]}"; do
  echo "  $provider: ${URLS[$provider]}"
done

echo ""
echo "=== Update Procedure ==="
echo "  1. Open each URL and verify the limits match the table"
echo "  2. If a limit changed, update src/routing.rs quota_reference()"
echo "  3. Rerun 'cargo test' after changing"
echo "  4. Update the 'Last verified' date at the top of this file"
echo "  5. Commit both files together"
echo ""
echo "=== Last full verification ==="
echo "  Date: $(date '+%Y-%m-%d')"
echo "  By: verify-provider-limits.sh"
