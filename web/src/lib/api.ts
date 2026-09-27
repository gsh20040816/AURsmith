import type { Audit, AurPackage, ClientBootstrap, Doctor, Job, LogDocument, PackageDetail, Release, Session, Subscription } from "./types";

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
  const method = (init?.method ?? "GET").toUpperCase();
  const headers = new Headers(init?.headers);
  headers.set("Content-Type", "application/json");
  if (method !== "GET" && method !== "HEAD") {
    headers.set("X-AURsmith-CSRF", "1");
  }
  const response = await fetch(path, {
    ...init,
    credentials: "same-origin",
    headers
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

export const api = {
  login: (input: { username: string; password: string }) =>
    request<{ username: string }>("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify(input)
    }),
  logout: () => request<void>("/api/v1/auth/logout", { method: "POST" }),
  me: () => request<Session>("/api/v1/auth/me"),
  jobs: () => request<{ items: Job[] }>("/api/v1/jobs"),
  jobLogs: (id: string) =>
    request<LogDocument>(`/api/v1/jobs/${encodeURIComponent(id)}/logs`),
  searchAur: (query: string) =>
    request<{ items: AurPackage[] }>(`/api/v1/aur/search?q=${encodeURIComponent(query)}`),
  subscriptions: () => request<{ items: Subscription[] }>("/api/v1/subscriptions"),
  audits: () => request<{ items: Audit[] }>("/api/v1/audits"),
  releases: () => request<{ items: Release[] }>("/api/v1/releases"),
  rollbackRelease: (id: string) => request<{
    release_id: string;
    server_rolled_back: boolean;
    client_auto_downgrade: false;
  }>(`/api/v1/releases/${encodeURIComponent(id)}/rollback`, { method: "POST" }),
  retryRelease: (id: string) => request<{
    failed_release_id: string;
    release_id: string;
    state: "issued";
  }>(`/api/v1/releases/${encodeURIComponent(id)}/retry`, { method: "POST" }),
  clientBootstrap: () => request<ClientBootstrap>("/api/v1/client-bootstrap"),
  doctor: () => request<Doctor>("/api/v1/doctor"),
  decideAudit: (bundle: string, approve: boolean, rationale: string) =>
    request<{ bundle_sha256: string; decision: string }>(
      `/api/v1/audits/${encodeURIComponent(bundle)}/manual-decision`,
      { method: "POST", body: JSON.stringify({ approve, rationale }) }
    ),
  retryAudit: (bundle: string) =>
    request<{ bundle_sha256: string; state: string }>(
      `/api/v1/audits/${encodeURIComponent(bundle)}/retry`,
      { method: "POST" }
    ),
  subscribe: (packageName: string) =>
    request<{ package_base: string; revision_id: string; batch_id: string | null; batch_state: string }>(
      "/api/v1/subscriptions",
      { method: "POST", body: JSON.stringify({ package_name: packageName }) }
    ),
  deleteSubscription: (packageBase: string) =>
    request<{ package_base: string; state: string; batch_id: string; removed_package_bases: string[] }>(
      `/api/v1/subscriptions/${encodeURIComponent(packageBase)}`,
      { method: "DELETE" }
    ),
  packageDetail: (packageBase: string) =>
    request<PackageDetail>(`/api/v1/packages/${encodeURIComponent(packageBase)}`),
  setBuildPolicy: (packageBase: string, allowCheck: boolean) =>
    request<{ package_base: string; build_policy: { allow_check: boolean } }>(
      `/api/v1/packages/${encodeURIComponent(packageBase)}/build-policy`,
      { method: "POST", body: JSON.stringify({ allow_check: allowCheck }) }
    ),
  selectProvider: (packageBase: string, dependencyName: string, selectedPackageBase: string) =>
    request<{ package_base: string; dependency_name: string; selected_package_base: string; refresh: { state?: string; message?: string } }>(
      `/api/v1/packages/${encodeURIComponent(packageBase)}/providers/${encodeURIComponent(dependencyName)}`,
      { method: "POST", body: JSON.stringify({ selected_package_base: selectedPackageBase }) }
    ),
  refreshPackage: (packageBase: string) =>
    request<{ package_base: string; batch_id: string | null; batch_state: string }>(
      `/api/v1/packages/${encodeURIComponent(packageBase)}/refresh`,
      { method: "POST" }
    ),
  rebuildPackage: (packageBase: string) =>
    request<{ package_base: string; state: string; batch_id: string }>(
      `/api/v1/packages/${encodeURIComponent(packageBase)}/rebuild`,
      { method: "POST" }
    )
};
