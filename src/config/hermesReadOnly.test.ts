import { describe, expect, it } from "vitest";
import { isHermesReadOnlyProvider } from "./hermesReadOnly";

// 断言用字面量而非导出常量：hermes_config.rs::get_providers 注入的就是
// `_cc_source = "providers_dict"`，测试要钉住这条前后端共线的 wire 约定。
describe("isHermesReadOnlyProvider", () => {
  it("treats the injected providers_dict marker as read-only", () => {
    expect(isHermesReadOnlyProvider({ _cc_source: "providers_dict" })).toBe(
      true,
    );
  });

  it("rejects non-object settingsConfig", () => {
    expect(isHermesReadOnlyProvider(null)).toBe(false);
    expect(isHermesReadOnlyProvider("providers_dict")).toBe(false);
    expect(isHermesReadOnlyProvider(7)).toBe(false);
    expect(isHermesReadOnlyProvider(undefined)).toBe(false);
  });

  it("rejects objects without the marker field", () => {
    expect(isHermesReadOnlyProvider({})).toBe(false);
    expect(isHermesReadOnlyProvider({ env: { PATH: "/usr/bin" } })).toBe(false);
  });

  it("rejects any marker value other than providers_dict", () => {
    expect(isHermesReadOnlyProvider({ _cc_source: "manual" })).toBe(false);
    expect(isHermesReadOnlyProvider({ _cc_source: "" })).toBe(false);
  });
});
