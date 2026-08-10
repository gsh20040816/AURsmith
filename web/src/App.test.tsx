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
});
