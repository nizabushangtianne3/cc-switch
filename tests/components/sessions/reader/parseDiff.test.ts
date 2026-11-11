import { describe, expect, it } from "vitest";
import {
  parseDiff,
  type DiffLine,
} from "@/components/sessions/reader/SessionDiff";

/** 只留 type/text/行号，方便对结构做断言。 */
const shape = (lines: DiffLine[]) =>
  lines.map((line) => {
    if (line.type === "file") return { type: "file", path: line.path };
    if (line.type === "gap") return { type: "gap" };
    return {
      type: line.type,
      text: line.text,
      oldNo: line.oldNo ?? null,
      newNo: line.newNo ?? null,
    };
  });

describe("parseDiff — unified diff", () => {
  it("reads a git diff hunk into file/add/del/context with line numbers", () => {
    const diff = [
      "diff --git a/src/a.ts b/src/a.ts",
      "index 1234567..89abcde 100644",
      "--- a/src/a.ts",
      "+++ b/src/a.ts",
      "@@ -1,4 +1,5 @@",
      " keep one",
      "-remove me",
      "+add me",
      " keep two",
    ].join("\n");

    expect(shape(parseDiff(diff))).toEqual([
      { type: "file", path: "src/a.ts" },
      { type: "context", text: "keep one", oldNo: 1, newNo: 1 },
      { type: "del", text: "remove me", oldNo: 2, newNo: null },
      { type: "add", text: "add me", oldNo: null, newNo: 2 },
      { type: "context", text: "keep two", oldNo: 3, newNo: 3 },
    ]);
  });

  it("strips the b/ prefix from the +++ header", () => {
    const diff = [
      "--- a/x/y.ts",
      "+++ b/x/y.ts",
      "@@ -1 +1 @@",
      "+a",
      "-b",
    ].join("\n");

    expect(parseDiff(diff)[0]).toEqual({ type: "file", path: "x/y.ts" });
  });

  it("ignores the --- header when the next line is not a +++ partner", () => {
    // 内容是「以 -- 开头的改动行」而不是文件头，所以整行都是内容
    const diff = ["--- standalone", "+++ also-standalone"].join("\n");

    // --- standalone 的下一条是 +++ also-standalone，成对，于是按文件头处理。
    // 这里改成不成对：第一条后面没有 +++，于是当内容。
    const notPaired = ["--- lonely", "context"].join("\n");
    // 注意：孤立 `--- x` 会先落进「- 开头」的 del 分支，所以内容是 `-- lonely`
    // （剥掉一个 `-`），不是 context。
    expect(shape(parseDiff(notPaired))).toEqual([
      { type: "del", text: "-- lonely", oldNo: null, newNo: null },
      { type: "context", text: "context", oldNo: null, newNo: null },
    ]);
    // 对照组：成对时被吃掉，只剩文件头
    expect(shape(parseDiff(diff))).toEqual([
      { type: "file", path: "also-standalone" },
    ]);
  });

  it("drops /dev/null file targets", () => {
    const diff = [
      "--- a/src/gone.ts",
      "+++ /dev/null",
      "@@ -1 +0,0 @@",
      "-x",
    ].join("\n");

    expect(shape(parseDiff(diff))).toEqual([
      { type: "del", text: "x", oldNo: 1, newNo: null },
    ]);
  });

  it("keeps an index line that lands inside a hunk (content starting with 'index ')", () => {
    const diff = [
      "--- a/f.ts",
      "+++ b/f.ts",
      "@@ -1,2 +1,2 @@",
      " index this is prose, not a git index line",
      "+done",
    ].join("\n");

    const lines = shape(parseDiff(diff));

    expect(lines).toContainEqual({
      type: "context",
      text: "index this is prose, not a git index line",
      oldNo: 1,
      newNo: 1,
    });
  });

  it("skips the index line before the first hunk", () => {
    const diff = [
      "--- a/f.ts",
      "+++ b/f.ts",
      "index 0000000..1111111 100644",
      "@@ -1 +1 @@",
      "+x",
    ].join("\n");

    const texts = shape(parseDiff(diff))
      .map((l) => ("text" in l ? l.text : ""))
      .join("\n");
    expect(texts).not.toContain("0000000");
  });

  it("inserts a gap between hunks and restarts the numbering", () => {
    const diff = [
      "--- a/f.ts",
      "+++ b/f.ts",
      "@@ -1,2 +1,2 @@",
      " one",
      "+two",
      "@@ -20,2 +20,3 @@",
      " twenty",
      "+twentyone",
    ].join("\n");

    expect(shape(parseDiff(diff))).toEqual([
      { type: "file", path: "f.ts" },
      { type: "context", text: "one", oldNo: 1, newNo: 1 },
      { type: "add", text: "two", oldNo: null, newNo: 2 },
      { type: "gap" },
      { type: "context", text: "twenty", oldNo: 20, newNo: 20 },
      { type: "add", text: "twentyone", oldNo: null, newNo: 21 },
    ]);
  });

  it("does not insert a gap before the first hunk", () => {
    const diff = ["--- a/f.ts", "+++ b/f.ts", "@@ -1 +1 @@", "+only"].join(
      "\n",
    );

    expect(parseDiff(diff).some((l) => l.type === "gap")).toBe(false);
  });

  it("reads the optional ,count part of the hunk header and the trailing prose", () => {
    const diff = ["@@ -3,7 +12,9 @@ function main()", " three", "+four"].join(
      "\n",
    );

    expect(shape(parseDiff(diff))).toEqual([
      { type: "context", text: "three", oldNo: 3, newNo: 12 },
      { type: "add", text: "four", oldNo: null, newNo: 13 },
    ]);
  });

  it("leaves line numbers unset when the hunk header does not parse", () => {
    const diff = ["@@ garbage @@", "+a", "-b", " c"].join("\n");

    // oldNo/newNo 从没被赋值过，于是 del 与 context 都不带行号、也不递增。
    expect(shape(parseDiff(diff))).toEqual([
      { type: "add", text: "a", oldNo: null, newNo: null },
      { type: "del", text: "b", oldNo: null, newNo: null },
      { type: "context", text: "c", oldNo: null, newNo: null },
    ]);
  });

  it("does not number a del line when the old side is unknown", () => {
    // @@ 无法解析 => oldNo/newNo 都 undefined，del 行不递增
    const diff = ["@@ bad @@", "-one", "-two"].join("\n");

    expect(shape(parseDiff(diff))).toEqual([
      { type: "del", text: "one", oldNo: null, newNo: null },
      { type: "del", text: "two", oldNo: null, newNo: null },
    ]);
  });

  it("trims trailing blank context lines but keeps blank lines in the middle", () => {
    const diff = ["@@ -1,3 +1,3 @@", " one", "", " two", ""].join("\n");

    expect(shape(parseDiff(diff))).toEqual([
      { type: "context", text: "one", oldNo: 1, newNo: 1 },
      { type: "context", text: "", oldNo: 2, newNo: 2 },
      { type: "context", text: "two", oldNo: 3, newNo: 3 },
    ]);
  });

  it("ignores leading blank lines before any content", () => {
    expect(shape(parseDiff("\n\n+later"))).toEqual([
      { type: "add", text: "later", oldNo: null, newNo: null },
    ]);
  });

  it("accepts CRLF input", () => {
    const diff = "--- a/f.ts\r\n+++ b/f.ts\r\n@@ -1 +1 @@\r\n+x";

    expect(shape(parseDiff(diff))).toEqual([
      { type: "file", path: "f.ts" },
      { type: "add", text: "x", oldNo: null, newNo: 1 },
    ]);
  });

  it("drops git metadata and the no-newline marker", () => {
    const diff = [
      "diff --git a/f b/f",
      "index 1..2 100644",
      "--- a/f",
      "+++ b/f",
      "@@ -1 +1 @@",
      "-old",
      "\\ No newline at end of file",
      "+new",
    ].join("\n");

    const texts = shape(parseDiff(diff))
      .map((l) => ("text" in l ? l.text : ""))
      .join("\n");
    expect(texts).not.toContain("No newline");
  });
});

describe("parseDiff — apply_patch", () => {
  it("reads Add/Update/Delete File headers and resets numbering", () => {
    const patch = [
      "*** Begin Patch",
      "*** Update File: src/app.ts",
      "@@ -2,2 +2,3 @@",
      " two",
      "+added",
      "*** Add File: src/new.ts",
      "@@ -0,0 +1,2 @@",
      "+one",
      "+two",
      "*** Move to: src/moved.ts",
      "*** End Patch",
    ].join("\n");

    expect(shape(parseDiff(patch))).toEqual([
      { type: "file", path: "src/app.ts" },
      { type: "context", text: "two", oldNo: 2, newNo: 2 },
      { type: "add", text: "added", oldNo: null, newNo: 3 },
      { type: "file", path: "src/new.ts" },
      { type: "add", text: "one", oldNo: null, newNo: 1 },
      { type: "add", text: "two", oldNo: null, newNo: 2 },
      { type: "file", path: "src/moved.ts" },
    ]);
  });

  it("skips the Begin/End Patch markers", () => {
    const patch = [
      "*** Begin Patch",
      "@@ -1 +1 @@",
      "+x",
      "*** End Patch",
    ].join("\n");

    expect(shape(parseDiff(patch))).toEqual([
      { type: "add", text: "x", oldNo: null, newNo: 1 },
    ]);
  });

  it("keeps an 'index ' line that appears inside a patch hunk", () => {
    const patch = [
      "*** Update File: f.ts",
      "@@ -1 +1 @@",
      " index lookup helper",
      "+added",
    ].join("\n");

    expect(shape(parseDiff(patch))).toEqual([
      { type: "file", path: "f.ts" },
      { type: "context", text: "index lookup helper", oldNo: 1, newNo: 1 },
      { type: "add", text: "added", oldNo: null, newNo: 2 },
    ]);
  });
});

describe("parseDiff — file headers", () => {
  it("collapses two consecutive headers for the same path", () => {
    const diff = [
      "--- a/f.ts",
      "+++ b/f.ts",
      "--- a/f.ts",
      "+++ b/f.ts",
      "@@ -1 +1 @@",
      "+x",
    ].join("\n");

    expect(parseDiff(diff).filter((l) => l.type === "file")).toHaveLength(1);
  });

  it("keeps two headers when the path changes", () => {
    const diff = [
      "--- a/f.ts",
      "+++ b/f.ts",
      "@@ -1 +1 @@",
      "+x",
      "--- a/g.ts",
      "+++ b/g.ts",
      "@@ -1 +1 @@",
      "+y",
    ].join("\n");

    expect(shape(parseDiff(diff)).filter((l) => l.type === "file")).toEqual([
      { type: "file", path: "f.ts" },
      { type: "file", path: "g.ts" },
    ]);
  });
});

describe("parseDiff — empty input", () => {
  it("returns nothing for an empty string", () => {
    expect(parseDiff("")).toEqual([]);
  });

  it("returns nothing for whitespace-only input", () => {
    // 整行空白不算「空上下文行」：剥掉行首一个空格后还剩两个空格，
    // 末尾修剪只剥 text === "" 的那些，所以会留下一条 context。
    expect(shape(parseDiff("   "))).toEqual([
      { type: "context", text: "  ", oldNo: null, newNo: null },
    ]);
  });
});
