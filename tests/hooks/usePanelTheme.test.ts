import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { usePanelTheme } from "@/tray-panel/usePanelTheme";

const THEME_STORAGE_KEY = "cc-switch-theme";

/** 按需伪造 matchMedia：jsdom 没有它，而 systemDark() 走的是可选链。 */
function stubMatchMedia(dark: boolean) {
  const listener = vi.fn();
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: dark,
    media: query,
    addEventListener: listener,
    removeEventListener: listener,
    addListener: listener,
    removeListener: listener,
  }));
  return listener;
}

afterEach(() => {
  vi.unstubAllGlobals();
  window.localStorage.removeItem(THEME_STORAGE_KEY);
  // hook 会把深浅色类写到 documentElement 上，别让它污染同文件后面的用例
  document.documentElement.classList.remove("dark");
  document.documentElement.classList.remove("light");
});

describe("usePanelTheme", () => {
  it("returns a tri-state: null follows the system, true/false are explicit", () => {
    stubMatchMedia(true);
    window.localStorage.setItem(THEME_STORAGE_KEY, "system");

    const { result } = renderHook(() => usePanelTheme(0));

    expect(result.current).toEqual({ dark: true, explicit: null });
  });

  it("takes the dark system scheme when nothing is stored", () => {
    stubMatchMedia(true);

    const { result } = renderHook(() => usePanelTheme(0));

    expect(result.current.dark).toBe(true);
    expect(result.current.explicit).toBeNull();
  });

  it("takes the light system scheme when nothing is stored", () => {
    stubMatchMedia(false);

    const { result } = renderHook(() => usePanelTheme(0));

    expect(result.current).toEqual({ dark: false, explicit: null });
  });

  it("an explicit dark choice wins over a light system scheme", () => {
    stubMatchMedia(false);
    window.localStorage.setItem(THEME_STORAGE_KEY, "dark");

    const { result } = renderHook(() => usePanelTheme(0));

    expect(result.current).toEqual({ dark: true, explicit: true });
  });

  it("an explicit light choice wins over a dark system scheme", () => {
    stubMatchMedia(true);
    window.localStorage.setItem(THEME_STORAGE_KEY, "light");

    const { result } = renderHook(() => usePanelTheme(0));

    expect(result.current).toEqual({ dark: false, explicit: false });
  });

  it("falls back to the system scheme when the stored value is garbage", () => {
    stubMatchMedia(true);
    window.localStorage.setItem(THEME_STORAGE_KEY, "solarized");

    const { result } = renderHook(() => usePanelTheme(0));

    expect(result.current).toEqual({ dark: true, explicit: null });
  });

  it("falls back to the system scheme when localStorage throws", () => {
    stubMatchMedia(false);
    vi.spyOn(window.localStorage, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });

    const { result } = renderHook(() => usePanelTheme(0));

    expect(result.current).toEqual({ dark: false, explicit: null });
  });

  it("mirrors the resolved scheme onto the document element", () => {
    stubMatchMedia(true);
    window.localStorage.setItem(THEME_STORAGE_KEY, "dark");

    renderHook(() => usePanelTheme(0));

    expect(document.documentElement.classList.contains("dark")).toBe(true);
    expect(document.documentElement.classList.contains("light")).toBe(false);
  });

  it("re-reads the stored choice when refreshKey changes", () => {
    stubMatchMedia(false);
    window.localStorage.setItem(THEME_STORAGE_KEY, "light");

    const { result, rerender } = renderHook(
      ({ key }: { key: number }) => usePanelTheme(key),
      { initialProps: { key: 0 } },
    );
    expect(result.current.dark).toBe(false);

    // 另一个窗口把选择改成了 dark，refreshKey 变了要跟着变
    window.localStorage.setItem(THEME_STORAGE_KEY, "dark");
    rerender({ key: 1 });

    expect(result.current).toEqual({ dark: true, explicit: true });
  });
});
