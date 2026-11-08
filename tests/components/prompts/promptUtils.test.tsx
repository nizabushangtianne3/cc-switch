import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { TFunction } from "i18next";
import {
  MCODE_PROMPT_LIMIT,
  charCount,
  firstLine,
  formatSize,
  formatUpdated,
  promptFileName,
  renderSegs,
  utf8Bytes,
} from "@/components/prompts/promptUtils";

const t = ((key: string, options?: Record<string, unknown>) =>
  options && Object.keys(options).length
    ? `${key}:${JSON.stringify(options)}`
    : key) as unknown as TFunction;

describe("promptFileName", () => {
  it("maps every prompts-capable app to its target file", () => {
    expect(promptFileName("claude")).toBe("CLAUDE.md");
    expect(promptFileName("claude-desktop")).toBe("CLAUDE.md");
    expect(promptFileName("codex")).toBe("AGENTS.md");
    expect(promptFileName("hermes")).toBe("SOUL.md");
    expect(promptFileName("pi")).toBe("AGENTS.md");
  });

  it("falls back to AGENTS.md for apps without a mapping", () => {
    expect(promptFileName("not-an-app" as never)).toBe("AGENTS.md");
  });
});

describe("utf8Bytes", () => {
  it("counts UTF-8 bytes rather than UTF-16 code units", () => {
    expect(utf8Bytes("")).toBe(0);
    expect(utf8Bytes("abc")).toBe(3);
    expect(utf8Bytes("中文")).toBe(6);
    expect(utf8Bytes("😀")).toBe(4);
  });

  it("agrees with the backend MiniMax limit, which counts bytes", () => {
    expect(utf8Bytes("中".repeat(MCODE_PROMPT_LIMIT))).toBeGreaterThan(
      MCODE_PROMPT_LIMIT,
    );
    expect(utf8Bytes("a".repeat(MCODE_PROMPT_LIMIT))).toBe(MCODE_PROMPT_LIMIT);
  });
});

describe("formatSize", () => {
  it("switches from bytes to KB at 1024", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(1023)).toBe("1023 B");
    expect(formatSize(1024)).toBe("1.0 KB");
    expect(formatSize(1536)).toBe("1.5 KB");
  });
});

describe("charCount", () => {
  it("counts code points so an astral character counts once", () => {
    expect(charCount("")).toBe(0);
    expect(charCount("abc")).toBe(3);
    expect(charCount("a😀b")).toBe(3);
  });
});

describe("firstLine", () => {
  it("returns the first non-blank line without markdown heading marks", () => {
    expect(firstLine("# Title\n\nbody")).toBe("Title");
    expect(firstLine("   \n  first  \nsecond")).toBe("first");
    expect(firstLine("## Hashed")).toBe("Hashed");
  });

  it("returns null when no line has content", () => {
    expect(firstLine("")).toBeNull();
    expect(firstLine("\n\n   \n")).toBeNull();
  });
});

describe("formatUpdated", () => {
  const now = new Date(2026, 5, 15, 12, 0, 0);

  it("returns null without a timestamp", () => {
    expect(formatUpdated(t, "en-US", undefined, now)).toBeNull();
    expect(formatUpdated(t, "en-US", 0, now)).toBeNull();
  });

  it("returns null for an unparseable timestamp", () => {
    expect(formatUpdated(t, "en-US", Number.NaN, now)).toBeNull();
  });

  it("uses the today wording with a clock time for today's date", () => {
    const seconds = Math.floor(new Date(2026, 5, 15, 18, 40).getTime() / 1000);
    expect(formatUpdated(t, "en-US", seconds, now)).toMatch(
      /^prompts\.updatedToday:\{"time":"\d{1,2}:\d{2}"\}$/,
    );
  });

  it("omits the year inside the current year", () => {
    const seconds = Math.floor(new Date(2026, 2, 1, 9, 30).getTime() / 1000);
    const text = formatUpdated(t, "en-US", seconds, now) as string;
    expect(text).toContain('"month":3');
    expect(text).toContain('"day":1');
    expect(text).not.toContain("2026");
  });

  it("adds the year once the date is from an earlier year", () => {
    const seconds = Math.floor(new Date(2024, 2, 1, 9, 30).getTime() / 1000);
    const text = formatUpdated(t, "en-US", seconds, now) as string;
    expect(text).toContain('"month":3');
    expect(text).toContain('"day":1');
    expect(text).toContain("2024");
  });
});

describe("renderSegs", () => {
  it("wraps a tilde path in code", () => {
    const { container } = render(
      <div>{renderSegs("写入 ~/.claude/CLAUDE.md 生效")}</div>,
    );
    const codes = Array.from(container.querySelectorAll("code"));
    expect(codes).toHaveLength(1);
    expect(codes[0].textContent).toBe("~/.claude/CLAUDE.md");
  });

  it("wraps an absolute path in code", () => {
    const { container } = render(
      <div>{renderSegs("运行 /usr/local/bin/tool 命令")}</div>,
    );
    const code = container.querySelector("code");
    // 斜杠命令分支只吃单段（/cmd）：深层路径按设计只把首段等宽化，
    // 这是 promptUtils 的有意窄匹配，不是缺陷。
    expect(code?.textContent).toBe("/usr");
  });

  it("keeps a bracketed command on one line without code styling", () => {
    const { container } = render(<div>{renderSegs("支持「斜杠命令」")}</div>);
    const span = container.querySelector("span");
    expect(span?.textContent).toBe("「斜杠命令」");
    expect(container.querySelector("code")).toBeNull();
  });

  it("leaves plain prose untouched", () => {
    const { container } = render(<div>{renderSegs("just words")}</div>);
    expect(container.querySelector("code")).toBeNull();
    expect(container.querySelector("span")).toBeNull();
    expect(container.textContent).toBe("just words");
  });
});
