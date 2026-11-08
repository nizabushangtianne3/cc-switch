import { describe, expect, it } from "vitest";
import { formatJSON } from "@/utils/formatters";

describe("formatJSON", () => {
  it("pretty-prints an object with two-space indent", () => {
    expect(formatJSON('{"b":1,"a":[1,2]}')).toBe(
      '{\n  "b": 1,\n  "a": [\n    1,\n    2\n  ]\n}',
    );
  });

  it("trims surrounding whitespace before parsing", () => {
    expect(formatJSON('  \n {"a":1} \n ')).toBe('{\n  "a": 1\n}');
  });

  it("returns an empty string for blank input", () => {
    expect(formatJSON("")).toBe("");
    expect(formatJSON("   \n  ")).toBe("");
  });

  it("normalizes scalars and arrays too", () => {
    expect(formatJSON("1")).toBe("1");
    expect(formatJSON("[1,2]")).toBe("[\n  1,\n  2\n]");
  });

  it("throws on invalid JSON so the caller can report it", () => {
    expect(() => formatJSON("{oops")).toThrow();
  });
});
