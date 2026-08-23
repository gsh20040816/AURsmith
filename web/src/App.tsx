import { BrowserRouter } from "react-router-dom";
import { AppProvider, useApp } from "./lib/appctx";
import { ToastProvider } from "./components/Toast";
import { Shell } from "./components/Shell";
import { Login } from "./features/Login";
import { IconBox } from "./components/icons";

function Root() {
  const { session, booted, bootError } = useApp();

  if (!booted) {
    return (
      <div className="auth-page-splash">
        <div className="sidebar__brand-mark">
          <IconBox size={18} />
        </div>
        <span className="skeleton" style={{ width: 160 }} />
        <p className="u-muted">正在读取控制面状态…</p>
      </div>
    );
  }

  if (!session) return <Login initialError={bootError} />;

  return <Shell />;
}

export function App() {
  return (
    <BrowserRouter>
      <AppProvider>
        <ToastProvider>
          <Root />
        </ToastProvider>
      </AppProvider>
    </BrowserRouter>
  );
}
