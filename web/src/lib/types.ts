/* Domain types returned by the AURsmith controller API. */

export type Session = { id: string; username: string };

export type DoctorCheck = { id: string; ok: boolean; message: string };
export type Doctor = {
  ready: boolean;
  checked_at: string;
  checks: DoctorCheck[];
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

export type Revision = {
  id: string;
  aur_commit: string;
  vcs_commit: string | null;
  upstream_version: string;
  published_version: string | null;
  state: string;
  release_state: string | null;
  created_at: string;
};

export type DependencyResolution = {
  name: string;
  kind: string;
  target_package_base: string | null;
  state: "official_or_unknown" | "resolved" | "needs_selection" | "cycle";
  candidates: string[];
};

export type PackageDetail = {
  package_base: string;
  version: string;
  description: string | null;
  maintainer: string | null;
  outputs: string[];
  build_policy: { allow_check: boolean };
  revisions: Revision[];
  dependency_resolution: DependencyResolution[];
};

export type AuditRun = {
  tier: "low" | "high";
  slot: number;
  attempt: number;
  adapter: string;
  provider: string;
  model: string;
  adapter_version: string;
  status: string;
  verdict: "approve" | "reject" | "error" | null;
  report: { summary?: string; findings?: unknown[]; files_read?: string[] } | null;
  started_at: string | null;
  finished_at: string | null;
};

export type AuditFinding = {
  rule_id: string;
  severity: "information" | "suspicious" | "block";
  path: string;
  summary: string;
};

export type Audit = {
  sha256: string;
  revision_id: string;
  state:
    | "agent_pending"
    | "agent_running"
    | "manual_review"
    | "approved"
    | "rejected"
    | "blocked";
  policy_version: string;
  package_base: string;
  aur_commit: string;
  findings: AuditFinding[];
  coverage: {
    aur_wrapper?: { mode: string; files: string[] };
    upstream_source?: { mode: string; sources?: unknown[]; statement: string };
    audit_reuse?: { mode: string; source_bundle_sha256: string };
  };
  runs: AuditRun[];
  created_at: string;
};

export type Job = {
  id: string;
  kind: "build";
  status: string;
  priority: number;
  failure_code: string | null;
  revision_sha256: string | null;
  attempt_count: number;
  has_logs: boolean;
  next_attempt_at: string | null;
  created_at: string;
  updated_at: string;
};

export type Release = {
  id: string;
  batch_id: string;
  state: string;
  position: "current" | "previous" | "failed";
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
  keyring_generation: number | null;
  keyring_published_at: string | null;
  keyring_next_due_at: string | null;
  client_ca_url: string | null;
  commands: string[];
  warnings: string[];
};

export type LogDocument = {
  job_id: string;
  kind: "build";
  sha256: string;
  document: {
    schema_version: number;
    status: string;
    failure_code: string | null;
    guest_result: unknown;
    logs: Array<{
      path: string;
      size: number;
      sha256: string | null;
      truncated: boolean;
      content_base64?: string;
      content_utf8?: string | null;
      omitted_reason?: string;
    }>;
  };
  created_at: string;
};
