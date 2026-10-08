#!/usr/bin/env python3
"""Compose 静态安全检查（不需要 docker）。

逐条核对与安全基线相关的部署约束；任何一条不满足都以非零退出。
"""
import pathlib
import sys

import yaml

ROOT = pathlib.Path(__file__).resolve().parent.parent
failures = []


def check(condition, message):
    if not condition:
        failures.append(message)


def load(path):
    with open(ROOT / path, encoding="utf-8") as handle:
        return yaml.safe_load(handle)


def hardened(name, service):
    check(service.get("read_only") is True, f"{name}: 必须 read_only")
    check(service.get("cap_drop") == ["ALL"], f"{name}: 必须 cap_drop: [ALL]")
    check("no-new-privileges:true" in service.get("security_opt", []), f"{name}: 必须 no-new-privileges")
    check(not service.get("privileged"), f"{name}: 不允许 privileged")
    check(not service.get("cap_add"), f"{name}: 不允许 cap_add")


server = load("deploy/server/compose.yaml")
services = server["services"]
check(sorted(services) == ["aursmithd", "signer"], f"公网节点只允许 aursmithd 与 signer，实际 {sorted(services)}")
for name, service in services.items():
    hardened(name, service)
    check(service.get("build", {}).get("dockerfile") == "deploy/Dockerfile", f"{name}: 必须使用 deploy/Dockerfile")
    for volume in service.get("volumes", []):
        check("docker.sock" not in volume, f"{name}: 公网节点不允许挂载 docker.sock")

signer = services["signer"]
aursmithd = services["aursmithd"]
check(signer.get("network_mode") == "none", "signer: 必须 network_mode: none（基线 1）")
check("ports" not in signer and "networks" not in signer, "signer: 不允许端口或网络")
check(signer.get("secrets") == ["repository_gpg_key"], "signer: 只允许持有仓库签名私钥")
check("repository_gpg_key" not in aursmithd.get("secrets", []), "aursmithd: 不允许持有签名私钥（基线 1）")
check(all(not str(s).startswith("repository_gpg") for s in aursmithd.get("secrets", [])), "aursmithd: 不允许任何签名密钥")
check(any(t.startswith("/run/aursmith-gnupg:") and "mode=0700" in t for t in signer.get("tmpfs", [])), "signer: GnuPG home 必须是 0700 tmpfs")
check(all(not v.startswith("data:") for v in signer.get("volumes", [])), "signer: 不允许访问主服务数据库卷")
check(any(v.startswith("exchange:") for v in aursmithd.get("volumes", [])), "aursmithd: 必须挂载 exchange 卷")
check(any(v.startswith("exchange:") for v in signer.get("volumes", [])), "signer: 必须挂载 exchange 卷")
ports = aursmithd.get("ports", [])
check(len(ports) == 1 and "127.0.0.1" in ports[0], "aursmithd: 只允许绑定到宿主回环地址，由 Caddy 反代")
check(len(server.get("networks", {})) == 1, "公网节点只允许一个 Docker 网络")

home = load("deploy/home/compose.yaml")
builder = home["services"]["builder"]
hardened("builder", builder)
check(builder.get("secrets") == ["builder_token"], "builder: 只允许 Builder token 一个 secret")
check("ports" not in builder, "builder: 不允许暴露端口")
check(not any("SSH" in key or "REVERSE" in key for key in builder.get("environment", {})), "builder: 不允许残留 SSH 中继配置")
check(builder["environment"].get("AURSMITH_BUILD_IMAGE") == "aursmith-build:latest", "builder: 构建镜像固定为 aursmith-build:latest")
build_image = home["services"]["build-image"]
check(build_image.get("profiles") == ["build-image"], "build-image: 只作为构建 profile")
check("secrets" not in build_image and "environment" not in build_image, "build-image: 构建容器不允许任何凭据（基线 5）")

environment = set()
for service in list(services.values()) + [builder]:
    environment.update(k for k in service.get("environment", {}) if k.startswith("AURSMITH_"))
check(len(environment) <= 26, f"AURSMITH_* 运行时变量过多：{len(environment)}")

if failures:
    for failure in failures:
        print(f"✗ {failure}", file=sys.stderr)
    sys.exit(1)
print(f"Compose 检查通过：公网 {len(services)} 个容器、{len(server.get('networks', {}))} 个网络；运行时 AURSMITH_* 变量 {len(environment)} 个")
