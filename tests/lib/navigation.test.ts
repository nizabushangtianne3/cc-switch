import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  appPageBelongsTo,
  isAppPage,
  parseView,
  readStoredView,
  sharedFeatureAppOf,
  storeView,
  VIEW_STORAGE_KEY,
} from "@/lib/navigation";

describe("parseView", () => {
  it("keeps every current view name", () => {
    for (const view of [
      "providers",
      "workspace",
      "openclawConfig",
      "hermesMemory",
      "usage",
      "auth",
      "mcp",
      "skills",
      "prompts",
      "sessions",
      "apps",
      "settings",
    ]) {
      expect(parseView(view)).toBe(view);
    }
  });

  it("folds the three legacy OpenClaw pages into one", () => {
    for (const legacy of [
      "openclawEnv",
      "openclawTools",
      "openclawAgents",
    ]) {
      expect(parseView(legacy)).toBe("openclawConfig");
    }
  });

  it("rejects nothing-shaped values instead of guessing", () => {
    for (const value of [null, undefined, "", "  ", "nope", "Usage", "provider"]) {
      expect(parseView(value)).toBeNull();
    }
  });
});

describe("isAppPage", () => {
  it("separates app pages from global pages and settings", () => {
    expect(isAppPage("providers")).toBe(true);
    expect(isAppPage("workspace")).toBe(true);
    expect(isAppPage("openclawConfig")).toBe(true);
    expect(isAppPage("hermesMemory")).toBe(true);
    expect(isAppPage("usage")).toBe(false);
    expect(isAppPage("mcp")).toBe(false);
    expect(isAppPage("settings")).toBe(false);
  });
});

describe("appPageBelongsTo", () => {
  it("keeps the provider page for every app", () => {
    for (const app of ["claude", "codex", "openclaw", "hermes"] as const) {
      expect(appPageBelongsTo("providers", app)).toBe(true);
    }
  });

  it("scopes the exclusive pages to their own app", () => {
    expect(appPageBelongsTo("workspace", "openclaw")).toBe(true);
    expect(appPageBelongsTo("workspace", "claude")).toBe(false);
    expect(appPageBelongsTo("openclawConfig", "openclaw")).toBe(true);
    expect(appPageBelongsTo("openclawConfig", "hermes")).toBe(false);
    expect(appPageBelongsTo("hermesMemory", "hermes")).toBe(true);
    expect(appPageBelongsTo("hermesMemory", "openclaw")).toBe(false);
  });
});

describe("sharedFeatureAppOf", () => {
  it("points Claude Desktop at the Claude feature pages", () => {
    expect(sharedFeatureAppOf("claude-desktop")).toBe("claude");
  });

  it("leaves every other app alone", () => {
    for (const app of [
      "claude",
      "codex",
      "gemini",
      "openclaw",
      "hermes",
    ] as const) {
      expect(sharedFeatureAppOf(app)).toBe(app);
    }
  });
});

describe("the stored view", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("falls back to the provider page when nothing is stored", () => {
    expect(readStoredView()).toBe("providers");
  });

  it("round-trips a stored view", () => {
    storeView("usage");
    expect(localStorage.getItem(VIEW_STORAGE_KEY)).toBe("usage");
    expect(readStoredView()).toBe("usage");
  });

  it("maps a legacy view through the same parser", () => {
    localStorage.setItem(VIEW_STORAGE_KEY, "openclawAgents");
    expect(readStoredView()).toBe("openclawConfig");
  });

  it("falls back when the stored value is junk", () => {
    localStorage.setItem(VIEW_STORAGE_KEY, "junk");
    expect(readStoredView()).toBe("providers");
  });

  it("swallows a storage that refuses to be read or written", () => {
    const getItem = vi
      .spyOn(Storage.prototype, "getItem")
      .mockImplementation(() => {
        throw new Error("denied");
      });
    expect(readStoredView()).toBe("providers");
    getItem.mockRestore();

    const setItem = vi
      .spyOn(Storage.prototype, "setItem")
      .mockImplementation(() => {
        throw new Error("denied");
      });
    expect(() => storeView("usage")).not.toThrow();
    setItem.mockRestore();
  });
});
