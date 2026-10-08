import { api } from "../lib/api";
import { usePolling, useStable } from "../lib/hooks";
import { shortHash, dateTime, relativeTime, formatCount } from "../lib/format";
import { statusMeta } from "../lib/status";
import type { Publication } from "../lib/types";
import { Badge, Button, Card, CardBody, CardHead, CardTitle, Empty, PageHead, StatusBadge } from "../components/ui";
import { IconAlert, IconRefresh, IconRocket } from "../components/icons";

export function Publications() {
  const load = useStable(() => api.publications());
  const publications = usePolling(load, 10000);
  const list = publications.data?.items ?? [];
  const desired = publications.data?.desired;
  const current = list.find((p) => p.current);
  const latest = list[0];
  const converged = !!current && current.plan_sha256 === desired?.plan_sha256;

  return (
    <>
      <PageHead
        title="发布"
        lede="期望状态 = 所有已批准且已构建的包；与当前发布不同且没有进行中的相关构建时，交给无网络签名器原子切换。"
        actions={<Button variant="ghost" onClick={publications.reload} icon={<IconRefresh size={15} />}>刷新</Button>}
      />

      <div className="stack">
        {publications.error && (
          <div className="notice notice--danger" role="alert">
            <IconAlert size={16} />
            <div><b>发布列表读取失败。</b> {publications.error}</div>
          </div>
        )}

        <div className="publication-positions">
          <div className="publication-card publication-card--current">
            <div className="publication-card__label">
              <strong>当前仓库</strong>
              {current ? <StatusBadge meta={statusMeta("published")} /> : <Badge tone="neutral">暂无</Badge>}
            </div>
            {current ? (
              <div className="publication-card__meta">
                <Row label="包数量" value={<code className="inline">{formatCount(current.artifacts.length)}</code>} />
                <Row label="Plan" value={<code className="inline hash">{shortHash(current.plan_sha256, 16)}</code>} />
                <Row label="Manifest" value={<code className="inline hash">{shortHash(current.manifest_sha256, 16)}</code>} />
                <Row label="发布时间" value={<span>{dateTime(current.finished_at)}</span>} />
              </div>
            ) : (
              <p className="u-muted" style={{ fontSize: 12.5 }}>尚无成功发布。</p>
            )}
          </div>
          <div className="publication-card publication-card--prev">
            <div className="publication-card__label">
              <strong>期望状态</strong>
              <Badge tone={converged ? "success" : "info"}>{converged ? "已收敛" : "待收敛"}</Badge>
            </div>
            {desired ? (
              <div className="publication-card__meta">
                <Row label="包数量" value={<code className="inline">{formatCount(desired.artifact_count)}</code>} />
                <Row label="Plan" value={<code className="inline hash">{shortHash(desired.plan_sha256, 16)}</code>} />
                <Row label="暂缓" value={<span>{desired.withheld.length ? desired.withheld.join(", ") : "无"}</span>} />
              </div>
            ) : (
              <p className="u-muted" style={{ fontSize: 12.5 }}>读取中…</p>
            )}
          </div>
        </div>

        {latest?.state === "failed" && (
          <div className="notice notice--warning" style={{ margin: 0 }}>
            <IconRocket size={16} />
            <div style={{ flex: 1 }}>
              <b>最近一次发布失败，当前仓库未受影响。</b>
              <div style={{ fontSize: 12.5 }}>{latest.error}</div>
              <div style={{ fontSize: 12.5 }}>相同期望状态会在 15 分钟后自动重试；期望状态变化时立即重新发布。</div>
            </div>
          </div>
        )}

        <Card>
          <CardHead>
            <CardTitle title="发布历史" sub="紧急回滚：在签名器上运行 aursmithd signer --rollback" />
            <Badge tone="neutral">{list.length} 条</Badge>
          </CardHead>
          <CardBody flush>
            {list.length === 0 ? (
              <Empty title="尚无发布" detail="第一个构建成功后，签名器会生成签名仓库。" glyph={<IconRocket size={22} />} />
            ) : (
              <div className="tbl-wrap">
                <table className="tbl publication-table">
                  <thead>
                    <tr>
                      <th>Plan</th>
                      <th>状态</th>
                      <th>包</th>
                      <th>Manifest</th>
                      <th>时间</th>
                    </tr>
                  </thead>
                  <tbody>
                    {list.map((p: Publication) => (
                      <tr key={p.id}>
                        <td>
                          <div className="tbl__cell-main">
                            <strong className="u-mono">{shortHash(p.plan_sha256, 10)}</strong>
                            <span className="tbl__sub">{p.current ? "current" : p.error ?? "—"}</span>
                          </div>
                        </td>
                        <td><StatusBadge meta={statusMeta(p.state)} /></td>
                        <td><code className="inline">{formatCount(p.artifacts.length)}</code></td>
                        <td><code className="inline hash">{shortHash(p.manifest_sha256, 16)}</code></td>
                        <td>
                          <div className="tbl__cell-main">
                            <span className="tbl__sub">{relativeTime(p.finished_at ?? p.created_at)}</span>
                            <span className="tbl__sub">{dateTime(p.finished_at ?? p.created_at)}</span>
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
    </>
  );
}

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="row" style={{ justifyContent: "space-between" }}>
      <span>{label}</span>
      {value}
    </div>
  );
}
