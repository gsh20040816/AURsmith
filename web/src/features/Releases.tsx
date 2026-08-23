import { useState } from "react";
import { api } from "../lib/api";
import { usePolling, useStable } from "../lib/hooks";
import { shortHash, dateTime, relativeTime, formatCount } from "../lib/format";
import { type Release } from "../lib/types";
import { Badge, Button, Card, CardBody, CardHead, CardTitle, Empty, PageHead, StatusBadge } from "../components/ui";
import { Confirm } from "../components/Overlay";
import { useToast } from "../components/Toast";
import { IconAlert, IconRefresh, IconRocket } from "../components/icons";
import { releaseState } from "../lib/status";

export function Releases() {
  const toast = useToast();
  const [rollbackTarget, setRollbackTarget] = useState<Release | null>(null);
  const [busy, setBusy] = useState("");

  const load = useStable(() => api.releases());
  const releases = usePolling(load, 10000);
  const list = releases.data?.items ?? [];

  const current = list.find((r) => r.position === "current");
  const previous = list.find((r) => r.position === "previous");
  const failed = list.find((r) => r.position === "failed");

  const rollback = async (release: Release) => {
    setBusy(release.id);
    try {
      const res = await api.rollbackRelease(release.id);
      releases.reload();
      toast.success("服务端已恢复 previous", `已恢复到 ${shortHash(res.release_id, 10)}；客户端不会自动降级。`);
    } catch (reason) {
      toast.error("回退失败", reason instanceof Error ? reason.message : undefined);
    } finally {
      setBusy("");
    }
  };

  const retry = async (release: Release) => {
    setBusy(release.id);
    try {
      await api.retryRelease(release.id);
      releases.reload();
      toast.success("已重试发布", "修复后重新执行签名与 repo-add。");
    } catch (reason) {
      toast.error("重试失败", reason instanceof Error ? reason.message : undefined);
    } finally {
      setBusy("");
    }
  };

  return (
    <>
      <PageHead
        eyebrow="current / previous"
        title="发布"
        lede="Publisher 在 staging 中签名并生成仓库数据库，校验完成后原子切换；只保留 current 和 previous。"
        actions={<Button variant="ghost" onClick={releases.reload} icon={<IconRefresh size={15} />}>刷新</Button>}
      />

      <div className="stack">
        {releases.error && (
          <div className="notice notice--danger" role="alert">
            <IconAlert size={16} />
            <div><b>发布列表读取失败。</b> {releases.error}</div>
          </div>
        )}

        <div className="release-positions">
          <ReleaseCard position="current" release={current} />
          <ReleaseCard position="previous" release={previous} failed={failed} />
        </div>

        {failed && (
          <div className="notice notice--warning" style={{ margin: 0 }}>
            <IconRocket size={16} />
            <div style={{ flex: 1 }}>
              <b>上一次发布失败，current 未受影响。</b>
              <div style={{ fontSize: 12.5 }}>{failed.last_error}</div>
            </div>
            <Button size="sm" variant="danger" loading={busy === failed.id} onClick={() => void retry(failed)}>修复后重试</Button>
          </div>
        )}

        <Card>
          <CardHead>
            <CardTitle eyebrow="发布历史" title="所有 Release" />
            <Badge tone="neutral">{list.length} 条</Badge>
          </CardHead>
          <CardBody flush>
            {list.length === 0 ? (
              <Empty title="尚无发布" detail="批准的构建产物完成签名和 repo-add 后会出现在这里。" glyph={<IconRocket size={22} />} />
            ) : (
              <div className="tbl-wrap">
                <table className="tbl release-table">
                  <thead>
                    <tr>
                      <th>Release</th>
                      <th>位置 / 状态</th>
                      <th>包</th>
                      <th>Manifest</th>
                      <th>时间</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {list.map((r) => (
                      <tr key={r.id}>
                        <td>
                          <div className="tbl__cell-main">
                            <strong className="u-mono">{shortHash(r.id, 6)}</strong>
                            <span className="tbl__sub">{r.position ?? "—"}</span>
                          </div>
                        </td>
                        <td><StatusBadge meta={releaseState(r)} /></td>
                        <td><code className="inline">{formatCount(r.artifact_count)}</code></td>
                        <td><code className="inline hash">{shortHash(r.manifest_sha256, 16)}</code></td>
                        <td>
                          <div className="tbl__cell-main">
                            <span className="tbl__sub">{relativeTime(r.committed_at ?? r.created_at)}</span>
                            {r.committed_at && <span className="tbl__sub">{dateTime(r.committed_at)}</span>}
                          </div>
                        </td>
                        <td>
                          {r.position === "previous" && r.state === "committed" && (
                            <Button size="sm" variant="ghost" className="is-danger" disabled={busy === r.id} onClick={() => setRollbackTarget(r)}>恢复 previous</Button>
                          )}
                          {r.position === "failed" && (
                            <Button size="sm" variant="ghost" disabled={busy === r.id} onClick={() => void retry(r)}>重试发布</Button>
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
      </div>

      <Confirm
        open={!!rollbackTarget}
        onClose={() => setRollbackTarget(null)}
        onConfirm={() => {
          if (!rollbackTarget) return;
          void rollback(rollbackTarget).then(() => setRollbackTarget(null));
        }}
        danger
        title="确认恢复 previous？"
        body="将服务端仓库恢复到上一版本。已安装的客户端不会被自动降级；若 keyring 或签名指纹不兼容，回退会失败并给出明确错误。"
        confirmLabel="恢复 previous"
        busy={busy === rollbackTarget?.id}
      />
    </>
  );
}

function ReleaseCard({ position, release, failed }: { position: "current" | "previous"; release?: Release; failed?: Release }) {
  return (
    <div className={`release-card ${position === "current" ? "release-card--current" : "release-card--prev"}`}>
      <div className="release-card__label">
        <strong>{position === "current" ? "当前仓库" : "上一版本"}</strong>
        {release ? <StatusBadge meta={releaseState(release)} /> : <Badge tone={failed ? "danger" : "neutral"}>{failed ? "发布失败" : "暂无"}</Badge>}
      </div>
      {release ? (
        <div className="release-card__meta">
          <div className="row" style={{ justifyContent: "space-between" }}>
            <span>包数量</span>
            <code className="inline">{formatCount(release.artifact_count)}</code>
          </div>
          <div className="row" style={{ justifyContent: "space-between" }}>
            <span>Manifest</span>
            <code className="inline hash">{shortHash(release.manifest_sha256, 16)}</code>
          </div>
          <div className="row" style={{ justifyContent: "space-between" }}>
            <span>提交时间</span>
            <span>{dateTime(release.committed_at)}</span>
          </div>
        </div>
      ) : failed ? (
        <p className="u-secondary" style={{ fontSize: 12.5, lineHeight: 1.6 }}>{failed.last_error}</p>
      ) : (
        <p className="u-muted" style={{ fontSize: 12.5 }}>尚无此位置的发布。</p>
      )}
    </div>
  );
}
