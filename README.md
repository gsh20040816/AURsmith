# AURsmith

AURsmith 是面向少量 Arch Linux 客户端的私有、可审计 AUR 二进制仓库。它持续跟踪用户订阅的 AUR 软件包，审计不可变修订，在 KVM 虚拟机中构建，发布经过签名的 pacman 仓库，并独立归档历史 Release。

系统拆分为 Controller、Builder、Publisher 和 Archiver 四套 Docker Compose Stack。任何 AURsmith 服务都不会直接部署到宿主机。

规范性需求位于 `docs/requirements.md`。只有当一个需求 ID 具备实现、自动化测试或明确的人工验证记录时，才能标记为完成。

开发和发布全过程使用 Git 管理：验证通过的改动按阶段形成小型提交并直接进入 `main`，每个发布版本记录产生它的准确源码 commit。
