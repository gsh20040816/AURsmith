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
  storage: { total_bytes: number; available_bytes: number; available_percent: number; path: string } | null;
  clock_skew_seconds: number | null;
};
export type Job = {
  id: string;
  kind: "fetch" | "build" | "profile_fixture";
  required_role: Worker["role"];
  status: string;
  priority: number;
  failure_code: string | null;
  revision_sha256: string | null;
  worker_name: string | null;
  attempt_count: number;
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
  state: "active" | "paused" | "retained_without_references";
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
  vcs_rewrite_review: {
    previous_commit: string;
    current_commit: string;
    state: "pending" | "approved" | "rejected";
    rationale: string | null;
    requested_at: string;
    decided_at: string | null;
  } | null;
  revisions: Array<{ id: string; aur_commit: string; vcs_commit: string | null; upstream_version: string; published_version: string | null; state: string; created_at: string }>;
  dependency_resolution: Array<{ name: string; kind: string; target_package_base: string | null; state: string; candidates: string[] }>;
  events: Array<{ type: string; payload: unknown; actor: string; created_at: string }>;
};
export type RebuildRecommendation = {
  package_base: string;
  state: "suggested" | "disabled" | "scheduled" | "resolved";
  reason: string;
  changes: Array<{ dependency: string; built_with: string; current: string }>;
  detected_at: string;
  updated_at: string;
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
export type AuthorizedProfile = {
  id: string;
  profile_sha256: string;
  fixture_job_id: string;
  envelope: unknown;
};
export type ProfileRecommendation = {
  package_name: string;
  action: string;
  stats: {
    successful_builds: number;
    uses_recent: number;
    uses_this_month: number;
    download_bytes: number;
    average_saved_seconds: number;
    cache_hits: number;
    days_since_last_use: number;
    currently_baked: boolean;
  };
  consecutive_hot_periods: number;
  consecutive_low_periods: number;
  evaluated_at: string;
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
export type ReleaseEvidence = {
  release_id: string;
  authorization_sha256: string;
  evidence: {
    schema_version: number;
    records: Array<{ kind: string; identity: string; sha256: string; document: unknown }>;
  };
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
export type ClientBootstrap = {
  repository_config: string;
  gpg_fingerprint: string;
  gpg_key_url: string;
  commands: string[];
  warnings: string[];
};
export type Alert = {
  id: string;
  fingerprint: string;
  severity: string;
  state: "open" | "acknowledged" | "resolved";
  title: string;
  details: unknown;
  opened_at: string;
  acknowledged_at: string | null;
  resolved_at: string | null;
};
export type Doctor = {
  ready: boolean;
  checked_at: string;
  checks: Array<{ id: string; ok: boolean; message: string }>;
};
export type ControlPlaneBackup = {
  id: string;
  state: "creating" | "verified" | "failed";
  database_sha256: string | null;
  database_size: number | null;
  last_error: string | null;
  created_at: string;
  verified_at: string | null;
  archive_state: "issued" | "verified" | "failed" | null;
  archive_receipt_sha256: string | null;
  archiver_name: string | null;
};
export type ArchiveInventory = {
  id: string;
  archiver_name: string;
  full_digest: boolean;
  release_count: number;
  backup_count: number;
  file_count: number;
  byte_count: number;
  failure_count: number;
  checked_at: string;
};
export type Settings = {
  agents: {
    supported_adapters: string[];
    low_runner_count: number;
    high_runner_configured: boolean;
    configuration_source: string;
    api_keys_exposed: false;
  };
  budget: {
    agent_daily_call_limit: number;
    agent_monthly_call_limit: number;
    agent_monthly_cost_limit_microusd: number;
    daily_used: number;
    monthly_used: number;
    monthly_cost_microusd: number;
  };
  notifications: { webhook_configured: boolean; ntfy_configured: boolean };
  repository: { name: string; base_url: string; publisher_compatibility_days: number };
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
  settings: () => request<Settings>("/api/v1/settings"),
  updateSettings: (budget: Pick<Settings["budget"], "agent_daily_call_limit" | "agent_monthly_call_limit" | "agent_monthly_cost_limit_microusd">) =>
    request<Settings>("/api/v1/settings", { method: "PUT", body: JSON.stringify(budget) }),
  workers: () => request<{ items: Worker[] }>("/api/v1/workers"),
  registerWorker: (worker: {
    name: string;
    role: Worker["role"];
    endpoint: string;
    ssh_host_key_sha256: string;
    protocol_version: number;
    labels: string[];
  }) => request<{ id: string }>("/api/v1/workers", {
    method: "POST",
    body: JSON.stringify(worker)
  }),
  jobs: () => request<{ items: Job[] }>("/api/v1/jobs"),
  searchAur: (query: string) =>
    request<{ items: AurPackage[] }>(`/api/v1/aur/search?q=${encodeURIComponent(query)}`),
  subscriptions: () => request<{ items: Subscription[] }>("/api/v1/subscriptions"),
  audits: () => request<{ items: Audit[] }>("/api/v1/audits"),
  profiles: () => request<{ items: BuildProfile[] }>("/api/v1/profiles"),
  authorizeProfile: (candidate: unknown) => request<AuthorizedProfile>("/api/v1/profiles", {
    method: "POST",
    body: JSON.stringify(candidate)
  }),
  profileRecommendations: () => request<{ items: ProfileRecommendation[] }>("/api/v1/profile-recommendations"),
  releases: () => request<{ items: Release[] }>("/api/v1/releases"),
  releaseEvidence: (id: string) => request<ReleaseEvidence>(`/api/v1/releases/${encodeURIComponent(id)}/evidence`),
  rollbackRelease: (id: string) => request<{
    release_id: string;
    server_rolled_back: boolean;
    client_auto_downgrade: false;
    pacman_commands: string[];
  }>(`/api/v1/releases/${encodeURIComponent(id)}/rollback`, { method: "POST" }),
  archives: () => request<{ items: ArchiveCopy[] }>("/api/v1/archives"),
  archiveInventories: () => request<{ items: ArchiveInventory[] }>("/api/v1/archive-inventories"),
  clientBootstrap: () => request<ClientBootstrap>("/api/v1/client-bootstrap"),
  alerts: () => request<{ items: Alert[] }>("/api/v1/alerts"),
  acknowledgeAlert: (id: string) => request<{ id: string; state: string }>(`/api/v1/alerts/${encodeURIComponent(id)}/acknowledge`, { method: "POST" }),
  doctor: () => request<Doctor>("/api/v1/doctor"),
  backups: () => request<{ items: ControlPlaneBackup[] }>("/api/v1/backups"),
  createBackup: () => request<ControlPlaneBackup>("/api/v1/backups", { method: "POST" }),
  verifyBackup: (id: string) => request<ControlPlaneBackup>(`/api/v1/backups/${encodeURIComponent(id)}/verify`, { method: "POST" }),
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
  purgeSubscription: (packageBase: string) =>
    request<{ package_base: string; state: string }>(
      `/api/v1/subscriptions/${encodeURIComponent(packageBase)}/purge`,
      { method: "POST" }
    ),
  packageDetail: (packageBase: string) =>
    request<PackageDetail>(`/api/v1/packages/${encodeURIComponent(packageBase)}`),
  setBuildPolicy: (packageBase: string, allowCheck: boolean) =>
    request<{ package_base: string; build_policy: { allow_check: boolean } }>(
      `/api/v1/packages/${encodeURIComponent(packageBase)}/build-policy`,
      { method: "POST", body: JSON.stringify({ allow_check: allowCheck }) }
    ),
  decideVcsRewrite: (packageBase: string, approve: boolean, rationale: string) =>
    request<{ package_base: string; state: string }>(
      `/api/v1/packages/${encodeURIComponent(packageBase)}/vcs-rewrite-decision`,
      { method: "POST", body: JSON.stringify({ approve, rationale }) }
    ),
  selectProvider: (packageBase: string, dependencyName: string, selectedPackageBase: string) =>
    request<{ package_base: string; dependency_name: string; selected_package_base: string }>(
      `/api/v1/packages/${encodeURIComponent(packageBase)}/providers/${encodeURIComponent(dependencyName)}`,
      { method: "POST", body: JSON.stringify({ selected_package_base: selectedPackageBase }) }
    ),
  rebuildRecommendations: () => request<{ items: RebuildRecommendation[] }>("/api/v1/rebuild-recommendations"),
  disableRebuildRecommendation: (packageBase: string) =>
    request<{ package_base: string; state: string }>(`/api/v1/rebuild-recommendations/${encodeURIComponent(packageBase)}/disable`, { method: "POST" }),
  scheduleRebuildRecommendation: (packageBase: string) =>
    request<{ package_base: string; state: string; batch_id: string }>(`/api/v1/rebuild-recommendations/${encodeURIComponent(packageBase)}/schedule`, { method: "POST" }),
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
