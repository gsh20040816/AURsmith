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
  find "${runtime_directory}" -mindepth 1 -depth -delete >/dev/null 2>&1 || true
  rmdir "${runtime_directory}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

cargo build --locked -p aursmith-worker -p aursmithctl

install -d -m 0700 "${runtime_directory}/key-home" "${runtime_directory}/publisher-gpg"
install -d "${runtime_directory}/landing" "${runtime_directory}/staging" \
  "${runtime_directory}/repository" \
  "${runtime_directory}/jobs"
gpg --homedir "${runtime_directory}/key-home" --batch --passphrase '' \
  --quick-generate-key 'AURsmith upstream smoke' ed25519 sign 1d >/dev/null 2>&1
gpg --homedir "${runtime_directory}/key-home" --batch \
  --output "${runtime_directory}/repository-public-key.gpg" \
  --export 'AURsmith upstream smoke'
gpg --homedir "${runtime_directory}/key-home" --batch \
  --output "${runtime_directory}/repository-private-key.gpg" \
  --export-secret-keys 'AURsmith upstream smoke'

AURSMITH_WORKER_NAME=publisher-smoke \
AURSMITH_WORKER_ROLE=publisher \
AURSMITH_WORKER_SOCKET="${runtime_directory}/worker.sock" \
AURSMITH_WORKER_DATABASE="sqlite://${runtime_directory}/worker.db" \
 AURSMITH_REPOSITORY_GPG_PUBLIC_KEY_FILE="${runtime_directory}/repository-public-key.gpg" \
 AURSMITH_REPOSITORY_GPG_PRIVATE_KEY_FILE="${runtime_directory}/repository-private-key.gpg" \
 AURSMITH_PUBLISHER_GPG_HOME="${runtime_directory}/publisher-gpg" \
 AURSMITH_LANDING_DIR="${runtime_directory}/landing" \
 AURSMITH_PUBLISHER_STAGING_DIR="${runtime_directory}/staging" \
 AURSMITH_REPOSITORY_DIR="${runtime_directory}/repository" \
 AURSMITH_JOBS_DIR="${runtime_directory}/jobs" \
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

doctor_result="$("${repository_root}/target/debug/aursmithctl" worker \
  --socket "${runtime_directory}/worker.sock" publisher-doctor)"
jq -e '.ok == true and .data.checks.aur.ok == true' <<<"${doctor_result}" >/dev/null

echo "Publisher 上游冒烟通过"
