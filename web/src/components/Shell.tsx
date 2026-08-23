import { ReactNode, useEffect, useMemo, useState } from "react";
import { NavLink, Navigate, Route, Routes, useLocation } from "react-router-dom";
import { useApp } from "../lib/appctx";
import { api } from "../lib/api";
import { IconBell, IconBox, IconGrid, IconHammer, IconPackage, IconRocket, IconShield, IconTerminal } from "./icons";
import { CommandPalette } from "./CommandPalette";
import { Avatar } from "./Avatar";
import { useToast } from "./Toast";
import { Dashboard } from "../features/Dashboard";
import { Packages } from "../features/Packages";
import { Audits } from "../features/Audits";
import { Builds } from "../features/Builds";
import { Releases } from "../features/Releases";
import { Client } from "../features/Client";

type NavItem = { to: string; label: string; icon: ReactNode; badge?: number };

const TITLES: Record<string, { title: string; sub: string }> = {
  "/dashboard": { title: "总览", sub: "真实运行状态" },
  "/packages": { title: "软件包", sub: "订阅与依赖闭包" },
  "/audits": { title: "审查", sub: "diff-first 3+1" },
  "/builds": { title: "构建", sub: "固定 Builder 队列" },
  "/releases": { title: "发布", sub: "current / previous" },
  "/client": { title: "客户端", sub: "首次接入与 keyring" }
};

export function Shell() {
  const { session, logout } = useApp();
  const toast = useToast();
  const location = useLocation();
  const [palette, setPalette] = useState(false);
  const [auditAttention, setAuditAttention] = useState(0);
  const [activeJobs, setActiveJobs] = useState(0);
  const [keyringGeneration, setKeyringGeneration] = useState<number | null>(null);

  useEffect(() => {
    let alive = true;
    let reported = false;
    const load = async () => {
      try {
        const [audits, jobs, client] = await Promise.all([
          api.audits(),
          api.jobs(),
          api.clientBootstrap()
        ]);
        if (!alive) return;
        setAuditAttention(audits.items.filter((a) => a.state === "manual_review").length);
        setActiveJobs(jobs.items.filter((j) => ["queued", "no_eligible_worker", "dispatched", "running", "uncertain"].includes(j.status)).length);
        setKeyringGeneration(client.keyring_generation);
        reported = false;
      } catch (reason) {
        if (!alive || reported) return;
        reported = true;
        toast.error("状态读取失败", reason instanceof Error ? reason.message : "无法读取导航状态");
      }
    };
    void load();
    const id = window.setInterval(() => void load(), 15000);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [toast]);

  const sections = useMemo(() => {
    const op: NavItem[] = [{ to: "/dashboard", label: "总览", icon: <IconGrid size={16} /> }];
    const repo: NavItem[] = [
      { to: "/packages", label: "软件包", icon: <IconPackage size={16} /> },
      { to: "/audits", label: "审查", icon: <IconShield size={16} />, badge: auditAttention || undefined },
      { to: "/builds", label: "构建", icon: <IconHammer size={16} />, badge: activeJobs || undefined },
      { to: "/releases", label: "发布", icon: <IconRocket size={16} /> }
    ];
    const access: NavItem[] = [{ to: "/client", label: "客户端", icon: <IconTerminal size={16} /> }];
    return [
      { label: "操作", items: op },
      { label: "仓库", items: repo },
      { label: "接入", items: access }
    ];
  }, [auditAttention, activeJobs]);

  const current = TITLES[location.pathname] ?? TITLES["/dashboard"];

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="sidebar__brand">
          <div className="sidebar__brand-mark">
            <IconBox size={16} />
          </div>
          <div>
            <div className="sidebar__brand-name">AURsmith</div>
            <div className="sidebar__brand-sub">私有 AUR 仓库</div>
          </div>
        </div>

        <nav className="sidebar__nav">
          {sections.map((section) => (
            <div key={section.label}>
              <div className="nav-section">{section.label}</div>
              {section.items.map((item) => (
                <NavLink
                  key={item.to}
                  to={item.to}
                  className={({ isActive }) => `nav-item ${isActive ? "is-on" : ""}`}
                >
                  {item.icon}
                  <span>{item.label}</span>
                  {item.badge != null && <span className="count">{item.badge}</span>}
                </NavLink>
              ))}
            </div>
          ))}
        </nav>

        <div className="sidebar__foot">
          <div className="sidebar__env">
            <div className="sidebar__env-row">
              <span>节点</span>
              <strong className="dual">
                <span>公网</span>
                <span>Builder</span>
              </strong>
            </div>
            <div className="sidebar__env-row">
              <span>keyring</span>
              <strong>{keyringGeneration == null ? "未发布" : `gen ${keyringGeneration}`}</strong>
            </div>
          </div>
        </div>
      </aside>

      <div className="main">
        <header className="topbar">
          <div className="topbar__title">
            <strong>{current.title}</strong>
            <span>{current.sub}</span>
          </div>
          <div className="topbar__spacer" />
          <button className="btn btn--ghost topbar__search" aria-label="跳转页面" onClick={() => setPalette(true)}>
            <IconGrid size={15} />
            <span className="u-muted" style={{ flex: 1 }}>跳转页面…</span>
            <kbd className="key">⌘K</kbd>
          </button>
          <div className="topbar__actions">
            <button className="btn btn--ghost btn--icon" aria-label="通知">
              <IconBell size={16} />
            </button>
            <div className="row" style={{ gap: 8 }}>
              <Avatar name={session?.username ?? "a"} />
              <button
                className="btn btn--ghost btn--sm"
                aria-label="退出登录"
                onClick={() => {
                  void logout().catch((reason) => {
                    toast.error("退出失败", reason instanceof Error ? reason.message : "退出请求失败");
                  });
                }}
              >
                退出
              </button>
            </div>
          </div>
        </header>

        <main className="content">
          <Routes>
            <Route path="/dashboard" element={<Dashboard />} />
            <Route path="/packages" element={<Packages />} />
            <Route path="/audits" element={<Audits />} />
            <Route path="/builds" element={<Builds />} />
            <Route path="/releases" element={<Releases />} />
            <Route path="/client" element={<Client />} />
            <Route path="/" element={<Navigate to="/dashboard" replace />} />
            <Route path="*" element={<Navigate to="/dashboard" replace />} />
          </Routes>
        </main>
      </div>

      <CommandPalette open={palette} onClose={() => setPalette(false)} />
    </div>
  );
}
