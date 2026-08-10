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
  state: "active" | "paused" | "retained_without_references";
  reference_count: number;
  followed_outputs: string[];
  version: string | null;
  description: string | null;
  outputs: string[];
  maintainer: string | null;
  out_of_date: number | null;
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
export type BuildProfile = {
  id: string;
  name: string;
  architecture: string;
  profile_sha256: string;
  state: string;
  packages: string[];
  created_at: string;
  activated_at: string | null;
  last_verified_at: string | null;
  failure_reason: string | null;
};
export type Release = {
  id: string;
  batch_id: string;
  state: string;
  manifest_sha256: string;
  source_git_commit: string;
  writer_epoch: number;
  artifact_count: number;
  authorization_state: string | null;
  last_error: string | null;
  committed_at: string | null;
  created_at: string;
};
export type ArchiveCopy = {
  id: string;
  release_id: string;
  state: string;
  receipt_sha256: string | null;
  release_manifest_sha256: string;
  archiver_name: string | null;
  last_error: string | null;
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
  searchAur: (query: string) =>
    request<{ items: AurPackage[] }>(`/api/v1/aur/search?q=${encodeURIComponent(query)}`),
  subscriptions: () => request<{ items: Subscription[] }>("/api/v1/subscriptions"),
  audits: () => request<{ items: Audit[] }>("/api/v1/audits"),
  profiles: () => request<{ items: BuildProfile[] }>("/api/v1/profiles"),
  releases: () => request<{ items: Release[] }>("/api/v1/releases"),
  archives: () => request<{ items: ArchiveCopy[] }>("/api/v1/archives"),
  activateProfile: (id: string) =>
    request<{ id: string; state: string }>(`/api/v1/profiles/${encodeURIComponent(id)}/activate`, { method: "POST" }),
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
  pauseSubscription: (packageBase: string) =>
    request<{ package_base: string; state: string }>(
      `/api/v1/subscriptions/${encodeURIComponent(packageBase)}/pause`,
      { method: "POST" }
    ),
  resumeSubscription: (packageBase: string) =>
    request<{ package_base: string; state: string }>(
      `/api/v1/subscriptions/${encodeURIComponent(packageBase)}/resume`,
      { method: "POST" }
    ),
  unsubscribe: (packageBase: string) =>
    request<{ package_base: string; direct_subscription: boolean }>(
      `/api/v1/subscriptions/${encodeURIComponent(packageBase)}/unsubscribe`,
      { method: "POST" }
    ),
  refreshPackage: (packageBase: string) =>
    request<{ package_base: string; batch_id: string | null; batch_state: string }>(
      `/api/v1/packages/${encodeURIComponent(packageBase)}/refresh`,
      { method: "POST" }
    ),
  probeWorker: (id: string) =>
    request<{ id: string; state: string }>(`/api/v1/workers/${id}/probe`, {
      method: "POST"
    }),
  drainWorker: (id: string) =>
    request<{ id: string; state: string }>(`/api/v1/workers/${id}/drain`, {
      method: "POST"
    })
};
