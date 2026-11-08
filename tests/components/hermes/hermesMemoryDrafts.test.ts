import { beforeEach, describe, expect, it } from "vitest";
import {
  hermesMemoryDrafts,
  isHermesMemoryDirty,
} from "@/components/hermes/hermesMemoryDrafts";

// 模块级单例 store：每个用例都必须 reset，否则上一个用例的草稿会污染下一个。

describe("hermesMemoryDrafts", () => {
  beforeEach(() => {
    hermesMemoryDrafts.reset();
  });

  it("fills draft and saved on the first read of a memory file", () => {
    hermesMemoryDrafts.hydrate("memory", "hello");
    const state = hermesMemoryDrafts.getState();
    expect(state.draft.memory).toBe("hello");
    expect(state.saved.memory).toBe("hello");
    expect(isHermesMemoryDirty(state, "memory")).toBe(false);
  });

  it("keeps an in-progress edit when the same file is re-read", () => {
    hermesMemoryDrafts.hydrate("memory", "hello");
    hermesMemoryDrafts.edit("memory", "hello world");
    hermesMemoryDrafts.hydrate("memory", "hello");
    expect(hermesMemoryDrafts.getState().draft.memory).toBe("hello world");
    expect(isHermesMemoryDirty(hermesMemoryDrafts.getState(), "memory")).toBe(
      true,
    );
  });

  it("keeps drafts of the two memories independent", () => {
    hermesMemoryDrafts.hydrate("memory", "m");
    hermesMemoryDrafts.hydrate("user", "u");
    hermesMemoryDrafts.edit("user", "u2");
    const state = hermesMemoryDrafts.getState();
    expect(state.draft.memory).toBe("m");
    expect(isHermesMemoryDirty(state, "memory")).toBe(false);
    expect(isHermesMemoryDirty(state, "user")).toBe(true);
  });

  it("marks the draft dirty after an edit", () => {
    hermesMemoryDrafts.hydrate("user", "a");
    hermesMemoryDrafts.edit("user", "b");
    expect(isHermesMemoryDirty(hermesMemoryDrafts.getState(), "user")).toBe(
      true,
    );
  });

  it("clears the dirty flag when the saved content catches up", () => {
    hermesMemoryDrafts.hydrate("memory", "a");
    hermesMemoryDrafts.edit("memory", "b");
    hermesMemoryDrafts.markSaved("memory", "b");
    expect(isHermesMemoryDirty(hermesMemoryDrafts.getState(), "memory")).toBe(
      false,
    );
  });

  it("stays dirty when the content saved mid-edit is not what is in the editor", () => {
    // 保存途中又改了编辑区：以保存出去的内容为准，未保存修改仍然成立。
    hermesMemoryDrafts.hydrate("memory", "a");
    hermesMemoryDrafts.edit("memory", "b");
    hermesMemoryDrafts.markSaved("memory", "a");
    expect(isHermesMemoryDirty(hermesMemoryDrafts.getState(), "memory")).toBe(
      true,
    );
  });

  it("reports a never-hydrated memory as clean", () => {
    expect(isHermesMemoryDirty(hermesMemoryDrafts.getState(), "user")).toBe(
      false,
    );
  });

  it("notifies subscribers on a tab switch but not on a redundant one", () => {
    const seen: string[] = [];
    const unsubscribe = hermesMemoryDrafts.subscribe(() => {
      seen.push(hermesMemoryDrafts.getState().active);
    });
    hermesMemoryDrafts.setActive("user");
    hermesMemoryDrafts.setActive("user");
    expect(seen).toEqual(["user"]);
    unsubscribe();
  });

  it("stops notifying after unsubscribe", () => {
    let calls = 0;
    const unsubscribe = hermesMemoryDrafts.subscribe(() => {
      calls += 1;
    });
    unsubscribe();
    hermesMemoryDrafts.setActive("user");
    expect(calls).toBe(0);
  });

  it("clears every draft and returns to the memory tab on reset", () => {
    hermesMemoryDrafts.hydrate("user", "u");
    hermesMemoryDrafts.setActive("user");
    hermesMemoryDrafts.edit("user", "u2");
    hermesMemoryDrafts.reset();
    const state = hermesMemoryDrafts.getState();
    expect(state.active).toBe("memory");
    expect(state.draft).toEqual({});
    expect(state.saved).toEqual({});
  });
});
