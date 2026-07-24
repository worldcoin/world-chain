#!/usr/bin/env bash
set -euo pipefail

# Upserts the Status / Release-RC-Tag / Last-Verified fields of one row in the
# Notion "Release Deployment Tracker" database, matched by its Environment
# (title) property. Intentionally minimal: it only ever writes an environment
# name, a tag, a status, and a timestamp — never cluster contexts, account IDs,
# or other infra identifiers, even though those already exist as pre-populated
# fields on the tracker rows themselves (this script does not touch them).
#
# Requires: NOTION_TOKEN, NOTION_DATABASE_ID (env vars). Fails loudly (no
# silent no-op) if the target row can't be found unambiguously, or on any API
# error — an out-of-date tracker is worse than a visibly failed workflow step.

usage() {
  echo "Usage: NOTION_TOKEN=... NOTION_DATABASE_ID=... $0 --environment <name> --status <status> [--tag <tag>] [--mark-verified]" >&2
  exit 1
}

ENVIRONMENT=""
STATUS=""
TAG=""
MARK_VERIFIED=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --environment) ENVIRONMENT="$2"; shift 2 ;;
    --status) STATUS="$2"; shift 2 ;;
    --tag) TAG="$2"; shift 2 ;;
    --mark-verified) MARK_VERIFIED=true; shift ;;
    *) usage ;;
  esac
done

[[ -n "${ENVIRONMENT}" && -n "${STATUS}" ]] || usage
: "${NOTION_TOKEN:?NOTION_TOKEN is required}"
: "${NOTION_DATABASE_ID:?NOTION_DATABASE_ID is required}"

case "${STATUS}" in
  Deployed|"Rolling out"|Soaking|Reverted|Unknown) ;;
  *) echo "::error::Unknown status '${STATUS}'. Must be one of: Deployed, Rolling out, Soaking, Reverted, Unknown." >&2; exit 1 ;;
esac

for cmd in curl jq; do
  command -v "${cmd}" >/dev/null 2>&1 || { echo "${cmd} is required." >&2; exit 1; }
done

NOTION_API="https://api.notion.com/v1"
NOTION_VERSION="2022-06-28"

api() {
  local method="$1" path="$2" body="${3:-}"
  local args=(-sS -X "${method}" "${NOTION_API}${path}" \
    -H "Authorization: Bearer ${NOTION_TOKEN}" \
    -H "Notion-Version: ${NOTION_VERSION}" \
    -H "Content-Type: application/json")
  if [[ -n "${body}" ]]; then
    args+=(-d "${body}")
  fi
  curl "${args[@]}"
}

query_body="$(jq -n --arg env "${ENVIRONMENT}" '{filter: {property: "Environment", title: {equals: $env}}}')"
query_response="$(api POST "/databases/${NOTION_DATABASE_ID}/query" "${query_body}")"

if jq -e '.object == "error"' <<<"${query_response}" >/dev/null; then
  echo "::error::Notion query failed: $(jq -r '.message' <<<"${query_response}")" >&2
  exit 1
fi

match_count="$(jq '.results | length' <<<"${query_response}")"
if [[ "${match_count}" -eq 0 ]]; then
  echo "::error::No Release Deployment Tracker row found with Environment = '${ENVIRONMENT}'. Refusing to guess — create the row in Notion first." >&2
  exit 1
elif [[ "${match_count}" -gt 1 ]]; then
  echo "::error::${match_count} rows matched Environment = '${ENVIRONMENT}'; expected exactly one. Fix the duplicate in Notion before automation can update it." >&2
  exit 1
fi

page_id="$(jq -r '.results[0].id' <<<"${query_response}")"

properties="$(jq -n --arg status "${STATUS}" '{"Status": {"select": {"name": $status}}}')"

if [[ -n "${TAG}" ]]; then
  properties="$(jq --arg tag "${TAG}" '. + {"Release / RC Tag": {"rich_text": [{"text": {"content": $tag}}]}}' <<<"${properties}")"
fi

if [[ "${MARK_VERIFIED}" == "true" ]]; then
  if [[ -z "${LAST_VERIFIED_DATE:-}" ]]; then
    echo "::error::--mark-verified requires LAST_VERIFIED_DATE (YYYY-MM-DD) to be set — workflow scripts must not call date(1) themselves (breaks reproducibility/resume)." >&2
    exit 1
  fi
  properties="$(jq --arg d "${LAST_VERIFIED_DATE}" '. + {"Last Verified": {"date": {"start": $d}}}' <<<"${properties}")"
fi

update_body="$(jq -n --argjson props "${properties}" '{properties: $props}')"
update_response="$(api PATCH "/pages/${page_id}" "${update_body}")"

if jq -e '.object == "error"' <<<"${update_response}" >/dev/null; then
  echo "::error::Notion update failed: $(jq -r '.message' <<<"${update_response}")" >&2
  exit 1
fi

echo "Updated Notion row for '${ENVIRONMENT}': Status=${STATUS}${TAG:+, Tag=${TAG}}"
