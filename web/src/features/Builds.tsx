import { useState } from "react";
import { api } from "../lib/api";
import { usePolling, useStable, useTicker } from "../lib/hooks";
import { shortHash, relativeTime } from "../lib/format";
import { statusMeta } from "../lib/status";
import type { Build, BuildLog } from "../lib/types";
import { Badge, Button, Card, CardBody, CardHead, CardTitle, Empty, PageHead, Stat, StatusBadge } from "../components/ui";
import { Drawer } from "../components/Overlay";
import { useToast } from "../components/Toast";
import { IconAlert, IconHammer, IconLog, IconRefresh } from "../components/icons";

const ERROR_CLASS: Record<string, string> = {
  transient: "瞬态，自动重试",
  deterministic: "确定性失败",
  config: "配置问题"
};

export function Builds() {
  const toast = useToast();
  const [logBuild, setLogBuild] = useState<Build | null>(null);
  const [log, setLog] = useState<BuildLog | null>(null);
  const [logError, setLogError] = useState("");

  useTicker(1000);
  const load = useStable(() => api.builds());
  const builds = usePolling(load, 5000);

  const list = builds.data?.items ?? [];
  const running = list.filter((b) => b.state === "running" || b.state === "uploading").length;
  const queued = list.filter((b) => b.state === "queued").length;
  const failed = list.filter((b) => b.state === "failed").length;

  const openLog = async (build: Build) => {
    if (!build.has_log) return;
    setLogBuild(build);
    setLog(null);
    setLogError("");
    try {
      setLog(await api.buildLog(build.id));
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : "无法读取构建日志";
      setLogError(message);
      toast.error("加载日志失败", message);
    }
  };

  const copyLog = async () => {
    if (!log) return;
    try {
      await navigator.clipboard.writeText(log.log);
      toast.success("已复制日志");
    } catch (reason) {
      toast.error("复制失败", reason instanceof Error ? reason.message : "浏览器未授予剪贴板权限");
    }
  };

  return (
    <>
      <PageHead
        title="构建"
        lede="家用 Builder 通过 HTTPS 领取构建、分片上传产物；瞬态失败自动重试两次。"
        actions={<Button variant="ghost" onClick={builds.reload} icon={<IconRefresh size={15} />}>刷新</Button>}
      />

      <div className="grid grid--stats-4" style={{ marginBottom: 16 }}>
        <Stat label="构建 / 上传中" value={running} icon={<IconHammer size={17} />} foot="已被 Builder 领取" />
        <Stat label="排队中" value={queued} icon={<IconHammer size={17} />} foot="等待 Builder 或依赖" />
        <Stat label="失败" value={failed} icon={<IconHammer size={17} />} foot="最近 200 条中" />
        <Stat label="合计" value={list.length} icon={<IconHammer size={17} />} foot="最近 200 条" />
      </div>

      {builds.error && (
        <div className="notice notice--danger" role="alert" style={{ marginBottom: 16 }}>
          <IconAlert size={16} />
          <div><b>构建列表读取失败。</b> {builds.error}</div>
        </div>
      )}

      <Card>
        <CardHead>
          <CardTitle title="构建" sub="每个已批准 revision 一行；重试复用同一行" />
          <Button size="sm" variant="ghost" onClick={builds.reload} icon={<IconRefresh size={14} />}>刷新</Button>
        </CardHead>
        <CardBody flush>
          {list.length === 0 ? (
            <Empty title="没有构建" detail="revision 获批后会自动排队，由家用 Builder 领取。" />
          ) : (
            <div className="tbl-wrap">
              <table className="tbl tbl--clickable build-table">
                <thead>
                  <tr>
                    <th>软件包</th>
                    <th>状态 / 尝试</th>
                    <th>Revision</th>
                    <th>Builder</th>
                    <th>时间</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {list.map((build) => (
                    <tr key={build.id} onClick={() => void openLog(build)}>
                      <td>
                        <div className="tbl__cell-main">
                          <strong>{build.package_base}</strong>
                          <span className="tbl__sub">
                            {build.version}
                            {build.reason === "manual" ? " · 手工重建" : ""}
                          </span>
                        </div>
                      </td>
                      <td>
                        <div className="tbl__cell-main">
                          <StatusBadge meta={statusMeta(build.state)} />
                          <span className="tbl__sub">
                            第 {build.attempt} 次
                            {build.error_code && ` · ${build.error_code}`}
                            {build.error_class && `（${ERROR_CLASS[build.error_class] ?? build.error_class}）`}
                          </span>
                          {build.waiting_reason && <span className="tbl__sub">{build.waiting_reason}</span>}
                        </div>
                      </td>
                      <td>
                        <code className="inline hash">{shortHash(build.aur_commit, 10)}</code>
                      </td>
                      <td><code className="inline">{build.builder_id ?? "—"}</code></td>
                      <td>
                        <div className="tbl__cell-main">
                          <span className="tbl__sub">{relativeTime(build.finished_at ?? build.started_at ?? build.created_at)}</span>
                          {build.artifacts && <span className="tbl__sub">{build.artifacts.length} 个产物</span>}
                        </div>
                      </td>
                      <td onClick={(e) => e.stopPropagation()}>
                        {build.has_log ? (
                          <Button size="sm" variant="ghost" icon={<IconLog size={14} />} onClick={() => void openLog(build)}>日志</Button>
                        ) : (
                          <span className="u-muted" style={{ fontSize: 12 }}>—</span>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </CardBody>
      </Card>

      <Drawer
        open={!!logBuild}
        onClose={() => setLogBuild(null)}
        title={logBuild ? `构建日志 · ${logBuild.package_base} · ${shortHash(logBuild.id, 6)}` : ""}
      >
        {logError ? (
          <div className="notice notice--danger" role="alert">
            <IconAlert size={16} />
            <div>{logError}</div>
          </div>
        ) : !log ? (
          <div className="empty" style={{ padding: 32 }}><span className="spinner" style={{ width: 18, height: 18 }} /></div>
        ) : (
          <div className="stack">
            <div className="row" style={{ justifyContent: "space-between" }}>
              <div className="row" style={{ gap: 6 }}>
                <StatusBadge meta={statusMeta(log.state)} />
                {log.error_code && <Badge tone="danger">{log.error_code}</Badge>}
              </div>
              <Button size="sm" variant="ghost" onClick={() => void copyLog()}>复制</Button>
            </div>
            <div className="log-box">
              <pre>{renderLog(log.log)}</pre>
            </div>
            <p className="u-muted" style={{ fontSize: 12 }}>日志由 Builder 回报，只保留末尾 128 KiB。</p>
          </div>
        )}
      </Drawer>
    </>
  );
}

function renderLog(text: string) {
  return text.split("\n").map((line, i) => {
    const lower = line.toLowerCase();
    const cls = line.startsWith("==>") ? "log-time" : lower.includes("error") || line.includes("FAIL") ? "log-err" : "";
    return <span key={i} className={cls}>{line}{"\n"}</span>;
  });
}
