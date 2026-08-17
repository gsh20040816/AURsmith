import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

describe("AURsmith 控制台", () => {
  beforeEach(() => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const body = url.endsWith("/auth/me")
          ? { id: "admin-id", username: "admin" }
          : url.endsWith("/requirements")
            ? { items: [{ id: "P01", title: "AUR 软件包搜索和订阅生命周期" }] }
            : { items: [] };
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "Content-Type": "application/json" }
      });
    }));
  });

  it("登录后显示锻造流程和需求总账", async () => {
    render(<App />);
    expect(await screen.findByText("从上游变化到可安装软件包")).toBeInTheDocument();
    expect(screen.getByLabelText("软件包锻造流程")).toBeInTheDocument();
    expect(await screen.findByText("AUR 软件包搜索和订阅生命周期")).toBeInTheDocument();
    const urls = vi.mocked(fetch).mock.calls.map(([input]) => String(input));
    expect(urls.some((url) => url.includes("/setup"))).toBe(false);
  });

  it("退出失败时保持当前界面并显示错误", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/auth/logout")) {
        return new Response(JSON.stringify({ code: "INTERNAL_ERROR", message: "退出请求失败" }), {
          status: 500,
          headers: { "Content-Type": "application/json" }
        });
      }
      const body = url.endsWith("/auth/me")
        ? { id: "admin-id", username: "admin" }
        : url.endsWith("/requirements")
          ? { items: [] }
          : { items: [] };
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "Content-Type": "application/json" }
      });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "退出登录" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("退出请求失败");
    expect(screen.getByText("admin")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "登录" })).not.toBeInTheDocument();
  });

  it("退出返回 401 时清除本地会话并进入登录页", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/auth/logout")) {
        return new Response(JSON.stringify({ code: "SESSION_REQUIRED", message: "会话已失效" }), {
          status: 401,
          headers: { "Content-Type": "application/json" }
        });
      }
      const body = url.endsWith("/auth/me")
        ? { id: "admin-id", username: "admin" }
        : { items: [] };
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "Content-Type": "application/json" }
      });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "退出登录" }));
    expect(await screen.findByRole("button", { name: "登录" })).toBeInTheDocument();
    expect(screen.queryByText("admin")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("AUR 软件包消失时在全局和总览显示迁移告警", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const body = url.endsWith("/auth/me") ? { id: "admin-id", username: "admin" }
          : url.endsWith("/alerts") ? { items: [{ id: "alert-1", fingerprint: "aur-lifecycle-missing:removed-demo", severity: "warning", state: "open", title: "AUR 软件包已不可见：removed-demo", details: { package_base: "removed-demo", possible_causes: ["deleted", "renamed", "merged"] }, opened_at: "2026-08-15T00:00:00Z", acknowledged_at: null, resolved_at: null }] }
            : { items: [] };
      return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
    }));
    render(<App />);
    expect(await screen.findByRole("alert")).toHaveTextContent("AUR 软件包已不可见：removed-demo");
    expect(screen.getByRole("button", { name: "查看 1 条待处理告警" })).toBeInTheDocument();
    expect(screen.getAllByText(/当前已发布版本继续保留/).length).toBeGreaterThan(0);
  });

  it("Worker 页面提供探测注册表单", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Worker/ }));
    expect(await screen.findByText("注册 Worker")).toBeInTheDocument();
    expect(screen.getByLabelText("实例名称")).toBeInTheDocument();
    expect(screen.getByLabelText("连接模式")).toHaveValue("reverse");
    expect(screen.getByLabelText("Builder 实例 UUID")).toBeInTheDocument();
    expect(screen.getByLabelText("Builder 身份公钥")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "探测并注册" })).toBeInTheDocument();
  });

  it("软件包详情可以显式禁用 check", async () => {
    let allowCheck = true;
    let csrfHeader: string | null = null;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      let body: unknown;
      if (url.endsWith("/auth/me")) body = { id: "admin-id", username: "admin" };
      else if (url.endsWith("/requirements")) body = { items: [] };
      else if (url.endsWith("/subscriptions")) body = { items: [{ id: "sub", package_base: "demo", kind: "direct", reference_count: 0, followed_outputs: ["demo"], version: "1-1", description: "演示", outputs: ["demo"], maintainer: "tester", out_of_date: null }] };
      else if (url.endsWith("/packages/demo/build-policy") && init?.method === "POST") {
        csrfHeader = new Headers(init.headers).get("X-AURsmith-CSRF");
        allowCheck = false;
        body = { package_base: "demo", build_policy: { allow_check: false } };
      } else if (url.endsWith("/packages/demo")) body = { package_base: "demo", version: "1-1", description: "演示", maintainer: "tester", outputs: ["demo"], build_policy: { allow_check: allowCheck }, revisions: [], dependency_resolution: [], events: [] };
      else body = { items: [] };
      return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /软件包/ }));
    fireEvent.click(await screen.findByRole("button", { name: "详情" }));
    fireEvent.click(await screen.findByRole("button", { name: "禁用 check()" }));
    expect(await screen.findByText("已显式禁用")).toBeInTheDocument();
    expect(csrfHeader).toBe("1");
  });

  it("设置页显示 Agent 预算且不回显密钥", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const body = url.endsWith("/auth/me") ? { id: "admin-id", username: "admin" }
          : url.endsWith("/settings") ? { agents: { supported_adapters: ["codex", "claude_code"], low_runner_count: 3, high_runner_configured: true, configuration_source: "docker_compose_environment_and_secrets", api_keys_exposed: false }, budget: { agent_daily_call_limit: 300, agent_monthly_call_limit: 3000, agent_monthly_cost_limit_microusd: 5000000, agent_random_high_cost_review_basis_points: 0, daily_used: 1, monthly_used: 2, monthly_cost_microusd: 3 }, notifications: { webhook_configured: false, ntfy_configured: false }, repository: { name: "aursmith", base_url: "https://repo.test", publisher_compatibility_days: 30 } }
            : url.endsWith("/client-bootstrap") ? { repository_config: "[aursmith]", gpg_fingerprint: "ABCD", gpg_key_url: "https://repo.test/key", client_ca_url: "/api/v1/client-ca.crt", commands: [], warnings: [] }
              : { items: [] };
      return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /设置/ }));
    expect(await screen.findByText("API key 不可见")).toBeInTheDocument();
    expect(screen.getByLabelText("每日调用上限")).toHaveValue(300);
    expect(screen.queryByText(/sk-/)).not.toBeInTheDocument();
  });

  it("Release 页面可以查看签名证据摘要", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const body = url.endsWith("/auth/me") ? { id: "admin-id", username: "admin" }
          : url.endsWith("/requirements") ? { items: [] }
            : url.endsWith("/releases") ? { items: [{ id: "11111111-1111-4111-8111-111111111111", batch_id: "22222222-2222-4222-8222-222222222222", state: "committed", manifest_sha256: "a".repeat(64), source_git_commit: "b".repeat(40), writer_epoch: 1, artifact_count: 1, authorization_state: "published", last_error: null, committed_at: "2026-08-10T00:00:00Z", created_at: "2026-08-10T00:00:00Z" }] }
              : url.includes("/releases/11111111-1111-4111-8111-111111111111/evidence") ? { release_id: "11111111-1111-4111-8111-111111111111", authorization_sha256: "c".repeat(64), evidence: { schema_version: 1, records: [{ kind: "job_result", identity: "build-job", sha256: "d".repeat(64), document: { provenance: { check_enabled: true } } }] } }
                : { items: [] };
      return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Release/ }));
    fireEvent.click(await screen.findByRole("button", { name: "查看证据" }));
    expect(await screen.findByText("1 条证据记录")).toBeInTheDocument();
    expect(screen.getByText("job_result")).toBeInTheDocument();
    expect(screen.getByText("build-job")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "build-job" }));
    expect(await screen.findByText(/"check_enabled": true/)).toBeInTheDocument();
  });

  it("构建页可以查看失败任务的有界日志", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const body = url.endsWith("/auth/me") ? { id: "admin-id", username: "admin" }
          : url.endsWith("/requirements") ? { items: [] }
            : url.endsWith("/jobs") ? { items: [{ id: "11111111-1111-4111-8111-111111111111", kind: "build", required_role: "builder", status: "failed", priority: 40, failure_code: "GUEST_BUILD_FAILED", revision_sha256: "a".repeat(64), worker_name: "compute-01", attempt_count: 1, has_evidence: true, next_attempt_at: null, created_at: "2026-08-10T00:00:00Z", updated_at: "2026-08-10T00:01:00Z" }] }
              : url.includes("/jobs/11111111-1111-4111-8111-111111111111/evidence") ? { job_id: "11111111-1111-4111-8111-111111111111", kind: "build", sha256: "b".repeat(64), created_at: "2026-08-10T00:01:00Z", document: { status: "failed", logs: [{ path: "output/build.log", content_utf8: "compiler error" }] } }
                : { items: [] };
      return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /构建/ }));
    fireEvent.click(await screen.findByRole("button", { name: "查看日志与证据" }));
    expect(await screen.findByText(/compiler error/)).toBeInTheDocument();
  });
});
