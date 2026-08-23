import { useState } from "react";
import { api } from "../lib/api";
import { usePolling, useStable, useTicker } from "../lib/hooks";
import { shortHash, relativeTime, formatCount } from "../lib/format";
import { jobState } from "../lib/status";
import { type Job, type LogDocument } from "../lib/types";
import { Badge, Button, Card, CardBody, CardHead, CardTitle, Empty, PageHead, Stat, StatusBadge } from "../components/ui";
import { Drawer } from "../components/Overlay";
import { useToast } from "../components/Toast";
import { IconAlert, IconHammer, IconLog, IconRefresh } from "../components/icons";

export function Builds() {
  const toast = useToast();
  const [logJob, setLogJob] = useState<Job | null>(null);
  const [logs, setLogs] = useState<LogDocument | null>(null);
  const [logError, setLogError] = useState("");

  useTicker(1000);
  const load = useStable(() => api.jobs());
  const jobs = usePolling(load, 5000);

  const list = jobs.data?.items ?? [];
  const running = list.filter((j) => j.status === "running").length;
  const queued = list.filter((j) => j.status === "queued").length;
  const failed = list.filter((j) => j.status === "failed").length;

  const openLogs = async (job: Job) => {
    if (!job.has_logs) return;
    setLogJob(job);
    setLogs(null);
    setLogError("");
    try {
      setLogs(await api.jobLogs(job.id));
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : "无法读取构建日志";
      setLogError(message);
      toast.error("加载日志失败", message);
    }
  };

  const copyLogs = async () => {
    if (!logs) return;
    const text = logs.document.logs
      .map((log) => `## ${log.path}\n${log.content_utf8 ?? log.omitted_reason ?? "[非 UTF-8 内容]"}`)
      .join("\n\n");
    try {
      await navigator.clipboard.writeText(text);
      toast.success("已复制日志");
    } catch (reason) {
      toast.error("复制失败", reason instanceof Error ? reason.message : "浏览器未授予剪贴板权限");
    }
  };

  return (
    <>
      <PageHead
        eyebrow="固定 Builder"
        title="构建任务"
        lede="只显示 Build、attempt、有限重试、最后错误和有界日志。构建在一个一次性联网 Docker 容器中完成，产物经 write-only rrsync 推送。"
        actions={<Button variant="ghost" onClick={jobs.reload} icon={<IconRefresh size={15} />}>刷新</Button>}
      />

      <div className="grid grid--stats" style={{ marginBottom: 16 }}>
        <Stat label="构建中" value={running} icon={<IconHammer size={17} />} foot="运行中的 attempt" />
        <Stat label="排队中" value={queued} icon={<IconHammer size={17} />} foot="等待空槽位" />
        <Stat label="失败" value={failed} icon={<IconHammer size={17} />} foot="需人工处置" />
        <Stat label="Builder" value="固定" icon={<IconHammer size={17} />} foot="由 Controller 调度" />
      </div>

      {jobs.error && (
        <div className="notice notice--danger" role="alert" style={{ marginBottom: 16 }}>
          <IconAlert size={16} />
          <div><b>构建任务读取失败。</b> {jobs.error}</div>
        </div>
      )}

      <Card>
        <CardHead>
          <CardTitle eyebrow="Job 队列" title="任务" sub="按优先级领取，全局并发槽位控制" />
          <div className="row">
            <Badge tone="info">{list.length} 总任务</Badge>
            <Button size="sm" variant="ghost" onClick={jobs.reload} icon={<IconRefresh size={14} />}>刷新</Button>
          </div>
        </CardHead>
        <CardBody flush>
          {list.length === 0 ? (
            <Empty title="没有构建任务" detail="加入软件包并批准审查后，Builder 会从这里领取任务。" />
          ) : (
            <div className="tbl-wrap">
              <table className="tbl tbl--clickable">
                <thead>
                  <tr>
                    <th>任务</th>
                    <th>状态 / Attempt</th>
                    <th>优先级</th>
                    <th>Revision</th>
                    <th>日志证据</th>
                    <th>更新</th>
                    <th style={{ width: 120 }}></th>
                  </tr>
                </thead>
                <tbody>
                  {list.map((job) => {
                    const meta = jobState(job);
                    return (
                      <tr key={job.id} onClick={() => void openLogs(job)}>
                        <td>
                          <div className="tbl__cell-main">
                            <strong className="u-mono">{shortHash(job.id, 6)}</strong>
                            <span className="tbl__sub">
                              {job.status === "failed" ? job.failure_code ?? "失败" : "x86_64 · 固定 Builder"}
                            </span>
                          </div>
                        </td>
                        <td>
                          <div className="tbl__cell-main">
                            <StatusBadge meta={meta} />
                            {job.next_attempt_at && <span className="tbl__sub">下次重试 {relativeTime(job.next_attempt_at)}</span>}
                          </div>
                        </td>
                        <td><code className="inline">{job.priority}</code></td>
                        <td>
                          <code className="inline hash">{job.revision_sha256 ? shortHash(job.revision_sha256, 12) : "—"}</code>
                        </td>
                        <td><code className="inline">{job.has_logs ? "有界日志" : "—"}</code></td>
                        <td>
                          <div className="tbl__cell-main">
                            <span className="tbl__sub">{relativeTime(job.updated_at)}</span>
                            <span className="tbl__sub">尝试 {job.attempt_count}</span>
                          </div>
                        </td>
                        <td onClick={(e) => e.stopPropagation()}>
                          {job.has_logs ? (
                            <Button size="sm" variant="ghost" icon={<IconLog size={14} />} onClick={() => void openLogs(job)}>日志</Button>
                          ) : (
                            <span className="u-muted" style={{ fontSize: 12 }}>—</span>
                          )}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </CardBody>
      </Card>

      <Drawer
        open={!!logJob}
        onClose={() => setLogJob(null)}
        title={logJob ? `构建日志 · ${shortHash(logJob.id, 6)}` : ""}
        eyebrow="有界构建日志"
      >
        {logError ? (
          <div className="notice notice--danger" role="alert">
            <IconAlert size={16} />
            <div>{logError}</div>
          </div>
        ) : !logs ? (
          <div className="empty" style={{ padding: 32 }}><span className="spinner" style={{ width: 18, height: 18 }} /></div>
        ) : (
          <div className="stack">
            <div className="row" style={{ justifyContent: "space-between" }}>
              <div className="row" style={{ gap: 8 }}>
                <Badge tone="info">manifest</Badge>
                <code className="inline hash">{shortHash(logs.sha256, 16)}</code>
              </div>
              <Button size="sm" variant="ghost" onClick={() => void copyLogs()}>
                复制
              </Button>
            </div>

            <div className="row" style={{ gap: 6 }}>
              <Badge tone={logs.document.status === "succeeded" ? "success" : "danger"}>{logs.document.status}</Badge>
              {logs.document.failure_code && <code className="inline">{logs.document.failure_code}</code>}
            </div>

            <div className="log-grid">
              {logs.document.logs.map((log) => (
                <div className="log-box" key={log.path}>
                  <div className="row" style={{ justifyContent: "space-between", padding: "8px 10px", borderBottom: "1px solid var(--border)" }}>
                    <code className="inline">{log.path}</code>
                    <span className="u-muted" style={{ fontSize: 11 }}>{formatCount(log.size)} B{log.truncated ? " · 已截断" : ""}</span>
                  </div>
                  <pre>{renderLog(linesOf(log.content_utf8 ?? log.omitted_reason ?? "[非 UTF-8 内容，仅保留摘要]"))}</pre>
                </div>
              ))}
            </div>
            <p className="u-muted" style={{ fontSize: 12 }}>
              Builder 完成核对后立即删除容器、输入、输出与临时工作目录；此处仅保留最小终态与有界日志。
            </p>
          </div>
        )}
      </Drawer>
    </>
  );
}

function linesOf(text: string): string[] {
  return text.split("\n");
}

function renderLog(lines: string[]) {
  return lines.map((line, i) => {
    const cls = line.includes("✓") ? "log-ok" : line.includes("error") || line.includes("FAIL") ? "log-err" : line.match(/^\S+ \S+/) ? "log-time" : "";
    return <span key={i} className={cls}>{line}{"\n"}</span>;
  });
}
