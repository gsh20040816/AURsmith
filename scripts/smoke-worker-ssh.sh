#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export KVM_GID="${KVM_GID:-996}"
export AURSMITH_SECRET_GID="${AURSMITH_SECRET_GID:-1000}"
export AURSMITH_CONTROLLER_VERIFYING_KEY_HEX="${AURSMITH_CONTROLLER_VERIFYING_KEY_HEX:-0000000000000000000000000000000000000000000000000000000000000000}"
export AURSMITH_CONTROLLER_POLL_URL="${AURSMITH_CONTROLLER_POLL_URL:-https://controller.example.test/api/v1/reverse-workers/poll}"
export AURSMITH_REVERSE_PUBLISHER_ENDPOINT="${AURSMITH_REVERSE_PUBLISHER_ENDPOINT:-ssh://aursmith@192.0.2.20:2223}"

builder_json="$(docker compose -f "${repository_root}/deploy/builder/compose.yaml" config --format json)"
if jq -e '.services | has("ssh")' <<<"${builder_json}" >/dev/null \
  || jq -e '.services.worker.ports[]?' <<<"${builder_json}" >/dev/null; then
  echo "反向 Builder 仍暴露了入站 SSH 或端口" >&2
  exit 1
fi
if [[ "$(jq '[.services.worker.secrets[]? | select(.source == "publisher_push_key" or .source == "publisher_known_hosts")] | length' <<<"${builder_json}")" != "2" ]]; then
  echo "反向 Builder 缺少 Publisher 推送凭据或固定 known_hosts" >&2
  exit 1
fi

echo "反向 Builder 无入站端口检查通过；公网 Worker SSH 冒烟由统一 E2E 覆盖"
