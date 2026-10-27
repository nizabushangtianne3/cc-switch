import { describe, expect, it } from "vitest";
import type { ManagedAuthAccount } from "@/lib/api/auth";
import {
  selectedAccountExists,
  selectedCodexAccountIsUsable,
  selectedXaiAccountIsUsable,
} from "@/components/providers/forms/hooks/managedAccountSelection";

function account(
  id: string,
  overrides: Partial<ManagedAuthAccount> = {},
): ManagedAuthAccount {
  return {
    id,
    provider: "codex_oauth",
    login: `${id}@example.test`,
    avatar_url: null,
    authenticated_at: 0,
    is_default: false,
    github_domain: "github.com",
    requires_reauth: false,
    ...overrides,
  };
}

describe("selectedCodexAccountIsUsable", () => {
  it("falls back to the backend default account when nothing is selected", () => {
    const accounts = [account("a"), account("b")];
    expect(
      selectedCodexAccountIsUsable(null, "b", accounts),
      "未选中时应落到后端默认账号",
    ).toBe(true);
  });

  it("falls back to the account flagged is_default, then to the first row", () => {
    const accounts = [account("a"), account("b", { is_default: true })];
    expect(selectedCodexAccountIsUsable(null, null, accounts)).toBe(true);
  });

  it("treats an account needing reauth as unusable", () => {
    const accounts = [account("a"), account("b", { reauth_required: true })];
    expect(selectedCodexAccountIsUsable("b", null, accounts)).toBe(false);
    expect(selectedCodexAccountIsUsable("a", null, accounts)).toBe(true);
  });

  it("is unusable when the account list is empty", () => {
    expect(selectedCodexAccountIsUsable(null, null, [])).toBe(false);
    expect(selectedCodexAccountIsUsable("a", null, [])).toBe(false);
  });

  it("is unusable when the selected id is not in the list", () => {
    const accounts = [account("a")];
    expect(selectedCodexAccountIsUsable("ghost", null, accounts)).toBe(false);
  });
});

describe("selectedXaiAccountIsUsable", () => {
  it("treats no selection as usable", () => {
    expect(selectedXaiAccountIsUsable(null, [])).toBe(true);
  });

  it("treats an account with an invalid refresh credential as unusable", () => {
    const accounts = [account("a"), account("b", { requires_reauth: true })];
    expect(selectedXaiAccountIsUsable("b", accounts)).toBe(false);
    expect(selectedXaiAccountIsUsable("a", accounts)).toBe(true);
  });

  it("is unusable when the selected id is not in the list", () => {
    expect(selectedXaiAccountIsUsable("ghost", [account("a")])).toBe(false);
  });
});

describe("selectedAccountExists", () => {
  it("treats no selection as existing", () => {
    expect(selectedAccountExists(null, [])).toBe(true);
  });

  it("checks membership only by id", () => {
    const accounts = [{ id: "a" }, { id: "b" }];
    expect(selectedAccountExists("b", accounts)).toBe(true);
    expect(selectedAccountExists("ghost", accounts)).toBe(false);
  });
});
