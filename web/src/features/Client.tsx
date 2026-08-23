import { api } from "../lib/api";
import { useAsync } from "../lib/hooks";
import { relativeTime } from "../lib/format";
import { Badge, Button, Card, CardBody, CardHead, CardTitle, PageHead } from "../components/ui";
import { useToast } from "../components/Toast";
import { IconAlert, IconCopy, IconTerminal } from "../components/icons";

export function Client() {
  const toast = useToast();
  const { data, loading, error } = useAsync(() => api.clientBootstrap());

  const copy = async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text);
      toast.success("已复制", `${label} 已写入剪贴板`);
    } catch {
      toast.error("复制失败", "浏览器未授予剪贴板权限");
    }
  };

  if (loading) {
    return (
      <div className="stack">
        <PageHead eyebrow="首次接入" title="客户端" lede="先带外核对完整 GPG 指纹，再安装 keyring 与仓库配置。" />
        <Card><CardBody><div className="stack" style={{ gap: 10 }}>
          <span className="skeleton" style={{ width: "100%", height: 44 }} />
          <span className="skeleton" style={{ width: "60%" }} />
          <span className="skeleton" style={{ width: "100%", height: 120 }} />
        </div></CardBody></Card>
      </div>
    );
  }

  if (error || !data) {
    return (
      <>
        <PageHead eyebrow="首次接入" title="客户端" lede="先带外核对完整 GPG 指纹，再安装 keyring 与仓库配置。" />
        <div className="notice notice--danger" role="alert">
          <IconAlert size={16} />
          <div><b>客户端配置读取失败。</b> {error ?? "接口未返回配置"}</div>
        </div>
      </>
    );
  }

  return (
    <>
      <PageHead
        eyebrow="首次接入"
        title="客户端"
        lede="先带外核对完整 GPG 指纹，再安装 keyring 与仓库配置；客户端不会远程操作，只读取签名仓库。"
      />

      <div className="stack">
        <Card>
          <CardHead>
            <CardTitle eyebrow="完整指纹" title="仓库 GPG 主指纹" sub="通过独立可信渠道人工核对后再导入" />
            <Button size="sm" variant="ghost" icon={<IconCopy size={14} />} onClick={() => void copy(data.gpg_fingerprint, "指纹")}>复制</Button>
          </CardHead>
          <CardBody>
            <div className="fingerprint">
              {data.gpg_fingerprint.split(" ").map((group, i) => (
                <span key={i}>
                  {group}
                  {i === data.gpg_fingerprint.split(" ").length - 1 ? "" : " "}
                </span>
              ))}
            </div>

            <div className="grid grid--3" style={{ marginTop: 16 }}>
              <KeyringStat label="keyring generation" value={`gen ${data.keyring_generation ?? "—"}`} />
              <KeyringStat label="上次发布" value={data.keyring_published_at ? relativeTime(data.keyring_published_at) : "等待首次发布"} />
              <KeyringStat label="下次到期" value={data.keyring_next_due_at ? relativeTime(data.keyring_next_due_at) : "—"} />
            </div>

            {data.warnings.map((w) => (
              <div className="notice notice--warning" key={w} style={{ marginTop: 16 }}>{w}</div>
            ))}
          </CardBody>
        </Card>

        <Card>
          <CardHead>
            <CardTitle eyebrow="pacman.conf" title="仓库配置" />
            <div className="row">
              <Badge tone="accent">SigLevel Required</Badge>
              <Button size="sm" variant="ghost" icon={<IconCopy size={14} />} onClick={() => void copy(data.repository_config, "仓库配置")}>复制</Button>
            </div>
          </CardHead>
          <CardBody>
            <div className="log-box">
              <pre>{data.repository_config}</pre>
            </div>
          </CardBody>
        </Card>

        <Card>
          <CardHead>
            <CardTitle eyebrow="安装步骤" title="首装命令" />
            <Badge tone="info">{data.commands.length} 步</Badge>
          </CardHead>
          <CardBody>
            <div className="stack" style={{ gap: 8 }}>
              {data.commands.map((cmd, i) => (
                <div key={cmd} className="row" style={{ justifyContent: "space-between", background: "var(--surface-muted)", border: "1px solid var(--border)", borderRadius: 10, padding: "10px 12px" }}>
                  <span className="row" style={{ gap: 10, fontFamily: "var(--font-mono)", fontSize: 12.5 }}>
                    <span className="u-muted" style={{ fontFamily: "var(--font-mono)" }}>{i + 1}.</span>
                    <span>{cmd}</span>
                  </span>
                  <Button size="sm" variant="ghost" icon={<IconCopy size={13} />} onClick={() => void copy(cmd, "命令")}>复制</Button>
                </div>
              ))}
            </div>
          </CardBody>
        </Card>

        <div className="notice notice--info">
          <IconTerminal size={16} />
          <div>
            <b>换钥流程必须由管理员主动安排。</b>旧钥泄露时重新进行带外信任；正常换钥保持旧/新钥重叠期，回退 previous 不会静默恢复已不再受当前信任集合接受的仓库。
          </div>
        </div>
      </div>
    </>
  );
}

function KeyringStat({ label, value }: { label: string; value: string }) {
  return (
    <div style={{ border: "1px solid var(--border)", borderRadius: 10, padding: "12px 14px" }}>
      <div className="u-muted" style={{ fontSize: 11, fontWeight: 600, letterSpacing: "0.05em", textTransform: "uppercase" }}>{label}</div>
      <div style={{ fontSize: 15, fontWeight: 650, marginTop: 6, color: "var(--ink)" }}>{value}</div>
    </div>
  );
}
