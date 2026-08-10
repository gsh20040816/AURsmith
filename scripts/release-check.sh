#!/usr/bin/env bash
set -euo pipefail

version="${1:-}"
if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "用法：scripts/release-check.sh MAJOR.MINOR.PATCH" >&2
  exit 2
fi

if [[ -n "$(git status --porcelain)" ]]; then
  echo "发布前 Git 工作树必须干净" >&2
  exit 1
fi

cargo_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
web_version="$(jq -r .version web/package.json)"
if [[ "${cargo_version}" != "${version}" || "${web_version}" != "${version}" ]]; then
  echo "Cargo workspace、Web 和发布版本必须一致" >&2
  exit 1
fi

if git rev-parse "v${version}" >/dev/null 2>&1; then
  echo "标签 v${version} 已存在" >&2
  exit 1
fi

bash scripts/test-all.sh

jq -n \
  --arg version "${version}" \
  --arg commit "$(git rev-parse HEAD)" \
  --arg created_at "$(date --utc +%Y-%m-%dT%H:%M:%SZ)" \
  '{schema_version: 1, version: $version, source_git_commit: $commit, created_at: $created_at}'
