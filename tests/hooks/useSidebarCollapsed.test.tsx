import { renderHook, act } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  SIDEBAR_AUTO_COLLAPSE_WIDTH,
  SIDEBAR_EXPANDED_WIDTH,
  sidebarWidth,
  useSidebarCollapsed,
} from "@/hooks/useSidebarCollapsed";

const navRef = { current: document.createElement("nav") };

const setWindowWidth = (width: number) => {
  window.innerWidth = width;
  act(() => {
    window.dispatchEvent(new Event("resize"));
  });
};

afterEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});

describe("useSidebarCollapsed", () => {
  it("follows the window width when there is no manual choice", () => {
    setWindowWidth(SIDEBAR_AUTO_COLLAPSE_WIDTH - 1);
    const { result } = renderHook(() => useSidebarCollapsed(navRef));
    expect(result.current.collapsed).toBe(true);

    setWindowWidth(SIDEBAR_AUTO_COLLAPSE_WIDTH + 1);
    expect(result.current.collapsed).toBe(false);
  });

  it("keeps the manual choice across window resizes", () => {
    setWindowWidth(SIDEBAR_AUTO_COLLAPSE_WIDTH + 1);
    localStorage.setItem("cc-switch-sidebar-collapsed", "true");
    const { result } = renderHook(() => useSidebarCollapsed(navRef));
    expect(result.current.collapsed).toBe(true);

    setWindowWidth(SIDEBAR_AUTO_COLLAPSE_WIDTH + 400);

    // 宽窗也保持收起：手动的优先于自动
    expect(result.current.collapsed).toBe(true);
  });

  it("honors a saved expanded choice even on a narrow window", () => {
    setWindowWidth(SIDEBAR_AUTO_COLLAPSE_WIDTH - 400);
    localStorage.setItem("cc-switch-sidebar-collapsed", "false");
    const { result } = renderHook(() => useSidebarCollapsed(navRef));

    expect(result.current.collapsed).toBe(false);
  });

  it("falls back to the window width when the stored value is unusable", () => {
    setWindowWidth(SIDEBAR_AUTO_COLLAPSE_WIDTH - 1);
    localStorage.setItem("cc-switch-sidebar-collapsed", "yes");
    const { result } = renderHook(() => useSidebarCollapsed(navRef));

    expect(result.current.collapsed).toBe(true);
  });

  it("persists a manual toggle and flips the collapsed state", () => {
    setWindowWidth(SIDEBAR_AUTO_COLLAPSE_WIDTH + 1);
    const { result } = renderHook(() => useSidebarCollapsed(navRef));
    expect(result.current.collapsed).toBe(false);

    act(() => result.current.toggle());

    expect(result.current.collapsed).toBe(true);
    expect(localStorage.getItem("cc-switch-sidebar-collapsed")).toBe("true");

    act(() => result.current.toggle());

    expect(result.current.collapsed).toBe(false);
    expect(localStorage.getItem("cc-switch-sidebar-collapsed")).toBe("false");
  });

  it("toggles on the keyboard shortcut and keeps the browser default away", () => {
    setWindowWidth(SIDEBAR_AUTO_COLLAPSE_WIDTH + 1);
    const { result } = renderHook(() => useSidebarCollapsed(navRef));

    act(() => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "\\", metaKey: true }),
      );
    });

    expect(result.current.collapsed).toBe(true);

    act(() => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "\\", ctrlKey: true }),
      );
    });

    expect(result.current.collapsed).toBe(false);
  });

  it("ignores the shortcut without a modifier", () => {
    setWindowWidth(SIDEBAR_AUTO_COLLAPSE_WIDTH + 1);
    const { result } = renderHook(() => useSidebarCollapsed(navRef));

    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "\\" }));
    });

    expect(result.current.collapsed).toBe(false);
  });

  it("still toggles when the preference cannot be stored", () => {
    setWindowWidth(SIDEBAR_AUTO_COLLAPSE_WIDTH + 1);
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("storage full");
    });
    const { result } = renderHook(() => useSidebarCollapsed(navRef));

    act(() => result.current.toggle());

    // 记不住不影响这次切换
    expect(result.current.collapsed).toBe(true);
  });

  it("maps the collapsed state to the rail and expanded widths", () => {
    expect(sidebarWidth(true)).toBe(72);
    expect(sidebarWidth(false)).toBe(SIDEBAR_EXPANDED_WIDTH);
  });
});
