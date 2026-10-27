import { describe, expect, it, vi } from "vitest";
import {
  confirmManagedAccountRemove,
  type ManagedAccountRemoveTarget,
} from "@/components/providers/forms/ManagedAccountRemoveDialog";

function handlers() {
  return {
    clearTarget: vi.fn(),
    logout: vi.fn(),
    removeAccount: vi.fn(),
    selectedAccountId: "a" as string | null | undefined,
    onAccountSelect: vi.fn(),
  };
}

describe("confirmManagedAccountRemove", () => {
  it("does nothing when there is no pending target", () => {
    const h = handlers();
    confirmManagedAccountRemove(null, h);
    expect(h.clearTarget).not.toHaveBeenCalled();
    expect(h.logout).not.toHaveBeenCalled();
    expect(h.removeAccount).not.toHaveBeenCalled();
  });

  it("logs out of everything when the target is the whole account", () => {
    const h = handlers();
    const target: ManagedAccountRemoveTarget = {
      kind: "all",
      accountIds: ["a", "b"],
    };
    confirmManagedAccountRemove(target, h);
    expect(h.clearTarget).toHaveBeenCalledTimes(1);
    expect(h.logout).toHaveBeenCalledTimes(1);
    expect(h.removeAccount).not.toHaveBeenCalled();
    expect(h.onAccountSelect).not.toHaveBeenCalled();
  });

  it("removes the account and clears the selection when it was the selected one", () => {
    const h = handlers();
    const target: ManagedAccountRemoveTarget = {
      kind: "one",
      accountId: "a",
      login: "alice",
    };
    confirmManagedAccountRemove(target, h);
    expect(h.clearTarget).toHaveBeenCalledTimes(1);
    expect(h.removeAccount).toHaveBeenCalledWith("a");
    expect(h.onAccountSelect).toHaveBeenCalledWith(null);
    expect(h.logout).not.toHaveBeenCalled();
  });

  it("removes the account but leaves the selection alone when it was not selected", () => {
    const h = handlers();
    h.selectedAccountId = "b";
    const target: ManagedAccountRemoveTarget = {
      kind: "one",
      accountId: "a",
      login: "alice",
    };
    confirmManagedAccountRemove(target, h);
    expect(h.removeAccount).toHaveBeenCalledWith("a");
    expect(h.onAccountSelect).not.toHaveBeenCalled();
  });

  it("tolerates a caller without onAccountSelect", () => {
    const h = handlers();
    delete (h as { onAccountSelect?: unknown }).onAccountSelect;
    const target: ManagedAccountRemoveTarget = {
      kind: "one",
      accountId: "a",
      login: "alice",
    };
    expect(() => confirmManagedAccountRemove(target, h)).not.toThrow();
    expect(h.removeAccount).toHaveBeenCalledWith("a");
  });
});
