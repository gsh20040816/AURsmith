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

export type Session = { id: string; username: string };
export type Job = {
  id: string;
  kind: "build";
  required_role: "builder";
  status: string;
  priority: number;
  failure_code: string | null;
  revision_sha256: string | null;
  worker_name: string | null;
  attempt_count: number;
  has_evidence: boolean;
  next_attempt_at: string | null;
  created_at: string;
  updated_at: string;
};
export type AurPackage = {
  name: string;
  package_base: string;
  version: string;
  description: string | null;
  maintainer: string | null;
  out_of_date: number | null;
  last_modified: number;
  depends: string[];
  make_depends: string[];
  check_depends: string[];
  opt_depends: string[];
  provides: string[];
};
export type Subscription = {
  id: string;
  package_base: string;
  kind: "direct" | "implicit";
  reference_count: number;
  followed_outputs: string[];
  version: string | null;
  description: string | null;
  outputs: string[];
  maintainer: string | null;
  out_of_date: number | null;
};
export type PackageDetail = {
  package_base: string;
  version: string;
  description: string | null;
  maintainer: string | null;
  outputs: string[];
  build_policy: { allow_check: boolean };
  revisions: Array<{ id: string; aur_commit: string; vcs_commit: string | null; upstream_version: string; published_version: string | null; state: string; release_state: string | null; created_at: string }>;
  dependency_resolution: Array<{ name: string; kind: string; target_package_base: string | null; state: string; candidates: string[] }>;
  events: Array<{ type: string; payload: unknown; actor: string; created_at: string }>;
};
export type Audit = {
  sha256: string;
  revision_id: string;
  state: string;
  policy_version: string;
  package_base: string;
  aur_commit: string;
  findings: Array<{ rule_id: string; severity: string; path: string; summary: string }>;
  coverage: {
    aur_wrapper?: { mode: string; files: string[] };
    upstream_source?: { mode: string; statement: string };
  };
  created_at: string;
};
export type Release = {
  id: string;
  batch_id: string;
  state: string;
  manifest_sha256: string;
  artifact_count: number;
  last_error: string | null;
  committed_at: string | null;
  created_at: string;
};
export type ClientBootstrap = {
  repository_config: string;
  gpg_fingerprint: string;
  gpg_key_url: string;
  client_ca_url: string | null;
  commands: string[];
  warnings: string[];
};
export type Doctor = {
  ready: boolean;
  checked_at: string;
  checks: Array<{ id: string; ok: boolean; message: string }>;
};
export const api = {
  login: (input: { username: string; password: string }) =>
    request<{ username: string }>("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify(input)
    }),
  logout: () => request<void>("/api/v1/auth/logout", { method: "POST" }),
  me: () => request<Session>("/api/v1/auth/me"),
  jobs: () => request<{ items: Job[] }>("/api/v1/jobs"),
  jobEvidence: (id: string) => request<{ job_id: string; kind: string; sha256: string; document: unknown; created_at: string }>(`/api/v1/jobs/${encodeURIComponent(id)}/evidence`),
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
  clientBootstrap: () => request<ClientBootstrap>("/api/v1/client-bootstrap"),
  doctor: () => request<Doctor>("/api/v1/doctor"),
  decideAudit: (bundle: string, approve: boolean, rationale: string) =>
    request<{ bundle_sha256: string; decision: string }>(
      `/api/v1/audits/${encodeURIComponent(bundle)}/manual-decision`,
      { method: "POST", body: JSON.stringify({ approve, rationale }) }
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
    request<{ package_base: string; dependency_name: string; selected_package_base: string }>(
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
