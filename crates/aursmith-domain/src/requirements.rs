use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Requirement {
    pub id: &'static str,
    pub title: &'static str,
}

pub const REQUIREMENTS: &[Requirement] = &[
    Requirement {
        id: "P01",
        title: "AUR 软件包搜索和订阅生命周期",
    },
    Requirement {
        id: "P02",
        title: "AUR 与 Git VCS 上游跟踪",
    },
    Requirement {
        id: "P03",
        title: "pkgbase 与 split outputs",
    },
    Requirement {
        id: "P04",
        title: "依赖 DAG、隐式引用、Provider 和循环",
    },
    Requirement {
        id: "P05",
        title: "依赖闭包事务化 ReleaseBatch",
    },
    Requirement {
        id: "P06",
        title: "本地 pkgrel 重建版本",
    },
    Requirement {
        id: "P07",
        title: "官方仓库和 AUR 生命周期事件",
    },
    Requirement {
        id: "P08",
        title: "失败时保留稳定 Release",
    },
    Requirement {
        id: "A01",
        title: "确定性扫描和 Agent 审计",
    },
    Requirement {
        id: "A02",
        title: "三个低成本 Agent 投票策略",
    },
    Requirement {
        id: "A03",
        title: "Agent 隔离、配置和溯源",
    },
    Requirement {
        id: "A04",
        title: "诚实记录审计覆盖范围",
    },
    Requirement {
        id: "B01",
        title: "KVM 隔离构建",
    },
    Requirement {
        id: "B02",
        title: "Fetch 受限联网与 Build 断网",
    },
    Requirement {
        id: "B03",
        title: "完整构建 provenance",
    },
    Requirement {
        id: "B04",
        title: "动态依赖 Profile",
    },
    Requirement {
        id: "B05",
        title: "构建镜像源配置与溯源",
    },
    Requirement {
        id: "W01",
        title: "全部服务 Docker Compose 部署",
    },
    Requirement {
        id: "W02",
        title: "角色可分离部署",
    },
    Requirement {
        id: "W03",
        title: "OpenSSH 与 rsync 传输",
    },
    Requirement {
        id: "W04",
        title: "Worker 幂等和 Journal",
    },
    Requirement {
        id: "W05",
        title: "静态多 Builder 拓扑",
    },
    Requirement {
        id: "R01",
        title: "产物校验和离线 Signer",
    },
    Requirement {
        id: "R02",
        title: "不可变 pacman Release 原子发布",
    },
    Requirement {
        id: "R03",
        title: "独立归档和控制面备份",
    },
    Requirement {
        id: "R04",
        title: "仓库回滚和客户端降级命令",
    },
    Requirement {
        id: "U01",
        title: "完整 Web UI",
    },
    Requirement {
        id: "U02",
        title: "单用户认证和客户端接入",
    },
    Requirement {
        id: "U03",
        title: "告警、Doctor 和恢复状态",
    },
    Requirement {
        id: "O01",
        title: "复用成熟 Arch 工具",
    },
    Requirement {
        id: "O02",
        title: "Git 管理交付和发布全过程",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn requirement_ids_are_unique() {
        let unique: BTreeSet<_> = REQUIREMENTS
            .iter()
            .map(|requirement| requirement.id)
            .collect();
        assert_eq!(unique.len(), REQUIREMENTS.len());
    }
}
