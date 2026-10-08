import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

function ok(body: unknown) {
  return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
}

const STATUS = {
  ready: true,
  checked_at: "2026-10-01T00:00:00Z",
  checks: [
    { id: "database", ok: true, message: "SQLite 可用" },
    { id: "review", ok: true, message: "2+1 Agent 审查已配置" },
    { id: "builder", ok: false, message: "自服务启动以来没有 Builder 请求租约" },
    { id: "signer", ok: true, message: "尚未发布" }
  ],
  counts: { subscriptions: 1, pending_review: 0, manual_review: 1, queued: 0, running: 0, failed: 0 }
};

const PUBLICATIONS = { items: [], desired: { plan_sha256: "d".repeat(64), artifact_count: 0, withheld: [] } };

type Handler = (url: string, init?: RequestInit) => Response | undefined;

function stub(handler: Handler = () => undefined) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const handled = handler(url, init);
      if (handled) return handled;
      if (url.endsWith("/auth/me")) return ok({ username: "admin" });
      if (url.endsWith("/status")) return ok(STATUS);
      if (url.endsWith("/publications")) return ok(PUBLICATIONS);
      return ok({ items: [] });
    })
  );
}

describe("AURsmith 控制台", () => {
  beforeEach(() => {
    window.history.replaceState({}, "", "/");
    stub();
  });

  it("总览展示合并后的系统状态与流程", async () => {
    render(<App />);
    expect(await screen.findByRole("heading", { name: "总览" })).toBeInTheDocument();
    expect(screen.getByLabelText("软件包锻造流程")).toBeInTheDocument();
    expect(await screen.findByText("自服务启动以来没有 Builder 请求租约")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "客户端" })).toHaveAttribute("href", "/client");
    expect(screen.getByRole("link", { name: /审查/ })).toHaveAttribute("href", "/reviews");
    expect(screen.getByRole("link", { name: "发布" })).toHaveAttribute("href", "/publications");
    expect(screen.queryByRole("link", { name: /Worker/ })).not.toBeInTheDocument();
  });

  it("退出失败时保持当前界面并显示错误", async () => {
    stub((url) =>
      url.endsWith("/auth/logout")
        ? new Response(JSON.stringify({ code: "INTERNAL_ERROR", message: "退出请求失败" }), { status: 500, headers: { "Content-Type": "application/json" } })
        : undefined
    );
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "退出登录" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("退出请求失败");
    expect(screen.getByTitle("admin")).toBeInTheDocument();
  });

  it("退出返回 401 时清除本地会话", async () => {
    stub((url) =>
      url.endsWith("/auth/logout")
        ? new Response(JSON.stringify({ code: "UNAUTHORIZED", message: "会话已失效" }), { status: 401, headers: { "Content-Type": "application/json" } })
        : undefined
    );
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "退出登录" }));
    expect(await screen.findByRole("button", { name: "登录" })).toBeInTheDocument();
    expect(screen.queryByText("admin")).not.toBeInTheDocument();
  });

  it("软件包详情可关闭 check 并带 CSRF 头", async () => {
    let allowCheck = true;
    let csrfHeader: string | null = null;
    stub((url, init) => {
      if (url.endsWith("/subscriptions")) {
        return ok({
          items: [{
            package_base: "demo", direct: true, required_by: [], version: "1-1", description: "演示", maintainer: "tester",
            out_of_date: null, outputs: ["demo"], sync_error: null, last_checked_at: null, revision_state: "approved",
            revision_version: "1-1", build_state: "succeeded", build_version: "1-1", unresolved_providers: []
          }]
        });
      }
      if (url.endsWith("/packages/demo/build-policy") && init?.method === "POST") {
        csrfHeader = new Headers(init.headers).get("X-AURsmith-CSRF");
        allowCheck = false;
        return ok({ package_base: "demo", allow_check: false });
      }
      if (url.endsWith("/packages/demo")) {
        return ok({
          package_base: "demo", direct: true, in_closure: true, required_by: [], version: "1-1", description: "演示",
          maintainer: "tester", outputs: ["demo"], allow_check: allowCheck,
          sync: { last_checked_at: null, next_check_at: "2026-10-01T00:00:00Z", error: null },
          revisions: [], dependencies: [], builds: []
        });
      }
      return undefined;
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("link", { name: "软件包" }));
    fireEvent.click(await screen.findByRole("button", { name: "详情" }));
    expect(await screen.findByText(/客户端不会按版本比较自动升级/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "禁用 check()" }));
    expect(await screen.findByText("已显式禁用")).toBeInTheDocument();
    expect(csrfHeader).toBe("1");
  });

  it("审查页可以人工批准首次添加的 revision", async () => {
    let decision: unknown = null;
    stub((url, init) => {
      if (url.endsWith("/reviews")) {
        return ok({
          items: [{
            revision_id: "rev-1", package_base: "demo", version: "1-1", aur_commit: "a".repeat(40), vcs_commit: null,
            state: "manual_review", first_time: true, baseline_revision_id: null, created_at: "2026-10-01T00:00:00Z", decided_at: null,
            reviews: [
              { kind: "scan", role: null, model: null, verdict: "approve", summary: "确定性扫描未发现 Block 级问题", findings: [], created_at: "2026-10-01T00:00:00Z" },
              { kind: "agent", role: "low", model: "model-a", verdict: "approve", summary: "无异常", findings: { findings: [], files_read: ["PKGBUILD"] }, created_at: "2026-10-01T00:00:01Z" },
              { kind: "agent", role: "low", model: "model-b", verdict: "approve", summary: "仅下载上游源码", findings: { findings: [], files_read: ["PKGBUILD"] }, created_at: "2026-10-01T00:00:02Z" }
            ]
          }]
        });
      }
      if (url.endsWith("/revisions/rev-1/decision") && init?.method === "POST") {
        decision = JSON.parse(String(init.body));
        return ok({ revision_id: "rev-1", state: "approved" });
      }
      return undefined;
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("link", { name: /审查/ }));
    expect(await screen.findByText("首次添加的软件包必须人工审批。")).toBeInTheDocument();
    expect(screen.getByText("model-a")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("人工判断理由"), { target: { value: "已人工核对 PKGBUILD" } });
    fireEvent.click(screen.getByRole("button", { name: "批准当前 commit" }));
    expect(await screen.findByText("已批准该 revision")).toBeInTheDocument();
    expect(decision).toEqual({ approve: true, rationale: "已人工核对 PKGBUILD" });
  });

  it("客户端页显示带外核对指纹和 pacman 配置", async () => {
    stub((url) =>
      url.endsWith("/client-bootstrap")
        ? ok({ repository_config: "[aursmith]", gpg_fingerprint: "ABCD1234", gpg_key_url: "https://repo.test/key", commands: ["pacman -Syu"], warnings: ["请带外核对"] })
        : undefined
    );
    render(<App />);
    fireEvent.click(await screen.findByRole("link", { name: "客户端" }));
    expect(await screen.findByText("ABCD1234")).toBeInTheDocument();
    expect(screen.getByText("[aursmith]")).toBeInTheDocument();
  });

  it("构建页可以查看失败构建的日志", async () => {
    const id = "11111111-1111-4111-8111-111111111111";
    stub((url) => {
      if (url.endsWith("/builds")) {
        return ok({
          items: [{
            id, package_base: "demo", revision_id: "rev-1", version: "1-1", aur_commit: "a".repeat(40), vcs_commit: null,
            state: "failed", attempt: 1, reason: "approved", builder_id: "home", error_code: "GUEST_BUILD_FAILED",
            error_class: "deterministic", artifacts: null, has_log: true, lease_expires_at: null,
            created_at: "2026-10-01T00:00:00Z", started_at: "2026-10-01T00:00:01Z", finished_at: "2026-10-01T00:01:00Z"
          }]
        });
      }
      if (url.endsWith(`/builds/${id}/log`)) return ok({ build_id: id, state: "failed", error_code: "GUEST_BUILD_FAILED", log: "==> build.log\ncompiler error" });
      return undefined;
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("link", { name: "构建" }));
    expect(await screen.findByText(/确定性失败/)).toBeInTheDocument();
    fireEvent.click(await screen.findByRole("button", { name: "日志" }));
    expect(await screen.findByText(/compiler error/)).toBeInTheDocument();
  });

  it("发布页展示期望状态与暂缓的软件包", async () => {
    stub((url) =>
      url.endsWith("/publications")
        ? ok({ items: [], desired: { plan_sha256: "e".repeat(64), artifact_count: 2, withheld: ["broken-pkg"] } })
        : undefined
    );
    render(<App />);
    fireEvent.click(await screen.findByRole("link", { name: "发布" }));
    expect(await screen.findByText("期望状态")).toBeInTheDocument();
    expect(await screen.findByText("broken-pkg")).toBeInTheDocument();
  });
});
