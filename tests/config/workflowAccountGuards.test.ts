import { describe, expect, it } from "vitest";

/**
 * GitHub 账号从 15683862759 改名为 nizabushangtianne3 之后，凡是把旧账号写死在
 * 仓库判断里的工作流都会静默失效：Actions 实际跑在 nizabushangtianne3/cc-switch 上，
 * 旧账号的仓库守卫永远为假，发布任务会被整段跳过（FlClash v0.8.113 的发布就是这样
 * 失败的）。这份测试盯着 `.github/workflows/`，防止旧账号或别家仓库名再混进来。
 */
const LEGACY_OWNER = "15683862759";
const CURRENT_REPOSITORY = "nizabushangtianne3/cc-switch";
const UPSTREAM_REPOSITORY = "farion1231/cc-switch";

const workflows = import.meta.glob<string>("../../.github/workflows/*.yml", {
  eager: true,
  query: "?raw",
  import: "default",
});

/** 工作流里 `github.repository == '...'` 守卫写死的仓库名。 */
function guardedRepositories(source: string): string[] {
  return Array.from(
    source.matchAll(/github\.repository\s*==\s*'([^']+)'/g),
    ([, repository]) => repository,
  );
}

/** 源码里是否还提到改名前的账号。 */
function mentionsLegacyOwner(source: string): boolean {
  return source.includes(LEGACY_OWNER);
}

describe("workflow account guards", () => {
  it("actually loads the workflow files it guards", () => {
    const names = Object.keys(workflows).map((path) => path.split("/").pop());
    // 空 glob 会让下面两条断言变成空转，这里先钉住真的读到了工作流。
    expect(names).toContain("release-fork.yml");
    expect(names).toContain("release.yml");
    expect(names.length).toBeGreaterThan(3);
  });

  it("never names the pre-rename GitHub account", () => {
    const offenders = Object.entries(workflows)
      .filter(([, source]) => mentionsLegacyOwner(source))
      .map(([path]) => path);
    expect(offenders).toEqual([]);
  });

  it("only gates jobs on this fork or the upstream repository", () => {
    const guards = Object.entries(workflows).flatMap(([path, source]) =>
      guardedRepositories(source).map((repository) => ({ path, repository })),
    );
    // fork 现在不按仓库名做守卫（发布走 release-unsigned）。将来从上游合并回
    // `github.repository == '…'` 名单时，这里会失败，提醒把新账号加进去。
    for (const { path, repository } of guards) {
      expect(
        [CURRENT_REPOSITORY, UPSTREAM_REPOSITORY],
        `${path} 把任务委托给了 ${repository}`,
      ).toContain(repository);
    }
  });

  it("catches a guard that names the pre-rename account", () => {
    expect(
      guardedRepositories(
        `if: github.repository == '${LEGACY_OWNER}/cc-switch'`,
      ),
    ).toEqual([`${LEGACY_OWNER}/cc-switch`]);
    expect(
      mentionsLegacyOwner("github.repository == '15683862759/FlClash'"),
    ).toBe(true);
    expect(
      mentionsLegacyOwner(
        "github.repository == 'nizabushangtianne3/cc-switch'",
      ),
    ).toBe(false);
  });
});
