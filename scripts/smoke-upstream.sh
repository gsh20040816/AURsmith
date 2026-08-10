#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runtime_directory="$(mktemp -d /tmp/aursmith-upstream-smoke.XXXXXX)"
worker_pid=""

cleanup() {
  if [[ -n "${worker_pid}" ]]; then
    kill "${worker_pid}" >/dev/null 2>&1 || true
    wait "${worker_pid}" >/dev/null 2>&1 || true
  fi
  find "${runtime_directory}" -mindepth 1 -maxdepth 1 -type f -delete >/dev/null 2>&1 || true
  find "${runtime_directory}" -mindepth 1 -maxdepth 1 -type s -delete >/dev/null 2>&1 || true
  rmdir "${runtime_directory}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

cargo build --locked -p aursmith-worker -p aursmithctl

AURSMITH_WORKER_NAME=publisher-smoke \
AURSMITH_WORKER_ROLE=publisher \
AURSMITH_WORKER_SOCKET="${runtime_directory}/worker.sock" \
AURSMITH_WORKER_DATABASE="sqlite://${runtime_directory}/worker.db" \
AURSMITH_CONTROLLER_VERIFYING_KEY_HEX=0000000000000000000000000000000000000000000000000000000000000000 \
  "${repository_root}/target/debug/aursmith-worker" &
worker_pid="$!"

for _ in $(seq 1 50); do
  [[ -S "${runtime_directory}/worker.sock" ]] && break
  sleep .1
done
if [[ ! -S "${runtime_directory}/worker.sock" ]]; then
  echo "Publisher Worker 未能启动" >&2
  exit 1
fi

search_result="$("${repository_root}/target/debug/aursmithctl" worker \
  --socket "${runtime_directory}/worker.sock" aur-search visual-studio-code-bin)"
jq -e '.ok == true and (.data.items | length) >= 1' <<<"${search_result}" >/dev/null

snapshot_result="$("${repository_root}/target/debug/aursmithctl" worker \
  --socket "${runtime_directory}/worker.sock" aur-snapshot visual-studio-code-bin)"
jq -e '.ok == true and (.data.aur_commit | test("^[0-9a-f]{40}$")) and (.data.outputs | index("visual-studio-code-bin"))' \
  <<<"${snapshot_result}" >/dev/null

vcs_result="$("${repository_root}/target/debug/aursmithctl" worker \
  --socket "${runtime_directory}/worker.sock" aur-snapshot paru-git)"
jq -e '.ok == true and (.data.vcs_commit | test("^[0-9a-f]{40}$"))' \
  <<<"${vcs_result}" >/dev/null

official_result="$("${repository_root}/target/debug/aursmithctl" worker \
  --socket "${runtime_directory}/worker.sock" official-info pacman)"
jq -e '.ok == true and (.data.pacman | length) >= 1' <<<"${official_result}" >/dev/null

echo "Publisher 上游冒烟通过"
