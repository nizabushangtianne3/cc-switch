import { describe, expect, it } from "vitest";
import type { ToolName } from "@/components/apps/useToolManagement";
import { uninstallCommandsFor } from "@/components/apps/toolUninstallCommands";

type UninstallEnvironment = "windows" | "macos" | "linux";

const envFor =
  (map: Partial<Record<ToolName, UninstallEnvironment>>) =>
  (tool: ToolName): UninstallEnvironment | undefined =>
    map[tool];

describe("uninstallCommandsFor", () => {
  it("keeps ungated commands no matter what the environment reports", () => {
    const commands = uninstallCommandsFor("npm", envFor({ claude: "windows" }));
    expect(commands.map((entry) => entry.tool)).toEqual([
      "dsh",
      "claude",
      "codex",
      "gemini",
      "grok",
      "opencode",
      "openclaw",
      "pi",
      "mcode",
      "kimi",
      "copilot",
    ]);
    expect(
      commands.every((entry) => entry.command.startsWith("npm uninstall -g ")),
    ).toBe(true);
  });

  it("drops a gated tool whose environment does not match", () => {
    // opencode 只登记了 macOS/Linux；Windows 上必须不出现在卸载清单里。
    const commands = uninstallCommandsFor(
      "bun",
      envFor({ opencode: "windows", openclaw: "windows", omp: "windows" }),
    );
    expect(commands.map((entry) => entry.tool)).toEqual(["openclaw", "omp"]);
  });

  it("keeps a gated tool once the environment matches", () => {
    const commands = uninstallCommandsFor("bun", envFor({ opencode: "macos" }));
    expect(commands.map((entry) => entry.tool)).toEqual([
      "opencode",
      "openclaw",
      "omp",
    ]);
    expect(commands[0].command).toBe("bun remove -g opencode-ai");
  });

  it("drops gated tools when the environment cannot be determined", () => {
    expect(uninstallCommandsFor("brew-formula", () => undefined)).toEqual([]);
  });

  it("returns every brew cask entry on macOS", () => {
    // claude 登记了两个 cask 变体，macOS 上都要给出。
    const commands = uninstallCommandsFor(
      "brew-cask",
      envFor({ claude: "macos", codex: "macos", agy: "macos" }),
    );
    expect(commands.map((entry) => entry.command)).toEqual([
      "brew uninstall --cask claude-code",
      "brew uninstall --cask claude-code@latest",
      "brew uninstall --cask codex",
      "brew uninstall --cask antigravity-cli",
    ]);
  });

  it("returns every brew formula entry on Linux", () => {
    const commands = uninstallCommandsFor(
      "brew-formula",
      envFor({ gemini: "linux", opencode: "linux" }),
    );
    expect(commands.map((entry) => entry.variant)).toEqual([
      undefined,
      "anomalyco/tap",
      "homebrew/core",
    ]);
  });

  it("returns the pnpm pair without environment gating", () => {
    const commands = uninstallCommandsFor("pnpm", () => "windows");
    expect(commands.map((entry) => entry.command)).toEqual([
      "pnpm remove -g opencode-ai",
      "pnpm remove -g openclaw",
    ]);
  });

  it("returns the single hermes command", () => {
    expect(uninstallCommandsFor("hermes", () => "windows")).toEqual([
      { tool: "hermes", command: "hermes uninstall --yes" },
    ]);
  });
});
