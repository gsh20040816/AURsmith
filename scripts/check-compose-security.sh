#!/usr/bin/env bash
set -euo pipefail

# 只用于渲染测试；生产 Compose 仍要求部署者显式提供 HTTPS Origin。
export AURSMITH_PUBLIC_ORIGIN="https://aursmith.example.com"

compose_json="$(docker compose -f deploy/compose.yaml config --format json)"

if [[ "$(jq -c '.services | keys' <<<"${compose_json}")" != '["aursmith"]' ]]; then
  echo "Compose 必须且只能包含 aursmith 服务" >&2
  exit 1
fi
if [[ "$(jq -r '.services.aursmith.read_only // false' <<<"${compose_json}")" != "true" ]] \
  || [[ "$(jq -c '.services.aursmith.cap_drop // []' <<<"${compose_json}")" != '["ALL"]' ]] \
  || [[ "$(jq '[.services.aursmith.security_opt[]? | select(. == "no-new-privileges:true")] | length' <<<"${compose_json}")" != "1" ]] \
  || [[ "$(jq -r '.services.aursmith.privileged // false' <<<"${compose_json}")" != "false" ]]; then
  echo "aursmith 必须使用只读根、cap_drop ALL、no-new-privileges 且禁止 privileged" >&2
  exit 1
fi
if [[ "$(jq '.services.aursmith.ports | length' <<<"${compose_json}")" != "1" ]] \
  || [[ "$(jq '[.services.aursmith.ports[] | select(.target == 8080 and .host_ip == "127.0.0.1" and .protocol == "tcp")] | length' <<<"${compose_json}")" != "1" ]]; then
  echo "管理服务必须且只能发布一个宿主回环 TCP 端口" >&2
  exit 1
fi
if [[ "$(jq '.services.aursmith.volumes | length' <<<"${compose_json}")" != "1" ]] \
  || [[ "$(jq '[.services.aursmith.volumes[] | select(.target == "/var/lib/aursmith" and .type == "volume" and (.read_only // false) == false)] | length' <<<"${compose_json}")" != "1" ]]; then
  echo "aursmith 必须且只能把持久数据写入专用数据卷" >&2
  exit 1
fi
if [[ "$(jq '(.services.aursmith.cap_add // []) | length' <<<"${compose_json}")" != "0" ]] \
  || [[ -n "$(jq -r '.services.aursmith.pid // empty' <<<"${compose_json}")" ]] \
  || [[ -n "$(jq -r '.services.aursmith.ipc // empty' <<<"${compose_json}")" ]] \
  || [[ -n "$(jq -r '.services.aursmith.network_mode // empty' <<<"${compose_json}")" ]]; then
  echo "单服务核心禁止 cap_add 以及 host pid/ipc/network 模式" >&2
  exit 1
fi
if jq -e '.services.aursmith | (.devices[]? // empty), (.secrets[]? // empty)' <<<"${compose_json}" >/dev/null \
  || jq -e '.services.aursmith.volumes[]? | select((.source // "") | test("docker[.]sock|libvirt|kvm"))' <<<"${compose_json}" >/dev/null; then
  echo "单服务核心禁止设备、secret sidecar 输入或宿主运行时 socket" >&2
  exit 1
fi
if [[ "$(jq -r '.services.aursmith.environment.AURSMITH_DATABASE_PATH' <<<"${compose_json}")" != "/var/lib/aursmith/aursmith.db" ]] \
  || [[ "$(jq -r '.services.aursmith.environment.AURSMITH_PUBLIC_ORIGIN' <<<"${compose_json}")" != "https://aursmith.example.com" ]]; then
  echo "Compose 必须使用 fresh 数据库路径和显式公网 Origin" >&2
  exit 1
fi
if [[ "$(jq -c '.services.aursmith.healthcheck.test' <<<"${compose_json}")" != '["CMD","curl","--fail","--silent","--show-error","http://127.0.0.1:8080/healthz"]' ]] \
  || [[ "$(jq -r '.services.aursmith.healthcheck.interval' <<<"${compose_json}")" != "30s" ]] \
  || [[ "$(jq -r '.services.aursmith.healthcheck.timeout' <<<"${compose_json}")" != "5s" ]] \
  || [[ "$(jq -r '.services.aursmith.healthcheck.retries' <<<"${compose_json}")" != "3" ]]; then
  echo "Compose healthcheck 必须固定为内部 HTTP liveness" >&2
  exit 1
fi

while IFS= read -r from_line; do
  if [[ "${from_line}" != *"@sha256:"* ]]; then
    echo "Dockerfile 基础镜像必须固定 digest: ${from_line}" >&2
    exit 1
  fi
done < <(rg '^FROM ' deploy/Dockerfile)
if ! rg -q '^USER 10001:10001$' deploy/Dockerfile; then
  echo "运行镜像必须固定使用 USER 10001:10001" >&2
  exit 1
fi

if ! rg -q '^\s*respond @health 404$' deploy/Caddyfile.example \
  || ! rg -q '^\s*header_up X-AURsmith-Client-IP \{remote_host\}$' deploy/Caddyfile.example; then
  echo "通用反代必须隐藏 health 并覆盖可信客户端 IP header" >&2
  exit 1
fi
if ! command -v caddy >/dev/null 2>&1; then
  echo "缺少 caddy，无法验证通用反代示例" >&2
  exit 1
fi
caddy validate --config deploy/Caddyfile.example --adapter caddyfile >/dev/null

echo "单服务 Compose 安全策略检查通过"
