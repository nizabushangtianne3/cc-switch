import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useLastValidValue } from "@/hooks/useLastValidValue";

describe("useLastValidValue", () => {
  it("returns the current value while it is valid", () => {
    const { result, rerender } = renderHook(
      ({ value }) => useLastValidValue(value),
      { initialProps: { value: "first" as string | null } },
    );

    expect(result.current).toBe("first");

    rerender({ value: "second" });

    expect(result.current).toBe("second");
  });

  it("keeps showing the last valid value while the current one is empty", () => {
    // 对话框关动画期间内容不能空掉
    const { result, rerender } = renderHook(
      ({ value }: { value: string | null | undefined }) =>
        useLastValidValue(value),
      { initialProps: { value: "content" as string | null | undefined } },
    );

    rerender({ value: null });
    expect(result.current).toBe("content");

    rerender({ value: undefined });
    expect(result.current).toBe("content");
  });

  it("returns null when there has never been a valid value", () => {
    const { result } = renderHook(() => useLastValidValue<string>(null));

    expect(result.current).toBeNull();
  });

  it("returns the new valid value after an empty stretch", () => {
    const { result, rerender } = renderHook(
      ({ value }) => useLastValidValue(value),
      { initialProps: { value: "a" as string | null } },
    );
    rerender({ value: null });
    rerender({ value: "b" });

    expect(result.current).toBe("b");
  });
});
