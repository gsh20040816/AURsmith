import { createContext, ReactNode, useContext, useEffect, useState } from "react";
import { api, ApiError } from "./api";
import type { Session } from "./types";

type AppState = {
  session: Session | null;
  booted: boolean;
  bootError: string;
  login: (session: Session) => void;
  logout: () => Promise<void>;
};

const Ctx = createContext<AppState | null>(null);

export function AppProvider({ children }: { children: ReactNode }) {
  const [session, setSession] = useState<Session | null>(null);
  const [booted, setBooted] = useState(false);
  const [bootError, setBootError] = useState("");

  useEffect(() => {
    let alive = true;
    void api
      .me()
      .then((s) => {
        if (!alive) return;
        setSession(s);
        setBooted(true);
      })
      .catch((reason) => {
        if (!alive) return;
        if (!(reason instanceof ApiError && reason.status === 401)) {
          setBootError(reason instanceof Error ? reason.message : "读取会话失败");
        }
        setBooted(true);
      });
    return () => {
      alive = false;
    };
  }, []);

  const value: AppState = {
    session,
    booted,
    bootError,
    login: (current) => {
      setBootError("");
      setSession(current);
    },
    logout: async () => {
      try {
        await api.logout();
      } catch (reason) {
        if (!(reason instanceof ApiError && reason.status === 401)) throw reason;
      }
      setSession(null);
    }
  };

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useApp(): AppState {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useApp must be used inside <AppProvider>");
  return ctx;
}
