import { FormEvent, useState } from "react";
import { api } from "../lib/api";
import { useApp } from "../lib/appctx";
import { Button } from "../components/ui";
import { IconBox } from "../components/icons";

const STEPS = ["同步", "审查", "构建", "发布"];

export function Login({ initialError = "" }: { initialError?: string }) {
  const { login } = useApp();
  const [username, setUsername] = useState("");
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
      <div className="login__inner">
        <div className="login__brand-row">
          <div className="sidebar__brand-mark">
            <IconBox size={14} />
          </div>
          <div className="wordmark">AURsmith</div>
        </div>

        <form className="login__card stack" onSubmit={submit}>
          <div style={{ display: "grid", gap: 4 }}>
            <h1>登录</h1>
            <p className="u-muted" style={{ fontSize: 13 }}>
              管理私有 AUR 仓库控制台。
            </p>
          </div>
          <div className="field">
            <label htmlFor="login-user">管理员名称</label>
            <input
              id="login-user"
              className="input"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder="用户名"
              autoComplete="username"
              autoFocus
            />
          </div>
          <div className="field">
            <label htmlFor="login-pass">密码</label>
            <input
              id="login-pass"
              className="input"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder="输入管理员密码"
              autoComplete="current-password"
            />
          </div>
          {error && (
            <div className="notice notice--danger" role="alert">
              {error}
            </div>
          )}
          <Button variant="primary" type="submit" loading={loading}>
            登录
          </Button>
        </form>

        <div className="login__steps" aria-hidden="true">
          {STEPS.map((s) => (
            <span key={s}>{s}</span>
          ))}
        </div>
      </div>
    </div>
  );
}
