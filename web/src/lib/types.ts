/* aursmithd 管理 API 返回的领域类型。 */

export type Session = { username: string };

export type StatusCheck = { id: "database" | "review" | "builder" | "signer" | string; ok: boolean; message: string };
export type SystemStatus = {
  ready: boolean;
  checked_at: string;
  checks: StatusCheck[];
  counts: {
    subscriptions: number;
    pending_review: number;
    manual_review: number;
    queued: number;
    running: number;
    failed: number;
  };
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

export type RevisionState = "pending_review" | "manual_review" | "approved" | "rejected" | "superseded";
export type BuildState = "queued" | "running" | "uploading" | "succeeded" | "failed";

/** 订阅闭包中的一个 pkgbase：direct 为显式订阅，否则是必要依赖。 */
export type Subscription = {
  package_base: string;
  direct: boolean;
  required_by: string[];
  version: string | null;
  description: string | null;
  maintainer: string | null;
  out_of_date: number | null;
  outputs: string[] | null;
  sync_error: string | null;
  last_checked_at: string | null;
  revision_state: RevisionState | null;
  revision_version: string | null;
  build_state: BuildState | null;
  build_version: string | null;
  unresolved_providers: string[];
};

export type RevisionRef = { id: string; state: RevisionState } | null;

export type PackageRevision = {
  id: string;
  aur_commit: string;
  vcs_commit: string | null;
  version: string;
  state: RevisionState;
  first_time: boolean;
  created_at: string;
  decided_at: string | null;
};

export type ResolvedDependency = {
  name: string;
  kind: string;
  state: "official" | "aur" | "needs_selection" | "unknown";
  target: string | null;
  selected: string | null;
  candidates: string[];
};

export type ArtifactRecord = {
  file: string;
  sha256: string;
  size: number;
  package_name: string;
  package_version: string;
  architecture: string;
};

export type Build = {
  id: string;
  package_base: string;
  revision_id: string;
  version: string;
  aur_commit: string;
  vcs_commit: string | null;
  state: BuildState;
  attempt: number;
  reason: "approved" | "manual";
  builder_id: string | null;
  error_code: string | null;
  error_class: "transient" | "deterministic" | "config" | null;
  artifacts: ArtifactRecord[] | null;
  has_log: boolean;
  lease_expires_at: string | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  waiting_reason?: string | null;
};

export type PackageDetail = {
  package_base: string;
  direct: boolean;
  in_closure: boolean;
  required_by: string[];
  version: string | null;
  description: string | null;
  maintainer: string | null;
  outputs: string[] | null;
  allow_check: boolean;
  sync: { last_checked_at: string | null; next_check_at: string; error: string | null };
  revisions: PackageRevision[];
  dependencies: ResolvedDependency[];
  builds: Build[];
};

export type AgentFinding = {
  severity: "info" | "warning" | "high" | "critical";
  category: string;
  message: string;
  file: string | null;
  line: number | null;
  evidence: string;
};

export type ScanFinding = {
  rule_id: string;
  severity: "information" | "suspicious" | "block";
  path: string;
  summary: string;
};

export type ReviewRecord = {
  kind: "scan" | "reuse" | "agent" | "human";
  role: "low" | "high" | null;
  model: string | null;
  verdict: "approve" | "reject" | "error";
  summary: string;
  /** scan：ScanFinding[]；agent：{ findings, files_read }；其他为 null。 */
  findings: ScanFinding[] | { findings: AgentFinding[]; files_read: string[] } | null;
  created_at: string;
};

export type ReviewItem = {
  revision_id: string;
  package_base: string;
  version: string;
  aur_commit: string;
  vcs_commit: string | null;
  state: RevisionState;
  first_time: boolean;
  baseline_revision_id: string | null;
  created_at: string;
  decided_at: string | null;
  reviews: ReviewRecord[];
};

export type Publication = {
  id: string;
  plan_sha256: string;
  state: "pending" | "published" | "failed";
  current: boolean;
  error: string | null;
  manifest_sha256: string | null;
  keyring_fingerprint: string | null;
  artifacts: ArtifactRecord[];
  created_at: string;
  finished_at: string | null;
};

export type Publications = {
  items: Publication[];
  desired: { plan_sha256: string; artifact_count: number; withheld: string[] };
};

export type ClientBootstrap = {
  repository_config: string;
  gpg_fingerprint: string;
  gpg_key_url: string;
  commands: string[];
  warnings: string[];
};

export type BuildLog = {
  build_id: string;
  state: BuildState;
  error_code: string | null;
  log: string;
};
