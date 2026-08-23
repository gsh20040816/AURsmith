import { useEffect, useState } from "react";
import { api } from "../lib/api";
import { usePolling, useStable } from "../lib/hooks";
import { shortHash } from "../lib/format";
import { type AurPackage, type PackageDetail, type Subscription } from "../lib/types";
import { Badge, Button, Card, CardBody, CardHead, CardTitle, Empty, PageHead, SearchBox, StatusBadge } from "../components/ui";
import { Confirm, Drawer } from "../components/Overlay";
import { useToast } from "../components/Toast";
import { IconAlert, IconPlus, IconRefresh, IconCheck } from "../components/icons";
import { statusMeta } from "../lib/status";

export function Packages() {
  const toast = useToast();
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<AurPackage[]>([]);
  const [searching, setSearching] = useState(false);
  const [searched, setSearched] = useState(false);
  const [detail, setDetail] = useState<PackageDetail | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<Subscription | null>(null);
  const [busyKey, setBusyKey] = useState("");

  const loadSubs = useStable(() => api.subscriptions());
  const subs = usePolling(loadSubs, 15000);

  // Debounced AUR search
  useEffect(() => {
    if (query.trim().length < 2) {
      setResults([]);
      setSearched(false);
      return;
    }
    const id = window.setTimeout(() => {
      setSearching(true);
      void api
        .searchAur(query.trim())
        .then((r) => {
          setResults(r.items);
          setSearched(true);
        })
        .catch((reason) => toast.error("搜索失败", reason instanceof Error ? reason.message : undefined))
        .finally(() => setSearching(false));
    }, 260);
    return () => window.clearTimeout(id);
  }, [query, toast]);

  const subbed = (base: string) =>
    (subs.data?.items ?? []).some((s) => s.package_base === base && s.kind === "direct");

  const operate = async (key: string, fn: () => Promise<unknown>, successMsg: string) => {
    setBusyKey(key);
    try {
      await fn();
      subs.reload();
      toast.success(successMsg);
    } catch (reason) {
      toast.error("操作失败", reason instanceof Error ? reason.message : undefined);
    } finally {
      setBusyKey("");
    }
  };

  const openDetail = async (sub: Subscription | { package_base: string }) => {
    setBusyKey(`detail-${sub.package_base}`);
    try {
      setDetail(await api.packageDetail(sub.package_base));
    } catch (reason) {
      toast.error("加载详情失败", reason instanceof Error ? reason.message : undefined);
    } finally {
      setBusyKey("");
    }
  };

  const shown = subs.data?.items ?? [];

  return (
    <>
      <PageHead
        eyebrow="pkgbase"
        title="AUR 软件包"
        lede="只有加入与删除两种生命周期操作；依赖随根订阅自动加入，删除时同步清理不再可达的隐式依赖。"
        actions={
          <Button variant="primary" icon={<IconPlus size={15} />} onClick={() => document.getElementById("pkg-search")?.focus()}>
            加入软件包
          </Button>
        }
      />

      <div className="stack">
        {subs.error && (
          <div className="notice notice--danger" role="alert">
            <IconAlert size={16} />
            <div><b>订阅列表读取失败。</b> {subs.error}</div>
          </div>
        )}

        <Card>
          <CardBody>
            <SearchBox
              value={query}
              onChange={setQuery}
              placeholder="搜索 AUR，例如 visual-studio-code-bin、typst…"
              autoFocus
            />
            {searching && (
              <div className="row" style={{ marginTop: 14, color: "var(--text-muted)", gap: 8 }}>
                <span className="spinner" /> 查询中…
              </div>
            )}
            {!searching && searched && results.length === 0 && (
              <div className="empty" style={{ padding: 28 }}>
                <h3>没有匹配的 AUR 包</h3>
                <p>尝试更短的名称或 pkgbase 关键字。</p>
              </div>
            )}
            {results.length > 0 && (
              <div className="stack" style={{ marginTop: 14 }}>
                {results.map((item) => {
                  const already = subbed(item.package_base);
                  return (
                    <div key={item.name} className="tbl__cell-main" style={{ border: "1px solid var(--border)", borderRadius: 12, padding: "12px 14px", flexDirection: "row", alignItems: "center", justifyContent: "space-between", display: "flex" }}>
                      <div>
                        <div className="row" style={{ gap: 8 }}>
                          <strong>{item.name}</strong>
                          <code className="inline">{item.version}</code>
                          {item.name !== item.package_base && <span className="pkg-tag pkg-tag--implicit">pkgbase {item.package_base}</span>}
                          {item.out_of_date && <span className="pkg-tag" style={{ color: "var(--warning)", borderColor: "var(--warning-border)", background: "var(--warning-soft)" }}>过期</span>}
                        </div>
                        <p className="u-secondary" style={{ fontSize: 13, marginTop: 4 }}>{item.description ?? "没有描述"}</p>
                        <div className="u-muted" style={{ fontSize: 12, marginTop: 4 }}>
                          {item.maintainer ? `维护者 ${item.maintainer}` : "孤儿包"}
                          {item.provides.length > 0 && ` · 提供 ${item.provides.join(", ")}`}
                        </div>
                      </div>
                      <Button
                        variant={already ? "ghost" : "primary"}
                        size="sm"
                        disabled={already || busyKey === item.name}
                        loading={busyKey === item.name}
                        onClick={() =>
                          operate(
                            item.name,
                            () => api.subscribe(item.name),
                            "已加入" // success
                          )
                        }
                      >
                        {already ? "已加入" : "加入"}
                      </Button>
                    </div>
                  );
                })}
              </div>
            )}
          </CardBody>
        </Card>

        <Card>
          <CardHead>
            <CardTitle eyebrow="订阅" title="显式包与必要依赖" />
            <div className="row">
              <Badge tone="accent">{shown.filter((s) => s.kind === "direct").length} 显式</Badge>
              <Badge tone="info">{shown.filter((s) => s.kind === "implicit").length} 依赖</Badge>
              <Button variant="ghost" size="sm" onClick={subs.reload} icon={<IconRefresh size={14} />}>刷新</Button>
            </div>
          </CardHead>
          <CardBody flush>
            {shown.length === 0 ? (
              <Empty title="尚未加入软件包" detail="从上方搜索 AUR，并加入需要的 pkgbase。" />
            ) : (
              <div className="tbl-wrap">
                <table className="tbl tbl--clickable">
                  <thead>
                    <tr>
                      <th>pkgbase</th>
                      <th>来源</th>
                      <th>版本 / 输出</th>
                      <th>引用</th>
                      <th style={{ width: 190 }}>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {shown.map((item) => (
                      <tr key={item.id} onClick={() => void openDetail(item)}>
                        <td>
                          <div className="tbl__cell-main">
                            <strong>{item.package_base}</strong>
                            <span className="tbl__sub">{item.description ?? "—"}</span>
                          </div>
                        </td>
                        <td>
                          <span className={`pkg-tag ${item.kind === "direct" ? "pkg-tag--direct" : "pkg-tag--implicit"}`}>
                            {item.kind === "direct" ? "显式加入" : "必要依赖"}
                          </span>
                        </td>
                        <td>
                          <div className="tbl__cell-main">
                            <code className="inline">{item.version ?? "等待同步"}</code>
                            <span className="tbl__sub">{item.outputs.join(" · ") || "—"}</span>
                          </div>
                        </td>
                        <td><code className="inline">{item.reference_count}</code></td>
                        <td onClick={(e) => e.stopPropagation()}>
                          <div className="row" style={{ gap: 6 }}>
                            <Button size="sm" variant="ghost" onClick={() => void openDetail(item)}>详情</Button>
                            {item.kind === "direct" && (
                              <>
                                <Button size="sm" variant="ghost" onClick={() => void operate(`refresh-${item.id}`, () => api.refreshPackage(item.package_base), "已检查更新")}>检查更新</Button>
                                <Button size="sm" variant="ghost" className="is-danger" onClick={() => setDeleteTarget(item)}>删除</Button>
                              </>
                            )}
                          </div>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </CardBody>
        </Card>
      </div>

      {detail && (
        <PackageDetailDrawer
          detail={detail}
          busyKey={busyKey}
          onClose={() => setDetail(null)}
          onSetPolicy={async (allow) => {
            await operate(`policy-${detail.package_base}`, () => api.setBuildPolicy(detail.package_base, allow), allow ? "已恢复 check()" : "已禁用 check()");
            setDetail(await api.packageDetail(detail.package_base));
          }}
          onProvider={async (dep, candidate) => {
            await operate(`provider-${dep}-${candidate}`, () => api.selectProvider(detail.package_base, dep, candidate), "已选择 Provider");
            setDetail(await api.packageDetail(detail.package_base));
          }}
          onRebuild={async () => {
            await operate(`rebuild-${detail.package_base}`, () => api.rebuildPackage(detail.package_base), "已创建手工重建任务");
          }}
        />
      )}

      <Confirm
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={() => {
          if (!deleteTarget) return;
          void operate(`delete-${deleteTarget.id}`, () => api.deleteSubscription(deleteTarget.package_base), "已删除订阅").then(() => {
            setDeleteTarget(null);
          });
        }}
        danger
        title={`删除 ${deleteTarget?.package_base}？`}
        body="它及其不再需要的依赖会在下一次原子发布中移出仓库；仍被其他显式订阅引用的共享依赖会保留。"
        confirmLabel="确认删除"
        busy={busyKey.startsWith("delete-")}
      />
    </>
  );
}

function PackageDetailDrawer({
  detail,
  busyKey,
  onClose,
  onSetPolicy,
  onProvider,
  onRebuild
}: {
  detail: PackageDetail;
  busyKey: string;
  onClose: () => void;
  onSetPolicy: (allow: boolean) => Promise<void>;
  onProvider: (dependency: string, candidate: string) => Promise<void>;
  onRebuild: () => Promise<void>;
}) {
  const [confirmRebuild, setConfirmRebuild] = useState(false);
  const revisions = detail.revisions;

  return (
    <Drawer
      open
      onClose={onClose}
      title={`${detail.package_base} · ${detail.version}`}
      eyebrow="pkgbase 详情"
      footer={
        <div className="row" style={{ justifyContent: "flex-start" }}>
          <Button variant="primary" size="sm" loading={busyKey === `rebuild-${detail.package_base}`} onClick={() => setConfirmRebuild(true)}>
            手工重建
          </Button>
          <Button size="sm" variant="ghost" onClick={onClose}>关闭</Button>
        </div>
      }
    >
      <div className="stack">
        <NoticeBlock>
          手工重建保持原版本与 pkgrel，客户端不会按版本比较自动升级；同名不同内容制品切换时，旧数据库与新文件可能短暂不一致。
        </NoticeBlock>

        <p className="u-secondary" style={{ fontSize: 13.5, lineHeight: 1.7 }}>
          {detail.description ?? "没有描述"}
          {detail.maintainer ? ` · 维护者 ${detail.maintainer}` : " · 孤儿包"}
        </p>
        <div className="row" style={{ gap: 6 }}>
          {detail.outputs.map((o) => <code className="inline" key={o}>{o}</code>)}
        </div>

        <Section title="构建策略">
          <div className="doctor-list">
            <div className="doctor-row">
              <div className="doctor-row__name">
                <span className="doctor-row__check ok"><IconCheck size={13} /></span>
                <span><code className="inline">check()</code> {detail.build_policy.allow_check ? "默认执行" : "已显式禁用"}</span>
              </div>
              <Button
                size="sm"
                variant="ghost"
                loading={busyKey === `policy-${detail.package_base}`}
                onClick={() => void onSetPolicy(!detail.build_policy.allow_check)}
              >
                {detail.build_policy.allow_check ? "禁用 check()" : "恢复 check()"}
              </Button>
            </div>
          </div>
          {!detail.build_policy.allow_check && (
            <p className="u-muted" style={{ fontSize: 12 }}>该设置只影响之后创建的 Job，需记录理由。</p>
          )}
        </Section>

        <Section title={`Revision (${revisions.length})`}>
          <div className="doctor-list">
            {revisions.map((rev) => (
              <div className="doctor-row" key={rev.id}>
                <div className="doctor-row__name">
                  <StatusBadge meta={statusMeta(rev.state)} />
                  <span>
                    <strong>{rev.upstream_version}</strong>
                    <span className="u-muted"> · commit {shortHash(rev.aur_commit)}</span>
                  </span>
                </div>
                <span className="doctor-row__msg">{rev.published_version ?? "尚未构建"}</span>
              </div>
            ))}
          </div>
        </Section>

        <Section title="依赖解析">
          <div className="doctor-list">
            {detail.dependency_resolution.map((dep) => (
              <div className="doctor-row" key={`${dep.kind}-${dep.name}`}>
                <div className="doctor-row__name">
                  <span className={`pkg-tag ${dep.state === "needs_selection" ? "pkg-tag--direct" : "pkg-tag--implicit"}`}>{dep.kind}</span>
                  <span>{dep.name}</span>
                </div>
                <span className="doctor-row__msg">
                  {dep.state === "needs_selection" ? (
                    <span className="row" style={{ justifyContent: "flex-end", gap: 6 }}>
                      {dep.candidates.map((c) => (
                        <button key={c} className="btn btn--sm" style={{ border: "1px solid var(--accent-border)", color: "var(--accent)", background: "var(--accent-soft)" }} onClick={() => void onProvider(dep.name, c)}>
                          选择 {c}
                        </button>
                      ))}
                    </span>
                  ) : (
                    dep.target_package_base ?? dep.state
                  )}
                </span>
              </div>
            ))}
          </div>
        </Section>

      </div>

      <Confirm
        open={confirmRebuild}
        onClose={() => setConfirmRebuild(false)}
        onConfirm={() => {
          void onRebuild().then(() => setConfirmRebuild(false));
        }}
        title="确认手工重建？"
        body="保持当前已批准 commit 的上游原版本与 pkgrel 重新构建；不会自动升级已安装客户端。"
        confirmLabel="开始重建"
        busy={busyKey === `rebuild-${detail.package_base}`}
      />
    </Drawer>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="stack" style={{ gap: 8 }}>
      <h3>{title}</h3>
      {children}
    </div>
  );
}

function NoticeBlock({ children }: { children: React.ReactNode }) {
  return (
    <div className="notice notice--warning" style={{ margin: 0 }}>
      <IconCheck size={16} />
      <div style={{ fontSize: 12.5 }}>{children}</div>
    </div>
  );
}
