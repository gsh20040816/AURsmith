#!/usr/bin/env bash
set -euo pipefail

# 仅给 Compose 渲染提供无权限测试值，不连接真实 Worker。
export KVM_GID="${KVM_GID:-996}"
export AURSMITH_CONTROLLER_VERIFYING_KEY_HEX="${AURSMITH_CONTROLLER_VERIFYING_KEY_HEX:-0000000000000000000000000000000000000000000000000000000000000000}"

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
done

builder_json="$(docker compose -f deploy/builder/compose.yaml config --format json)"
if [[ "$(jq '[.services.worker.devices[]? | select(.source == "/dev/kvm")] | length' <<<"${builder_json}")" != "1" ]]; then
  echo "Builder 必须且只能显式获得 /dev/kvm 构建设备" >&2
  exit 1
fi

echo "Compose 安全策略检查通过"
