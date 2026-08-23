import { FormEvent, useState } from "react";
import { api } from "../lib/api";
import { useApp } from "../lib/appctx";
import { Button } from "../components/ui";
import { IconBox, IconCheck, IconChevronRight } from "../components/icons";

const STAGES = [
  { name: "同步", detail: "固定 AUR commit" },
  { name: "审查", detail: "diff-first 3+1" },
  { name: "构建", detail: "隔离 Docker" },
  { name: "发布", detail: "GPG 原子切换" }
];

export function Login({ initialError = "" }: { initialError?: string }) {
  const { login } = useApp();
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(initialError);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setError("");
    setLoading(true);
    try {
      await api.login({ username, password });
      login(await api.me());
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "登录失败");
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="login">
      <div className="login__brand">
        <div className="login__brand-head">
          <div className="sidebar__brand-mark">
            <IconBox size={16} />
          </div>
          <div className="wordmark">AURsmith</div>
        </div>
        <div className="login__hero">
          <h1>每一个包，<br />先审查再安装。</h1>
          <p>私人 AUR 二进制仓库：同步 → 三遍独立审查 → 家庭 Builder 联网构建 → GPG 签名发布。只服务一位管理员与少量 Arch 客户端。</p>
        </div>
        <div className="login__pipeline">
          <h3 style={{ color: "var(--success)", marginBottom: 14 }}>流程</h3>
          <div className="pipeline" style={{ padding: 0 }}>
            {STAGES.map((s, i) => (
              <div className="pipe-stage is-active" key={s.name}>
                <div className="pipe-stage__node">
                  {i < STAGES.length - 1 ? <IconCheck size={14} /> : <IconChevronRight size={14} />}
                </div>
                <div className="pipe-stage__name">{s.name}</div>
                <div className="pipe-stage__detail">{s.detail}</div>
              </div>
            ))}
          </div>
        </div>
      </div>

      <div className="login__form">
        <form className="login__card stack" onSubmit={submit}>
          <div style={{ display: "grid", gap: 4 }}>
            <h1 style={{ fontSize: 24 }}>回到控制台</h1>
            <p className="u-secondary" style={{ fontSize: 13 }}>登录只管理私有仓库，不会远程操作 Arch 客户端。</p>
          </div>
          <div className="field">
            <label htmlFor="login-user">管理员名称</label>
            <input id="login-user" className="input" value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" />
          </div>
          <div className="field">
            <label htmlFor="login-pass">密码</label>
            <input id="login-pass" className="input" type="password" value={password} onChange={(e) => setPassword(e.target.value)} placeholder="输入管理员密码" autoComplete="current-password" />
          </div>
          {error && <div className="notice notice--danger" role="alert">{error}</div>}
          <Button variant="primary" type="submit" loading={loading}>
            登录
          </Button>
        </form>
      </div>
    </div>
  );
}
