import { renderHook, act } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useDelayedFlag } from "@/hooks/useDelayedFlag";

afterEach(() => {
  vi.useRealTimers();
});

describe("useDelayedFlag", () => {
  it("stays false immediately when the flag is off", () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useDelayedFlag(false));

    expect(result.current).toBe(false);

    act(() => vi.advanceTimersByTime(1000));

    expect(result.current).toBe(false);
  });

  it("returns true only after the flag has been on for the delay", () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useDelayedFlag(true));

    // 很快的写入不让整页按钮闪一下变灰
    expect(result.current).toBe(false);

    act(() => vi.advanceTimersByTime(299));
    expect(result.current).toBe(false);

    act(() => vi.advanceTimersByTime(1));
    expect(result.current).toBe(true);
  });

  it("drops back to false the moment the flag clears", () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(
      ({ flag }) => useDelayedFlag(flag),
      { initialProps: { flag: true } },
    );
    act(() => vi.advanceTimersByTime(300));
    expect(result.current).toBe(true);

    rerender({ flag: false });

    // 不等计时器：立刻回到可用状态
    expect(result.current).toBe(false);
  });

  it("clears the pending timer when the flag clears before the delay", () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(
      ({ flag }) => useDelayedFlag(flag),
      { initialProps: { flag: true } },
    );
    act(() => vi.advanceTimersByTime(100));

    rerender({ flag: false });
    act(() => vi.advanceTimersByTime(1000));

    // 旧的计时器被清掉：不会在 flag 已经关掉之后又翻成 true
    expect(result.current).toBe(false);
  });

  it("honors a custom delay", () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useDelayedFlag(true, 1000));

    act(() => vi.advanceTimersByTime(999));
    expect(result.current).toBe(false);

    act(() => vi.advanceTimersByTime(1));
    expect(result.current).toBe(true);
  });
});
