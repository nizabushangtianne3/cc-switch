import { describe, expect, it } from "vitest";
import {
  extraSnippetsOf,
  getSessionRowHeight,
} from "@/components/sessions/SessionItem";
import type { SessionContentHit } from "@/types";

const hit = (snippets: number): SessionContentHit => ({
  providerId: "p",
  sourcePath: "/tmp/x.jsonl",
  snippets: Array.from({ length: snippets }, (_, i) => ({
    messageIndex: i,
    text: `snippet ${i}`,
    highlights: [],
  })),
  matchCount: snippets,
});

describe("extraSnippetsOf", () => {
  it("drops the first snippet, which is already shown inside the row", () => {
    expect(extraSnippetsOf(hit(3), false)).toHaveLength(2);
    expect(extraSnippetsOf(hit(3), false)[0].text).toBe("snippet 1");
  });

  it("returns nothing when there is only the primary snippet", () => {
    expect(extraSnippetsOf(hit(1), false)).toEqual([]);
    expect(extraSnippetsOf(hit(0), false)).toEqual([]);
  });

  it("returns nothing when there is no hit at all", () => {
    expect(extraSnippetsOf(undefined, false)).toEqual([]);
  });

  it("suppresses snippets entirely in selection mode", () => {
    // 选择模式下不显示摘录，行高也要跟着收回基础高度
    expect(extraSnippetsOf(hit(3), true)).toEqual([]);
    expect(extraSnippetsOf(hit(0), true)).toEqual([]);
  });
});

describe("getSessionRowHeight", () => {
  it("uses the base height when no extra snippet is listed", () => {
    expect(getSessionRowHeight(hit(1), false)).toBe(56);
    expect(getSessionRowHeight(hit(0), false)).toBe(56);
    expect(getSessionRowHeight(undefined, false)).toBe(56);
  });

  it("adds a fixed slice per extra snippet plus one padding block", () => {
    // 56 基础 + 2 * 20 每条 + 6 内边距
    expect(getSessionRowHeight(hit(3), false)).toBe(56 + 2 * 20 + 6);
    expect(getSessionRowHeight(hit(4), false)).toBe(56 + 3 * 20 + 6);
  });

  it("returns the base height in selection mode regardless of the hit", () => {
    // 与 extraSnippetsOf(selectionMode=true) 为空保持一致，否则滚动会漂
    expect(getSessionRowHeight(hit(5), true)).toBe(56);
  });

  it("keeps the height in step with the extras count", () => {
    for (const count of [0, 1, 2, 3, 5, 8]) {
      const extras = extraSnippetsOf(hit(count), false).length;
      expect(getSessionRowHeight(hit(count), false)).toBe(
        extras === 0 ? 56 : 56 + extras * 20 + 6,
      );
    }
  });
});
