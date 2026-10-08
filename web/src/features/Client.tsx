import { api } from "../lib/api";
import { useAsync } from "../lib/hooks";
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
        <PageHead title="客户端" lede="先带外核对 GPG 指纹，再安装 keyring 与仓库配置。" />
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
        <PageHead title="客户端" lede="先带外核对 GPG 指纹，再安装 keyring 与仓库配置。" />
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
        title="客户端"
        lede="先带外核对 GPG 指纹，再安装 keyring；客户端只读取签名仓库。"
      />

      <div className="stack">
        <Card>
          <CardHead>
            <CardTitle title="GPG 主指纹" sub="带外核对后再导入" />
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

            <div className="row" style={{ marginTop: 16, gap: 8 }}>
              <span className="u-muted" style={{ fontSize: 12.5 }}>公钥地址</span>
              <code className="inline">{data.gpg_key_url}</code>
            </div>

            {data.warnings.map((w) => (
              <div className="notice notice--warning" key={w} style={{ marginTop: 16 }}>{w}</div>
            ))}
          </CardBody>
        </Card>

        <Card>
          <CardHead>
            <CardTitle title="仓库配置" />
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
            <CardTitle title="首装命令" />
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
            <b>签名私钥只存在于无网络的签名器中。</b>aursmith-keyring 只在签名密钥更换时重建；换钥需要重新进行带外指纹核对。
          </div>
        </div>
      </div>
    </>
  );
}
