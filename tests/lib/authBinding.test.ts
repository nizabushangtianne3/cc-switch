import { describe, expect, it } from "vitest";
import type { ProviderMeta } from "@/types";
import { resolveManagedAccountId } from "@/lib/authBinding";

// resolveManagedAccountId 被 ProviderCard / ProviderForm / OpenTerminalDialog /
// terminalCapabilities / CopilotQuotaFooter / UsageScriptModal /
// ClaudeDesktopProviderForm 七处共用：认错账号会把一家的用量记到另一家头上。

describe("resolveManagedAccountId", () => {
  it("returns the bound account id for a matching managed_account binding", () => {
    const meta: ProviderMeta = {
      authBinding: {
        source: "managed_account",
        authProvider: "codex",
        accountId: "acc-1",
      },
    };
    expect(resolveManagedAccountId(meta, "codex")).toBe("acc-1");
  });

  it("ignores a managed_account binding owned by a different auth provider", () => {
    const meta: ProviderMeta = {
      authBinding: {
        source: "managed_account",
        authProvider: "codex",
        accountId: "acc-1",
      },
      githubAccountId: "gh-1",
    };
    // 绑定不归属当前 authProvider 时不能泄露托管账号；唯一兜底是
    // github_copilot 的旧字段读取。
    expect(resolveManagedAccountId(meta, "github_copilot")).toBe("gh-1");
  });

  it("ignores bindings that come from provider_config", () => {
    const meta: ProviderMeta = {
      authBinding: {
        source: "provider_config",
        authProvider: "codex",
        accountId: "acc-1",
      },
    };
    expect(resolveManagedAccountId(meta, "codex")).toBeNull();
  });

  it("returns null when a managed_account binding carries no account id", () => {
    const meta: ProviderMeta = {
      authBinding: { source: "managed_account", authProvider: "codex" },
    };
    expect(resolveManagedAccountId(meta, "codex")).toBeNull();
  });

  it("falls back to the github account id for github_copilot", () => {
    const meta: ProviderMeta = { githubAccountId: "gh-9" };
    expect(resolveManagedAccountId(meta, "github_copilot")).toBe("gh-9");
  });

  it("returns null for github_copilot without a github account id", () => {
    expect(resolveManagedAccountId({}, "github_copilot")).toBeNull();
  });

  it("returns null when the provider has no meta at all", () => {
    expect(resolveManagedAccountId(undefined, "github_copilot")).toBeNull();
  });

  it("returns null for auth providers without a managed binding", () => {
    const meta: ProviderMeta = { githubAccountId: "gh-1" };
    expect(resolveManagedAccountId(meta, "claude")).toBeNull();
  });
});
