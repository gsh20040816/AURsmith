import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

describe("AURsmith 控制台", () => {
  beforeEach(() => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const body = url.endsWith("/setup/status")
        ? { initialized: true }
        : url.endsWith("/auth/me")
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
  });

  it("Worker 页面提供探测注册表单", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Worker/ }));
    expect(await screen.findByText("注册 Worker")).toBeInTheDocument();
    expect(screen.getByLabelText("实例名称")).toBeInTheDocument();
    expect(screen.getByLabelText("SSH host key 指纹")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "探测并注册" })).toBeInTheDocument();
  });

  it("Profile 页面可以提交构建候选", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Profile/ }));
    expect(await screen.findByText("授权 Profile candidate")).toBeInTheDocument();
    expect(screen.getByLabelText("profile-candidate.json")).toHaveAttribute("type", "file");
    expect(screen.getByRole("button", { name: "提交并创建 fixture" })).toBeInTheDocument();
  });

  it("软件包详情可以显式禁用 check", async () => {
    let allowCheck = true;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      let body: unknown;
      if (url.endsWith("/setup/status")) body = { initialized: true };
      else if (url.endsWith("/auth/me")) body = { id: "admin-id", username: "admin" };
      else if (url.endsWith("/requirements")) body = { items: [] };
      else if (url.endsWith("/subscriptions")) body = { items: [{ id: "sub", package_base: "demo", kind: "direct", state: "active", reference_count: 0, followed_outputs: ["demo"], version: "1-1", description: "演示", outputs: ["demo"], maintainer: "tester", out_of_date: null }] };
      else if (url.endsWith("/packages/demo/build-policy") && init?.method === "POST") {
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
  });
});
