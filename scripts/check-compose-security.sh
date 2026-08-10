#!/usr/bin/env bash
set -euo pipefail

# 仅给 Compose 渲染提供无权限测试值，不连接真实 Worker。
export KVM_GID="${KVM_GID:-996}"
export AURSMITH_CONTROLLER_VERIFYING_KEY_HEX="${AURSMITH_CONTROLLER_VERIFYING_KEY_HEX:-0000000000000000000000000000000000000000000000000000000000000000}"
export AURSMITH_FETCH_PROXY="${AURSMITH_FETCH_PROXY:-192.0.2.10:8080}"
export AURSMITH_TRANSFER_ENDPOINTS_JSON="${AURSMITH_TRANSFER_ENDPOINTS_JSON:-{\"00000000-0000-0000-0000-000000000001\":\"ssh://aursmith@192.0.2.10:2222\"}}"

for stack in controller builder publisher archiver; do
  json="$(docker compose -f "deploy/${stack}/compose.yaml" config --format json)"
  if jq -e '.services[] | select(.privileged == true)' <<<"${json}" >/dev/null; then
    echo "${stack}: 禁止 privileged" >&2
    exit 1
  fi
  if jq -e '[.services[].volumes[]? | .source // ""] | any(test("docker[.]sock|libvirt"))' <<<"${json}" >/dev/null; then
    echo "${stack}: 禁止 Docker/libvirt Socket" >&2
    exit 1
  fi
  if jq -e '.services[] | select((.cap_drop // []) | index("ALL") | not)' <<<"${json}" >/dev/null; then
    echo "${stack}: 所有服务必须 cap_drop ALL" >&2
    exit 1
  fi
  if jq -e '.services[] | select(((.cap_add // []) - ["CHOWN", "DAC_READ_SEARCH", "SETGID", "SETPCAP", "SETUID"]) | length > 0)' <<<"${json}" >/dev/null; then
    echo "${stack}: 只允许 SSH 降权启动器使用 secret 读取及永久降权所需能力" >&2
    exit 1
  fi
  if jq -e '.services[] | select((.cap_add // []) | length > 0) | select(.entrypoint[0] != "/usr/local/bin/aursmithctl" or .command[0] != "run-sshd")' <<<"${json}" >/dev/null; then
    echo "${stack}: 附加能力只能授予 SSH 降权启动器" >&2
    exit 1
  fi
done

while IFS= read -r from_line; do
  if [[ "${from_line}" != *"@sha256:"* ]]; then
    echo "Dockerfile 基础镜像必须固定 digest: ${from_line}" >&2
    exit 1
  fi
done < <(rg '^FROM ' deploy/images)

builder_json="$(docker compose -f deploy/builder/compose.yaml config --format json)"
if [[ "$(jq '[.services.worker.devices[]? | select(.source == "/dev/kvm")] | length' <<<"${builder_json}")" != "1" ]]; then
  echo "Builder 必须且只能显式获得 /dev/kvm 构建设备" >&2
  exit 1
fi

publisher_json="$(docker compose -f deploy/publisher/compose.yaml config --format json)"
if [[ "$(jq '[.services.signer | select(.network_mode == "none")] | length' <<<"${publisher_json}")" != "1" ]]; then
  echo "Signer 必须完全断网" >&2
  exit 1
fi
if jq -e '.services.signer.volumes[]? | select(.target == "/repository")' <<<"${publisher_json}" >/dev/null; then
  echo "Signer 禁止挂载公开仓库" >&2
  exit 1
fi
if jq -e '.services.worker.secrets[]? | select(.source == "repository_gpg_private_key")' <<<"${publisher_json}" >/dev/null; then
  echo "Publisher Worker 禁止挂载仓库 GPG 私钥" >&2
  exit 1
fi
if [[ "$(jq '[.services.worker.secrets[]? | select(.source == "repository_gpg_public_key")] | length' <<<"${publisher_json}")" != "1" ]]; then
  echo "Publisher Worker 必须只获得仓库 GPG 公钥" >&2
  exit 1
fi
if jq -e '.services.signer.secrets[]? | select(.source == "repository_gpg_public_key")' <<<"${publisher_json}" >/dev/null; then
  echo "Signer 不需要挂载仓库 GPG 公钥 secret" >&2
  exit 1
fi

archiver_json="$(docker compose -f deploy/archiver/compose.yaml config --format json)"
if [[ "$(jq '[.services.worker.secrets[]? | select(.source == "publisher_pull_key" or .source == "publisher_known_hosts")] | length' <<<"${archiver_json}")" != "2" ]]; then
  echo "Archiver 必须使用独立的 Publisher 只读拉取凭据" >&2
  exit 1
fi
if jq -e '.services.worker.secrets[]? | select(.source | contains("gpg"))' <<<"${archiver_json}" >/dev/null; then
  echo "Archiver 禁止获得仓库 GPG 密钥" >&2
  exit 1
fi

echo "Compose 安全策略检查通过"
