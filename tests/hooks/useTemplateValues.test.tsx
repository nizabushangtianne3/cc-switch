import { describe, expect, it } from "vitest";
import {
  applyTemplateValuesToConfigString,
  collectTemplatePaths,
  getValueAtPath,
  setValueAtPath,
  type TemplatePath,
  type TemplateValueMap,
} from "@/components/providers/forms/hooks/useTemplateValues";
import type { TemplateValueConfig } from "@/config/claudeProviderPresets";

// 真实路径里 editorValue 总是被填上的（hook 初始化时就写成 defaultValue），
// applyTemplateValues 也优先读它而不是 defaultValue，所以夹具得照这个形状来。
const templateValue = (
  label: string,
  defaultValue = "",
  editorValue = defaultValue,
): TemplateValueConfig => ({
  label,
  placeholder: "",
  defaultValue,
  editorValue,
});

const nestedPath: TemplatePath = ["a", "b", 1, "c"];
const templateTable: TemplateValueMap = {
  key: templateValue("API Key", "sk-filled"),
  other: templateValue("Other", "v2"),
};

describe("collectTemplatePaths", () => {
  it("collects the path of every string that carries a placeholder", () => {
    const source = {
      env: { A: "${key}", B: "plain", C: 7, D: null },
      tools: [{ name: "${key}" }, { name: "fine" }],
    };

    expect(collectTemplatePaths(source, ["key"])).toEqual([
      ["env", "A"],
      ["tools", 0, "name"],
    ]);
  });

  it("only matches the exact placeholder syntax, not a bare dollar word", () => {
    const source = { a: "$key", b: "${key", c: "x${key}y" };

    expect(collectTemplatePaths(source, ["key"])).toEqual([["c"]]);
  });

  it("collects the whole document when the root itself is a placeholder", () => {
    expect(collectTemplatePaths("${key}", ["key"])).toEqual([[]]);
  });

  it("collects array indices as array elements", () => {
    expect(collectTemplatePaths(["a", "${key}"], ["key"])).toEqual([[1]]);
  });

  it("returns nothing when no key is templated", () => {
    expect(collectTemplatePaths({ a: { b: "${other}" } }, ["key"])).toEqual([]);
  });
});

describe("getValueAtPath", () => {
  it("walks into nested objects and arrays", () => {
    const source = { a: { b: [{ c: 1 }, { c: 2 }] } };

    expect(getValueAtPath(source, nestedPath)).toBe(2);
  });

  it("returns the document itself for an empty path", () => {
    const source = { a: 1 };

    expect(getValueAtPath(source, [])).toBe(source);
  });

  it("short-circuits to undefined when a mid-path node is missing or null", () => {
    expect(getValueAtPath({ a: null }, ["a", "b"])).toBeUndefined();
    expect(getValueAtPath({}, ["a", "b", "c"])).toBeUndefined();
  });
});

describe("setValueAtPath", () => {
  it("creates missing objects along the way", () => {
    const target: Record<string, unknown> = {};

    setValueAtPath(target, ["a", "b", "c"], 5);
    expect(target).toEqual({ a: { b: { c: 5 } } });
  });

  it("creates missing arrays when the next key is numeric", () => {
    const target: Record<string, unknown> = {};

    setValueAtPath(target, ["list", 0, "name"], "x");
    expect(target).toEqual({ list: [{ name: "x" }] });
  });

  it("replaces a non-container when the path needs an object", () => {
    setValueAtPath({ a: 7 } as Record<string, unknown>, ["a", "b"], 1);
    const target: Record<string, unknown> = { a: 7 };
    setValueAtPath(target, ["a", "b"], 1);
    expect(target).toEqual({ a: { b: 1 } });
  });

  it("replaces a non-array when the path needs an array", () => {
    const target: Record<string, unknown> = { a: { b: "scalar" } };

    setValueAtPath(target, ["a", "b", 0], "first");
    expect(target).toEqual({ a: { b: ["first"] } });
  });

  it("returns the value itself when the path is empty", () => {
    const target: Record<string, unknown> = { a: 1 };

    expect(setValueAtPath(target, [], "whole")).toBe("whole");
  });
});

describe("applyTemplateValuesToConfigString", () => {
  const values = templateTable;

  it("rewrites only the placeholder fields and keeps the rest of the current config", () => {
    const presetConfig = {
      env: { KEY: "${key}", NOTE: "keep me", OTHER: "${other}" },
      untouched: { deep: [1, 2] },
    };
    const currentConfig = JSON.stringify(
      { env: { KEY: "", OTHER: "", NOTE: "keep me" }, extra: "mine" },
      null,
      2,
    );

    const result = JSON.parse(
      applyTemplateValuesToConfigString(presetConfig, currentConfig, values),
    );

    expect(result.env).toEqual({
      KEY: "sk-filled",
      OTHER: "v2",
      NOTE: "keep me",
    });
    // 用户在当前配置里额外加的字段不能丢
    expect(result.extra).toBe("mine");
    expect(result.untouched).toBeUndefined();
  });

  it("keeps the current config untouched when the preset has no placeholder", () => {
    const presetConfig = { env: { KEY: "literal" } };
    const currentConfig = JSON.stringify({ env: { KEY: "kept" } }, null, 2);

    expect(
      applyTemplateValuesToConfigString(presetConfig, currentConfig, values),
    ).toBe(currentConfig);
  });

  it("falls back to the filled preset config when there are no template values", () => {
    const presetConfig = { env: { KEY: "${key}" } };

    expect(
      applyTemplateValuesToConfigString(presetConfig, "{ not json", {}),
    ).toBe(JSON.stringify(presetConfig, null, 2));
  });

  it("treats a blank current config as an empty object", () => {
    const presetConfig = { env: { KEY: "${key}" } };

    expect(
      JSON.parse(
        applyTemplateValuesToConfigString(presetConfig, "   ", values),
      ),
    ).toEqual({ env: { KEY: "sk-filled" } });
  });

  it("replaces the whole document when the placeholder sits at the root", () => {
    const currentConfig = JSON.stringify({ stale: true }, null, 2);

    expect(
      JSON.parse(
        applyTemplateValuesToConfigString("${key}", currentConfig, values),
      ),
    ).toBe("sk-filled");
  });

  it("preserves an array current config instead of freezing it at the first run", () => {
    const presetConfig = [{ name: "${key}" }];
    const currentConfig = JSON.stringify(
      [{ name: "" }, { name: "second" }],
      null,
      2,
    );

    const result = JSON.parse(
      applyTemplateValuesToConfigString(presetConfig, currentConfig, values),
    );

    expect(result).toEqual([{ name: "sk-filled" }, { name: "second" }]);
  });

  it("builds objects whose mid-path node stayed absent in the current config", () => {
    const presetConfig = { a: { b: { c: "${key}" } } };
    const currentConfig = JSON.stringify({ a: {} }, null, 2);

    expect(
      JSON.parse(
        applyTemplateValuesToConfigString(presetConfig, currentConfig, values),
      ),
    ).toEqual({ a: { b: { c: "sk-filled" } } });
  });

  it("falls back to the filled preset config when the current config is not JSON", () => {
    const presetConfig = { env: { KEY: "${key}" } };

    expect(
      applyTemplateValuesToConfigString(
        presetConfig,
        "definitely not json",
        values,
      ),
    ).toBe(JSON.stringify({ env: { KEY: "sk-filled" } }, null, 2));
  });
});
