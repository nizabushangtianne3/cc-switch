import { describe, expect, it } from "vitest";
import type { TFunction } from "i18next";
import type { SkillRepoFailure } from "@/lib/api/skills";
import {
  describeRepoFailures,
  repoFailureKey,
} from "@/components/skills/repoFailures";

const t = ((key: string, options?: Record<string, unknown>) =>
  options && Object.keys(options).length
    ? `${key}:${JSON.stringify(options)}`
    : key) as unknown as TFunction;

const failure = (overrides: Partial<SkillRepoFailure>): SkillRepoFailure => ({
  owner: "owner",
  name: "name",
  branch: "main",
  error: "",
  ...overrides,
});

describe("repoFailureKey", () => {
  it("lowercases the owner/name pair", () => {
    expect(repoFailureKey({ owner: "Anthropic", name: "Skills" })).toBe(
      "anthropic/skills",
    );
  });
});

describe("describeRepoFailures", () => {
  it("renders one failure with its parsed reason", () => {
    const error = JSON.stringify({
      code: "DOWNLOAD_FAILED",
      context: { status: "403" },
      suggestion: "http403",
    });
    const text = describeRepoFailures(
      [failure({ owner: "a", name: "b", error })],
      t,
    );
    expect(text).toBe(
      'skillsPage.repoFail.item:{"repo":"a/b","reason":"skills.error.http403"}',
    );
  });

  it("falls back to the generic error text for an empty reason", () => {
    const text = describeRepoFailures([failure({ owner: "a", name: "b" })], t);
    expect(text).toBe(
      'skillsPage.repoFail.item:{"repo":"a/b","reason":"common.error"}',
    );
  });

  it("joins failures with the shared list separator", () => {
    const text = describeRepoFailures(
      [failure({ owner: "a", name: "b" }), failure({ owner: "C", name: "D" })],
      t,
    );
    expect(text).toBe(
      'skillsPage.repoFail.item:{"repo":"a/b","reason":"common.error"}' +
        "mcpPage.listSeparator" +
        'skillsPage.repoFail.item:{"repo":"C/D","reason":"common.error"}',
    );
  });

  it("returns an empty string when nothing failed", () => {
    expect(describeRepoFailures([], t)).toBe("");
  });
});
