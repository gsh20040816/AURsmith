#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temporary_directory="$(mktemp -d /tmp/aursmith-ssh-smoke.XXXXXX)"
project_name="aursmith-ssh-smoke"
port="${AURSMITH_SMOKE_SSH_PORT:-32222}"

cleanup() {
  AURSMITH_SSH_HOST_KEY_FILE="${temporary_directory}/host_key" \
  AURSMITH_AUTHORIZED_KEYS_FILE="${temporary_directory}/authorized_keys" \
  AURSMITH_CONTROLLER_VERIFYING_KEY_HEX="${AURSMITH_CONTROLLER_VERIFYING_KEY_HEX}" \
  KVM_GID="${KVM_GID}" \
  AURSMITH_SSH_BIND="127.0.0.1:${port}" \
    docker compose -p "${project_name}" -f "${repository_root}/deploy/builder/compose.yaml" \
      down --volumes >/dev/null 2>&1 || true
  shred -u -- "${temporary_directory}/client_key" "${temporary_directory}/host_key" >/dev/null 2>&1 || true
  find "${temporary_directory}" -mindepth 1 -maxdepth 1 -type f -delete >/dev/null 2>&1 || true
  rmdir "${temporary_directory}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

if [[ ! -c /dev/kvm ]]; then
  echo "缺少 /dev/kvm，无法执行 Builder SSH 冒烟" >&2
  exit 1
fi

export KVM_GID="$(stat -c %g /dev/kvm)"
export AURSMITH_CONTROLLER_VERIFYING_KEY_HEX="0000000000000000000000000000000000000000000000000000000000000000"
export AURSMITH_SSH_HOST_KEY_FILE="${temporary_directory}/host_key"
export AURSMITH_AUTHORIZED_KEYS_FILE="${temporary_directory}/authorized_keys"
export AURSMITH_SSH_BIND="127.0.0.1:${port}"
export AURSMITH_SOURCE_GIT_COMMIT="$(git -C "${repository_root}" rev-parse HEAD)"

ssh-keygen -q -t ed25519 -N '' -f "${temporary_directory}/client_key"
ssh-keygen -q -t ed25519 -N '' -f "${temporary_directory}/host_key"
cp "${temporary_directory}/client_key.pub" "${temporary_directory}/authorized_keys"
ssh-keygen -y -f "${temporary_directory}/host_key" >"${temporary_directory}/host_key.pub"
printf '[127.0.0.1]:%s ' "${port}" >"${temporary_directory}/known_hosts"
cat "${temporary_directory}/host_key.pub" >>"${temporary_directory}/known_hosts"

docker compose -p "${project_name}" -f "${repository_root}/deploy/builder/compose.yaml" \
  up -d --build --wait

sshd_pid="$(docker inspect "${project_name}-ssh-1" --format '{{.State.Pid}}')"
read -r _ real_uid effective_uid _ < <(rg '^Uid:' "/proc/${sshd_pid}/status")
if [[ "${real_uid}" != "10001" || "${effective_uid}" != "10001" ]]; then
  echo "sshd 未永久降权到 UID 10001" >&2
  exit 1
fi
if ! rg -q '^CapEff:\s+0+$' "/proc/${sshd_pid}/status" || \
   ! rg -q '^CapBnd:\s+0+$' "/proc/${sshd_pid}/status"; then
  echo "sshd 仍保留 Linux capability" >&2
  exit 1
fi

if ! status="$(ssh \
  -i "${temporary_directory}/client_key" \
  -o BatchMode=yes \
  -o IdentitiesOnly=yes \
  -o StrictHostKeyChecking=yes \
  -o "UserKnownHostsFile=${temporary_directory}/known_hosts" \
  -o ConnectTimeout=5 \
  -p "${port}" \
  aursmith@127.0.0.1 status)"; then
  docker compose -p "${project_name}" -f "${repository_root}/deploy/builder/compose.yaml" \
    logs --no-color ssh >&2
  exit 1
fi

jq -e '.ok == true and .data.name == "compute-01" and .data.role == "builder"' \
  <<<"${status}" >/dev/null
echo "Worker SSH 冒烟通过"
