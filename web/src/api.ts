export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string
  ) {
    super(message);
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    ...init,
    credentials: "same-origin",
    headers: {
      "Content-Type": "application/json",
      ...init?.headers
    }
  });
  if (!response.ok) {
    const body = await response.json().catch(() => ({
      code: "HTTP_ERROR",
      message: `请求失败：HTTP ${response.status}`
    }));
    throw new ApiError(response.status, body.code, body.message);
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return response.json() as Promise<T>;
}

export type Session = { id: string; username: string };
export type Requirement = { id: string; title: string };
export type Worker = {
  id: string;
  name: string;
  role: "builder" | "publisher" | "archiver";
  state: "online" | "draining" | "offline" | "degraded" | "incompatible";
  endpoint: string;
  protocol_version: number;
  labels: string[];
  last_seen_at: string | null;
};
export type Job = {
  id: string;
  required_role: Worker["role"];
  status: string;
  priority: number;
  failure_code: string | null;
  revision_sha256: string | null;
  worker_name: string | null;
  created_at: string;
  updated_at: string;
};
export const api = {
  setupStatus: () => request<{ initialized: boolean }>("/api/v1/setup/status"),
  setup: (input: { token: string; username: string; password: string }) =>
    request<{ initialized: boolean }>("/api/v1/setup", {
      method: "POST",
      body: JSON.stringify(input)
    }),
  login: (input: { username: string; password: string }) =>
    request<{ username: string }>("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify(input)
    }),
  logout: () => request<void>("/api/v1/auth/logout", { method: "POST" }),
  me: () => request<Session>("/api/v1/auth/me"),
  requirements: () => request<{ items: Requirement[] }>("/api/v1/requirements"),
  workers: () => request<{ items: Worker[] }>("/api/v1/workers"),
  jobs: () => request<{ items: Job[] }>("/api/v1/jobs"),
  probeWorker: (id: string) =>
    request<{ id: string; state: string }>(`/api/v1/workers/${id}/probe`, {
      method: "POST"
    }),
  drainWorker: (id: string) =>
    request<{ id: string; state: string }>(`/api/v1/workers/${id}/drain`, {
      method: "POST"
    })
};
