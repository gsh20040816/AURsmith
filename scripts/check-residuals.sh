#!/usr/bin/env bash
set -euo pipefail

metadata="$(cargo metadata --locked --no-deps --format-version 1)"
if [[ "$(jq '.workspace_members | length' <<<"${metadata}")" != "1" ]] \
  || [[ "$(jq '.packages | length' <<<"${metadata}")" != "1" ]] \
  || [[ "$(jq -r '.packages[0].name' <<<"${metadata}")" != "aursmith" ]] \
  || [[ "$(jq '[.packages[0].targets[] | select(.kind == ["bin"] and .name == "aursmith")] | length' <<<"${metadata}")" != "1" ]]; then
  echo "Cargo workspace 必须只有一个 aursmith crate 和一个 aursmith 二进制" >&2
  exit 1
fi

mapfile -t migration_entries < <(find migrations -mindepth 1 -printf '%P:%y\n' | sort)
if [[ "${#migration_entries[@]}" != "1" || "${migration_entries[0]}" != "0001_core.sql:f" ]]; then
  echo "migrations 递归拓扑必须且只能包含普通文件 0001_core.sql" >&2
  exit 1
fi

mapfile -t crate_entries < <(find crates -mindepth 1 -maxdepth 1 -printf '%f:%y\n' | sort)
if [[ "${#crate_entries[@]}" != "1" || "${crate_entries[0]}" != "aursmith:d" ]]; then
  echo "crates 顶层必须且只能包含 aursmith 目录" >&2
  exit 1
fi

while IFS= read -r deploy_entry; do
  case "${deploy_entry}" in
    Dockerfile | compose.yaml | Caddyfile.example | \
      archiver | builder | controller | publisher | \
      archiver/secrets | archiver/secrets/* | \
      builder/secrets | builder/secrets/* | \
      controller/secrets | controller/secrets/* | \
      publisher/secrets | publisher/secrets/*)
      ;;
    *)
      echo "deploy 出现允许列表外路径：${deploy_entry}" >&2
      exit 1
      ;;
  esac
done < <(find deploy -mindepth 1 -printf '%P\n' | sort)

if [[ -d web ]]; then
  while IFS= read -r web_entry; do
    case "${web_entry}" in
      dist:d | node_modules:d)
        ;;
      *)
        echo "web 只允许本地 ignored 的 dist/node_modules 目录：${web_entry}" >&2
        exit 1
        ;;
    esac
  done < <(find web -mindepth 1 -maxdepth 1 -printf '%f:%y\n' | sort)
fi

for required_file in deploy/Dockerfile deploy/compose.yaml deploy/Caddyfile.example; do
  if [[ ! -f "${required_file}" ]]; then
    echo "deploy 缺少允许列表中的必需文件：${required_file}" >&2
    exit 1
  fi
done

if ! rg -q 'https://aur[.]archlinux[.]org/\{pkgbase\}[.]git' crates/aursmith/src/aur.rs \
  || ! rg -q 'OsStr::new\("--depth=1"\)' crates/aursmith/src/aur.rs; then
  echo "生产 AUR 输入必须固定为官方 HTTPS pkgbase URL 和 depth=1 fetch" >&2
  exit 1
fi
if rg -n \
  -g '!docs/refactor-requirements.md' \
  -g '!scripts/check-residuals.sh' \
  'AURSMITH_(AUR|GIT)_(URL|REMOTE|PROXY)' \
  .; then
  echo "禁止任意 AUR 远端或代理配置" >&2
  exit 1
fi

if rg -n \
  -g '!docs/refactor-requirements.md' \
  -g '!scripts/check-residuals.sh' \
  -g '!scripts/check-compose-security.sh' \
  '(aursmith-(controller|worker|protocol|domain|ctl|signer|guest-agent|agent-runner|agent-gateway)|SignedEnvelope|ReverseWorker|ReleaseAuthorization|/dev/kvm|libvirt|pacoloco|source-proxy|profile-builder|npm (ci|run)|vite build)' \
  .; then
  echo "源码、配置或文档中仍有旧构建链标识" >&2
  exit 1
fi

echo "旧实现残留扫描通过"
