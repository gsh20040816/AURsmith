import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

function ok(body: unknown) {
  return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
}

describe("AURsmith 控制台", () => {
  beforeEach(() => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/auth/me")) return ok({ id: "admin-id", username: "admin" });
      if (url.endsWith("/doctor")) return ok({ ready: true, checked_at: "2026-08-17T00:00:00Z", checks: [] });
      return ok({ items: [] });
    }));
  });

  it("只展示固定两机核心流程", async () => {
    render(<App />);
    expect(await screen.findByText("审查后再构建，签名后再发布")).toBeInTheDocument();
    expect(screen.getByLabelText("软件包锻造流程")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "客户端" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Worker/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /告警/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /设置/ })).not.toBeInTheDocument();
    expect(screen.queryByText("归档")).not.toBeInTheDocument();
  });

  it("退出失败时保持当前界面并显示错误", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/auth/logout")) return new Response(JSON.stringify({ code: "INTERNAL_ERROR", message: "退出请求失败" }), { status: 500, headers: { "Content-Type": "application/json" } });
      if (url.endsWith("/auth/me")) return ok({ id: "admin-id", username: "admin" });
      if (url.endsWith("/doctor")) return ok({ ready: true, checked_at: "", checks: [] });
      return ok({ items: [] });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "退出登录" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("退出请求失败");
    expect(screen.getByText("admin")).toBeInTheDocument();
  });

  it("退出返回 401 时清除本地会话", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/auth/logout")) return new Response(JSON.stringify({ code: "SESSION_REQUIRED", message: "会话已失效" }), { status: 401, headers: { "Content-Type": "application/json" } });
      if (url.endsWith("/auth/me")) return ok({ id: "admin-id", username: "admin" });
      if (url.endsWith("/doctor")) return ok({ ready: true, checked_at: "", checks: [] });
      return ok({ items: [] });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "退出登录" }));
    expect(await screen.findByRole("button", { name: "登录" })).toBeInTheDocument();
    expect(screen.queryByText("admin")).not.toBeInTheDocument();
  });

  it("软件包详情说明同版本重建风险并可关闭 check", async () => {
    let allowCheck = true;
    let csrfHeader: string | null = null;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith("/auth/me")) return ok({ id: "admin-id", username: "admin" });
      if (url.endsWith("/doctor")) return ok({ ready: true, checked_at: "", checks: [] });
      if (url.endsWith("/subscriptions")) return ok({ items: [{ id: "sub", package_base: "demo", kind: "direct", reference_count: 0, followed_outputs: ["demo"], version: "1-1", description: "演示", outputs: ["demo"], maintainer: "tester", out_of_date: null }] });
      if (url.endsWith("/packages/demo/build-policy") && init?.method === "POST") {
        csrfHeader = new Headers(init.headers).get("X-AURsmith-CSRF");
        allowCheck = false;
        return ok({ package_base: "demo", build_policy: { allow_check: false } });
      }
      if (url.endsWith("/packages/demo")) return ok({ package_base: "demo", version: "1-1", description: "演示", maintainer: "tester", outputs: ["demo"], build_policy: { allow_check: allowCheck }, revisions: [], dependency_resolution: [], events: [] });
      return ok({ items: [] });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "软件包" }));
    fireEvent.click(await screen.findByRole("button", { name: "详情" }));
    expect(await screen.findByRole("status")).toHaveTextContent("客户端不会按版本比较自动升级");
    fireEvent.click(screen.getByRole("button", { name: "禁用 check()" }));
    expect(await screen.findByText("已显式禁用")).toBeInTheDocument();
    expect(csrfHeader).toBe("1");
  });

  it("客户端页显示带外核对指纹和 pacman 配置", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/auth/me")) return ok({ id: "admin-id", username: "admin" });
      if (url.endsWith("/doctor")) return ok({ ready: true, checked_at: "", checks: [] });
      if (url.endsWith("/client-bootstrap")) return ok({ repository_config: "[aursmith]", gpg_fingerprint: "ABCD1234", gpg_key_url: "https://repo.test/key", client_ca_url: null, commands: ["pacman -Syu"], warnings: ["请带外核对"] });
      return ok({ items: [] });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "客户端" }));
    expect(await screen.findByText("ABCD1234")).toBeInTheDocument();
    expect(screen.getByText("[aursmith]")).toBeInTheDocument();
  });

  it("构建页可以查看失败任务的有界日志", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/auth/me")) return ok({ id: "admin-id", username: "admin" });
      if (url.endsWith("/doctor")) return ok({ ready: true, checked_at: "", checks: [] });
      if (url.endsWith("/jobs")) return ok({ items: [{ id: "11111111-1111-4111-8111-111111111111", kind: "build", required_role: "builder", status: "failed", priority: 40, failure_code: "GUEST_BUILD_FAILED", revision_sha256: "a".repeat(64), worker_name: "compute-local", attempt_count: 1, has_evidence: true, next_attempt_at: null, created_at: "2026-08-10T00:00:00Z", updated_at: "2026-08-10T00:01:00Z" }] });
      if (url.includes("/jobs/11111111-1111-4111-8111-111111111111/evidence")) return ok({ job_id: "11111111-1111-4111-8111-111111111111", kind: "build", sha256: "b".repeat(64), created_at: "2026-08-10T00:01:00Z", document: { status: "failed", logs: [{ path: "output/build.log", content_utf8: "compiler error" }] } });
      return ok({ items: [] });
    }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "构建" }));
    fireEvent.click(await screen.findByRole("button", { name: "查看日志" }));
    expect(await screen.findByText(/compiler error/)).toBeInTheDocument();
  });
});
