import { describe, expect, it } from "vitest";
import type { PerApp, Profile, ProfilePayload } from "@/lib/api/profiles";
import {
  APP_PROFILE_SCOPE,
  hasScopeSnapshot,
} from "@/components/profiles/scope";

// scope.ts 是后端 ProfileScope::for_app / apps 的前端镜像：认错槽位会把 A
// 项目的快照应用到 B 项目上。

const perApp = <T>(value: T): PerApp<T> => ({
  claude: value,
  "claude-desktop": value,
  codex: value,
});

const profileWith = (overrides: Partial<ProfilePayload>): Profile => ({
  id: "p1",
  name: "P1",
  payload: {
    providers: perApp(null),
    mcp: perApp(null),
    skills: perApp(null),
    prompts: perApp(null),
    ...overrides,
  },
});

describe("hasScopeSnapshot", () => {
  it("reports no snapshot when every slot of the scope is null", () => {
    expect(hasScopeSnapshot(profileWith({}), "codex")).toBe(false);
  });

  it("counts a snapshot once the scope's providers slot holds an id", () => {
    const profile = profileWith({
      providers: { ...perApp<string | null>(null), codex: "prov-1" },
    });
    expect(hasScopeSnapshot(profile, "codex")).toBe(true);
  });

  it("counts a snapshot for an empty list, which is not the same as never", () => {
    // 空数组 = 拍到过且当时一个都没有（应用时会清空）；null = 从未拍过。
    const profile = profileWith({ skills: perApp([]) });
    expect(hasScopeSnapshot(profile, "claude")).toBe(true);
  });

  it("counts a snapshot from any of the four payload kinds", () => {
    const prompts = profileWith({ prompts: perApp("PROMPT.md") });
    expect(hasScopeSnapshot(prompts, "claude-desktop")).toBe(true);
    const mcp = profileWith({
      mcp: { ...perApp<string[] | null>(null), "claude-desktop": ["a"] },
    });
    expect(hasScopeSnapshot(mcp, "claude-desktop")).toBe(true);
  });

  it("ignores slots that belong to other scopes", () => {
    const profile = profileWith({
      providers: {
        ...perApp<string | null>(null),
        "claude-desktop": "prov-1",
      },
    });
    expect(hasScopeSnapshot(profile, "codex")).toBe(false);
  });
});

describe("APP_PROFILE_SCOPE", () => {
  it("mirrors the backend scope map for the supported apps", () => {
    expect(APP_PROFILE_SCOPE).toEqual({
      claude: "claude",
      "claude-desktop": "claude-desktop",
      codex: "codex",
    });
  });

  it("leaves apps without profile support unmapped", () => {
    expect(APP_PROFILE_SCOPE.gemini).toBeUndefined();
  });
});
