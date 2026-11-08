import { renderHook, act } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useCommittableRef, useLatestRef } from "@/hooks/useLatestRef";

describe("useLatestRef", () => {
  it("tracks the latest value across renders", () => {
    const { result, rerender } = renderHook(
      ({ value }) => useLatestRef(value),
      {
        initialProps: { value: "first" },
      },
    );

    expect(result.current.current).toBe("first");

    rerender({ value: "second" });

    expect(result.current.current).toBe("second");
  });

  it("lets an async callback read the value captured after it was created", () => {
    let read: (() => string) | undefined;
    const { result, rerender } = renderHook(
      ({ value }) => {
        const ref = useLatestRef(value);
        // 只在首次渲染时建一次回调，之后值变了它照旧读最新的
        read ??= () => ref.current;
        return ref;
      },
      { initialProps: { value: "old" } },
    );

    rerender({ value: "new" });

    expect(read!()).toBe("new");
    expect(result.current.current).toBe("new");
  });
});

describe("useCommittableRef", () => {
  it("writes the ref before calling onChange", () => {
    const seen: string[] = [];
    const { result } = renderHook(
      ({ value, onChange }) => useCommittableRef(value, onChange),
      {
        initialProps: {
          value: "a",
          onChange: (next: string) => {
            // 提交那一刻 ref 必须已经是新值：紧接着的读取不必等重渲染
            seen.push(result.current[0].current + "->" + next);
          },
        },
      },
    );

    act(() => result.current[1]("b"));

    expect(seen).toEqual(["b->b"]);
  });

  it("calls the latest onChange, not the one from the render that built commit", () => {
    const first = vi.fn();
    const second = vi.fn();
    const { result, rerender } = renderHook(
      ({ onChange }: { onChange: (next: string) => void }) =>
        useCommittableRef<string>("x", onChange),
      { initialProps: { onChange: first } },
    );

    rerender({ onChange: second });
    act(() => result.current[1]("y"));

    // 父组件换了回调：旧回调不能被晚到的提交带回去
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledWith("y");
  });

  it("keeps commit stable across renders", () => {
    const { result, rerender } = renderHook(
      ({ onChange }: { onChange: (next: string) => void }) =>
        useCommittableRef<string>("x", onChange),
      { initialProps: { onChange: vi.fn() } },
    );
    const firstCommit = result.current[1];

    rerender({ onChange: vi.fn() });

    expect(result.current[1]).toBe(firstCommit);
  });
});
