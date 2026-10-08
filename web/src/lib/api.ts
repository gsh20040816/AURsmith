import type {
  AurPackage,
  Build,
  BuildLog,
  ClientBootstrap,
  PackageDetail,
  Publications,
  ReviewItem,
  RevisionRef,
  Session,
  Subscription,
  SystemStatus
} from "./types";

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

const post = <T>(path: string, body?: unknown) =>
  request<T>(path, { method: "POST", body: body === undefined ? undefined : JSON.stringify(body) });
const segment = encodeURIComponent;

export type SyncResult = { package_base: string; revision: RevisionRef; sync_error: string | null };

export const api = {
  login: (input: { username: string; password: string }) => post<Session>("/api/v1/auth/login", input),
  logout: () => post<void>("/api/v1/auth/logout"),
  me: () => request<Session>("/api/v1/auth/me"),
  status: () => request<SystemStatus>("/api/v1/status"),
  clientBootstrap: () => request<ClientBootstrap>("/api/v1/client-bootstrap"),
  searchAur: (query: string) => request<{ items: AurPackage[] }>(`/api/v1/aur/search?q=${segment(query)}`),

  subscriptions: () => request<{ items: Subscription[] }>("/api/v1/subscriptions"),
  subscribe: (packageName: string) => post<SyncResult>("/api/v1/subscriptions", { package_name: packageName }),
  unsubscribe: (packageBase: string) =>
    request<{ package_base: string; removed_package_bases: string[] }>(`/api/v1/subscriptions/${segment(packageBase)}`, {
      method: "DELETE"
    }),

  packageDetail: (packageBase: string) => request<PackageDetail>(`/api/v1/packages/${segment(packageBase)}`),
  refreshPackage: (packageBase: string) => post<SyncResult>(`/api/v1/packages/${segment(packageBase)}/refresh`),
  rebuildPackage: (packageBase: string) =>
    post<{ package_base: string; build_id: string; state: "queued" }>(`/api/v1/packages/${segment(packageBase)}/rebuild`),
  setBuildPolicy: (packageBase: string, allowCheck: boolean) =>
    post<{ package_base: string; allow_check: boolean }>(`/api/v1/packages/${segment(packageBase)}/build-policy`, {
      allow_check: allowCheck
    }),
  selectProvider: (packageBase: string, dependency: string, selectedPackageBase: string) =>
    post<{ package_base: string; dependency: string; selected_package_base: string }>(
      `/api/v1/packages/${segment(packageBase)}/providers/${segment(dependency)}`,
      { selected_package_base: selectedPackageBase }
    ),

  reviews: () => request<{ items: ReviewItem[] }>("/api/v1/reviews"),
  decideRevision: (revisionId: string, approve: boolean, rationale: string) =>
    post<{ revision_id: string; state: string }>(`/api/v1/revisions/${segment(revisionId)}/decision`, { approve, rationale }),
  retryReview: (revisionId: string) =>
    post<{ revision_id: string; state: string }>(`/api/v1/revisions/${segment(revisionId)}/retry-review`),

  builds: () => request<{ items: Build[] }>("/api/v1/builds"),
  buildLog: (buildId: string) => request<BuildLog>(`/api/v1/builds/${segment(buildId)}/log`),

  publications: () => request<Publications>("/api/v1/publications")
};
