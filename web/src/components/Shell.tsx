import { ReactNode, useEffect, useMemo, useState } from "react";
import { NavLink, Navigate, Route, Routes, useLocation } from "react-router-dom";
import { useApp } from "../lib/appctx";
import { api } from "../lib/api";
import { IconBox, IconGrid, IconHammer, IconPackage, IconRocket, IconShield, IconTerminal } from "./icons";
import { CommandPalette } from "./CommandPalette";
import { Avatar } from "./Avatar";
import { useToast } from "./Toast";
import { Dashboard } from "../features/Dashboard";
import { Packages } from "../features/Packages";
import { Reviews } from "../features/Reviews";
import { Builds } from "../features/Builds";
import { Publications } from "../features/Publications";
import { Client } from "../features/Client";

type NavItem = { to: string; label: string; icon: ReactNode; badge?: number };

const TITLES: Record<string, { title: string }> = {
  "/dashboard": { title: "总览" },
  "/packages": { title: "软件包" },
  "/reviews": { title: "审查" },
  "/builds": { title: "构建" },
  "/publications": { title: "发布" },
  "/client": { title: "客户端" }
};

export function Shell() {
  const { session, logout } = useApp();
  const toast = useToast();
  const location = useLocation();
  const [palette, setPalette] = useState(false);
  const [reviewAttention, setReviewAttention] = useState(0);
  const [activeBuilds, setActiveBuilds] = useState(0);
  const [signerOk, setSignerOk] = useState<boolean | null>(null);
  const [builderOk, setBuilderOk] = useState<boolean | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    let alive = true;
    let reported = false;
    const load = async () => {
      try {
        const status = await api.status();
        if (!alive) return;
        setReviewAttention(status.counts.manual_review);
        setActiveBuilds(status.counts.queued + status.counts.running);
        setSignerOk(status.checks.find((c) => c.id === "signer")?.ok ?? null);
        setBuilderOk(status.checks.find((c) => c.id === "builder")?.ok ?? null);
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
      { to: "/reviews", label: "审查", icon: <IconShield size={16} />, badge: reviewAttention || undefined },
      { to: "/builds", label: "构建", icon: <IconHammer size={16} />, badge: activeBuilds || undefined },
      { to: "/publications", label: "发布", icon: <IconRocket size={16} /> }
    ];
    const access: NavItem[] = [{ to: "/client", label: "客户端", icon: <IconTerminal size={16} /> }];
    return [
      { label: "操作", items: op },
      { label: "仓库", items: repo },
      { label: "接入", items: access }
    ];
  }, [reviewAttention, activeBuilds]);

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
              <span>Builder</span>
              <strong>{builderOk == null ? "—" : builderOk ? "在线" : "离线"}</strong>
            </div>
            <div className="sidebar__env-row">
              <span>签名器</span>
              <strong>{signerOk == null ? "—" : signerOk ? "正常" : "失败"}</strong>
            </div>
          </div>
        </div>
      </aside>

      <div className="main">
        <header className="topbar">
          <div className="topbar__title">
            <strong>{current.title}</strong>
          </div>
          <div className="topbar__spacer" />
          <button className="btn btn--ghost topbar__search" aria-label="跳转页面" onClick={() => setPalette(true)}>
            <IconGrid size={14} />
            <kbd className="key">⌘K</kbd>
          </button>
          <div className="topbar__actions">
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
            <Route path="/reviews" element={<Reviews />} />
            <Route path="/builds" element={<Builds />} />
            <Route path="/publications" element={<Publications />} />
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
