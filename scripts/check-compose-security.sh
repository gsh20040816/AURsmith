#!/usr/bin/env bash
set -euo pipefail

# 仅给 Compose 渲染提供无权限测试值，不连接真实 Worker。
export DOCKER_GID="${DOCKER_GID:-996}"
export AURSMITH_SECRET_GID="${AURSMITH_SECRET_GID:-1000}"
export AURSMITH_JOBS_DIR="${AURSMITH_JOBS_DIR:-/var/lib/aursmith-builder/jobs}"
export AURSMITH_PUBLIC_ORIGIN="https://aursmith.example.test"
export AURSMITH_CONTROLLER_POLL_URL="${AURSMITH_CONTROLLER_POLL_URL:-https://controller.example.test/api/v1/builder/poll}"
export AURSMITH_BUILDER_TOKEN_SHA256="${AURSMITH_BUILDER_TOKEN_SHA256:-0000000000000000000000000000000000000000000000000000000000000000}"
export AURSMITH_REVERSE_PUBLISHER_ENDPOINT="${AURSMITH_REVERSE_PUBLISHER_ENDPOINT:-ssh://aursmith@192.0.2.20:2223}"

for stack in controller builder; do
  json="$(docker compose -f "deploy/${stack}/compose.yaml" config --format json)"
  if jq -e '.services[] | select(.privileged == true)' <<<"${json}" >/dev/null; then
    echo "${stack}: 禁止 privileged" >&2
    exit 1
  fi
  if [[ "${stack}" != "builder" ]] \
    && jq -e '[.services[].volumes[]? | .source // ""] | any(test("docker[.]sock|libvirt"))' <<<"${json}" >/dev/null; then
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

if ! rg -q '^\s*respond @health 404$' deploy/netcup/Caddyfile.snippet; then
  echo "公网管理反代必须隐藏 health" >&2
  exit 1
fi
if rg -q 'immutable' deploy/netcup/Caddyfile.snippet \
  || ! rg -q '^\s*header @package Cache-Control "no-cache, must-revalidate"$' deploy/netcup/Caddyfile.snippet; then
  echo "同版本重建会替换同名包，公网包禁止 immutable 缓存" >&2
  exit 1
fi

builder_json="$(docker compose -f deploy/builder/compose.yaml config --format json)"
if [[ "$(jq -r '.services.worker.environment.AURSMITH_CONTROLLER_POLL_URL // ""' <<<"${builder_json}")" != */api/v1/builder/poll ]]; then
  echo "Builder 必须使用固定 /api/v1/builder/poll 端点" >&2
  exit 1
fi
if [[ "$(jq '[.services | to_entries[] | .key as $service | .value.volumes[]? | select(.source == "/var/run/docker.sock" and .target == "/var/run/docker.sock") | $service] | length' <<<"${builder_json}")" != "1" ]] \
  || [[ "$(jq -r '[.services | to_entries[] | .key as $service | .value.volumes[]? | select(.source == "/var/run/docker.sock" and .target == "/var/run/docker.sock") | $service] | first // ""' <<<"${builder_json}")" != "worker" ]]; then
  echo "只有可信 Builder Worker 必须且只能获得一个 Docker Socket" >&2
  exit 1
fi
if jq -e '.services.worker.volumes[]? | select(.source | test("libvirt"))' <<<"${builder_json}" >/dev/null; then
  echo "Builder 禁止 libvirt Socket" >&2
  exit 1
fi
build_image_json="$(docker compose --profile build-image -f deploy/builder/compose.yaml config --format json)"
if [[ "$(jq -r '.services["build-image"].image // ""' <<<"${build_image_json}")" != "aursmith-build:latest" ]] \
  || jq -e '.services["build-image"].volumes[]?' <<<"${build_image_json}" >/dev/null \
  || jq -e '.services["build-image"].secrets[]?' <<<"${build_image_json}" >/dev/null; then
  echo "Build image 必须只负责构建固定 tag，禁止挂载 Socket、目录或 secret" >&2
  exit 1
fi
if jq -e '.services | has("ssh")' <<<"${builder_json}" >/dev/null \
  || jq -e '.services.worker.ports[]?' <<<"${builder_json}" >/dev/null; then
  echo "反向 Builder 禁止 SSH sidecar 和公网入站端口" >&2
  exit 1
fi
if [[ "$(jq '[.services.worker.secrets[]? | select(.source == "publisher_push_key" or .source == "publisher_known_hosts" or .source == "controller_bearer_token")] | length' <<<"${builder_json}")" != "3" ]]; then
  echo "反向 Builder 必须使用 Controller Bearer secret、独立 Publisher 推送密钥和固定 known_hosts" >&2
  exit 1
fi

controller_json="$(docker compose -f deploy/controller/compose.yaml config --format json)"
publisher_json="${controller_json}"
if jq -e '.services | has("repository")' <<<"${publisher_json}" >/dev/null \
  || [[ "$(jq -r '.services.publisher.environment.AURSMITH_REPOSITORY_HTTP_BIND // ""' <<<"${publisher_json}")" != "0.0.0.0:8080" ]] \
  || [[ "$(jq '[.services.publisher.ports[]? | select(.target == 8080 and .host_ip == "127.0.0.1")] | length' <<<"${publisher_json}")" != "1" ]]; then
  echo "Publisher 必须自行提供只绑定宿主回环地址的仓库 HTTP 服务" >&2
  exit 1
fi
if jq -e '.services | has("signer")' <<<"${publisher_json}" >/dev/null; then
  echo "Publisher 不得再拆分独立 Signer 服务" >&2
  exit 1
fi
if [[ "$(jq '[.services.publisher.secrets[]? | select(.source == "repository_gpg_private_key")] | length' <<<"${publisher_json}")" != "1" ]]; then
  echo "固定 Publisher 必须直接持有仓库 GPG 私钥" >&2
  exit 1
fi
if [[ "$(jq '[.services.publisher.secrets[]? | select(.source == "repository_gpg_public_key")] | length' <<<"${publisher_json}")" != "1" ]]; then
  echo "Publisher Worker 必须只获得仓库 GPG 公钥" >&2
  exit 1
fi
if [[ "$(jq '[.services["publisher-ssh"].volumes[]? | select(.target == "/landing" and .source == "publisher-landing" and (.read_only // false) == false)] | length' <<<"${publisher_json}")" != "1" ]]; then
  echo "Publisher SSH 必须只通过 Publisher landing 卷接收受限 Builder 推送" >&2
  exit 1
fi
if [[ "$(jq '(.services["publisher-ssh"].networks | has("publisher-control")) and (.services["publisher-ssh"].networks | has("edge"))' <<<"${publisher_json}")" != "true" ]]; then
  echo "Publisher SSH 必须同时连接内部控制网络和可发布宿主端口的 edge 网络" >&2
  exit 1
fi
if jq -e '.services | has("pacoloco")' <<<"${publisher_json}" >/dev/null; then
  echo "Publisher 不得部署 pacoloco" >&2
  exit 1
fi

if jq -e '.services | has("web")' <<<"${controller_json}" >/dev/null \
  || [[ "$(jq '[.services.controller.ports[]? | select(.target == 8080 and .host_ip == "127.0.0.1")] | length' <<<"${controller_json}")" != "1" ]]; then
  echo "Controller 必须自行提供 Web/API，并只绑定宿主回环地址" >&2
  exit 1
fi
if [[ "$(jq '[.services["agent-low-1"].environment.AURSMITH_AGENT_BASE_URL, .services["agent-low-2"].environment.AURSMITH_AGENT_BASE_URL, .services["agent-low-3"].environment.AURSMITH_AGENT_BASE_URL] | unique | length' <<<"${controller_json}")" != "3" ]]; then
  echo "三个低成本 Agent 必须使用不同的凭据网关路由" >&2
  exit 1
fi
if [[ "$(jq '[.services["agent-credential-gateway"].secrets[]? | select(.source | test("^low_agent_[123]_api_key$"))] | length' <<<"${controller_json}")" != "3" ]]; then
  echo "三个低成本 Agent 必须各自使用独立 API key secret" >&2
  exit 1
fi
if [[ "$(jq '[.services["agent-low-1"].security_opt[]? | select(. == "seccomp:unconfined" or . == "apparmor:unconfined")] | length' <<<"${controller_json}")" != "2" ]]; then
  echo "Codex Runner 必须允许其非特权内层 mount namespace" >&2
  exit 1
fi
if [[ "$(jq '(.services.controller.networks | has("edge")) and (.services | has("backup-ssh") | not)' <<<"${controller_json}")" != "true" ]] \
  || [[ "$(jq -r '.networks.edge.internal // false' <<<"${controller_json}")" != "false" ]]; then
  echo "默认 Controller 只能通过非 internal 的 edge 网络发布回环端口" >&2
  exit 1
fi
if [[ "$(jq '(.services.controller.volumes | any(.target == "/run/aursmith-publisher")) and (.services.publisher.volumes | any(.target == "/run/aursmith"))' <<<"${controller_json}")" != "true" ]] \
  || [[ "$(jq -r '.services.controller.environment.AURSMITH_PUBLISHER_SOCKET // ""' <<<"${controller_json}")" != "/run/aursmith-publisher/worker.sock" ]]; then
  echo "Controller 与固定 Publisher 必须只通过共享 Unix Socket 通信" >&2
  exit 1
fi
echo "Compose 安全策略检查通过"
