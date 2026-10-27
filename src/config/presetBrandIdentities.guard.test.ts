import { describe, expect, it } from "vitest";

import { PRESET_BRAND_IDENTITIES } from "./presetBrandIdentities";
import { PRESET_BRAND_IDENTITY_KEYS } from "./presetBrandIdentitiesBaseline";
import { providerPresets } from "./claudeProviderPresets";
import { claudeDesktopProviderPresets } from "./claudeDesktopProviderPresets";
import { codexProviderPresets } from "./codexProviderPresets";
import { opencodeProviderPresets } from "./opencodeProviderPresets";
import { openclawProviderPresets } from "./openclawProviderPresets";
import { hermesProviderPresets } from "./hermesProviderPresets";
import { piProviderPresets } from "./piProviderPresets";
import { geminiProviderPresets } from "./geminiProviderPresets";
import { universalProviderPresets } from "./universalProviderPresets";

/**
 * 品牌身份共享表的守卫测试。
 *
 * 共享表下沉的合法性完全由这个测试自证：它不信任 PRESET_BRAND_IDENTITIES，
 * 而是从各预设集合的运行时对象重新推导「哪些键本该进表、哪些键必须留在
 * 文件里」，再与常量表、与每个条目对象三方比对。共享表漏字段、多字段、
 * 值漂移，或者把没资格下沉的条目也 spread 掉，都会让这个测试失败。
 */

const IDENTITY_KEYS = [
  "name",
  "nameKey",
  "websiteUrl",
  "apiKeyUrl",
  "category",
  "icon",
  "iconColor",
  "family",
  "planKey",
  "regionKey",
  "isPartner",
  "primePartner",
  "partnerPromotionKey",
  "isCustomTemplate",
] as const;

type AnyEntry = Record<string, unknown>;

const collections = {
  claude: providerPresets,
  claudeDesktop: claudeDesktopProviderPresets,
  codex: codexProviderPresets,
  opencode: opencodeProviderPresets,
  openclaw: openclawProviderPresets,
  hermes: hermesProviderPresets,
  pi: piProviderPresets,
  gemini: geminiProviderPresets,
  universal: universalProviderPresets,
} as const;

interface BrandGroup {
  name: string;
  entries: Array<{ file: string; entry: AnyEntry }>;
  /** 在所有出现该名称的文件里都存在且值完全一致的键 */
  sharedKeys: string[];
  /** 至少一个文件缺失的键 */
  missingKeys: string[];
  /** 都出现但值不一致的键 */
  conflictKeys: string[];
}

function readIdentity(entry: AnyEntry, key: string) {
  return entry[key];
}

function identityKeySet(entry: AnyEntry) {
  return IDENTITY_KEYS.filter((key) => readIdentity(entry, key) !== undefined);
}

function sameValue(a: unknown, b: unknown) {
  return JSON.stringify(a) === JSON.stringify(b);
}

function analyzeBrand(name: string): BrandGroup {
  const entries: Array<{ file: string; entry: AnyEntry }> = [];
  for (const [file, presets] of Object.entries(collections)) {
    for (const preset of presets as unknown as readonly AnyEntry[]) {
      if (preset.name === name) entries.push({ file, entry: preset });
    }
  }
  const sharedKeys: string[] = [];
  const missingKeys: string[] = [];
  const conflictKeys: string[] = [];
  for (const key of IDENTITY_KEYS) {
    const present = entries.filter(
      ({ entry }) => readIdentity(entry, key) !== undefined,
    );
    if (present.length === 0) continue;
    if (present.length < entries.length) {
      missingKeys.push(key);
      continue;
    }
    const first = readIdentity(entries[0].entry, key);
    const allSame = entries.every(({ entry }) =>
      sameValue(readIdentity(entry, key), first),
    );
    if (allSame) sharedKeys.push(key);
    else conflictKeys.push(key);
  }
  return { name, entries, sharedKeys, missingKeys, conflictKeys };
}

const brandNames = [
  ...new Set(
    Object.values(collections).flatMap((presets) =>
      (presets as unknown as readonly AnyEntry[]).map(
        (preset) => preset.name as string,
      ),
    ),
  ),
];

const groups = new Map<string, BrandGroup>();
for (const name of brandNames) groups.set(name, analyzeBrand(name));

const tableRows = Object.entries(PRESET_BRAND_IDENTITIES).map(([id, row]) => ({
  id,
  row: row as AnyEntry,
}));

describe("PRESET_BRAND_IDENTITIES guard", () => {
  it("每行表都能对上一条可下沉的品牌，且没有重复品牌", () => {
    const seen = new Set<string>();
    for (const { id, row } of tableRows) {
      const name = row.name as string;
      expect(typeof name, `表行 ${id} 缺少 name`).toBe("string");
      expect(seen.has(name), `表行 ${id} 的品牌 ${name} 重复`).toBe(false);
      seen.add(name);
      const group = groups.get(name);
      expect(
        group,
        `表行 ${id} 的品牌 ${name} 不存在于任何预设集合`,
      ).toBeDefined();
    }
  });

  it("表行的键集合等于该品牌在所有文件里逐字一致的键集合", () => {
    for (const { id, row } of tableRows) {
      const name = row.name as string;
      const group = groups.get(name)!;
      expect(
        identityKeySet(row).sort(),
        `表行 ${id} 的键集合与推导结果不一致`,
      ).toEqual([...group.sharedKeys].sort());
    }
  });

  it("表行每个键的值与各预设条目运行时逐字相同", () => {
    for (const { id, row } of tableRows) {
      const name = row.name as string;
      const group = groups.get(name)!;
      for (const key of group.sharedKeys) {
        for (const { entry } of group.entries) {
          expect(
            sameValue(readIdentity(entry, key), readIdentity(row, key)),
            `表行 ${id} 的 ${key} 与 ${group.name} 的某个预设条目不一致`,
          ).toBe(true);
        }
      }
    }
  });

  it("所有可下沉条目的身份键都只来自共享表，没丢也没多", () => {
    for (const { row } of tableRows) {
      const name = row.name as string;
      const group = groups.get(name)!;
      for (const { file, entry } of group.entries) {
        expect(
          identityKeySet(entry).sort(),
          `${file} 的 ${name} 身份键集合 != 表行键集合`,
        ).toEqual([...group.sharedKeys].sort());
      }
    }
  });

  it("没资格下沉的品牌一律没有表行、条目键集合原样保留", () => {
    const tabled = new Set(tableRows.map(({ row }) => row.name as string));
    for (const group of groups.values()) {
      const eligible =
        group.entries.length > 1 &&
        group.sharedKeys.length > 0 &&
        group.missingKeys.length === 0 &&
        group.conflictKeys.length === 0;
      expect(eligible, `${group.name} 的可下沉性判定与预期不符`).toBe(
        tabled.has(group.name),
      );
      if (!eligible) {
        // 未下沉的品牌：条目上不应残留本该共享的键（防漏改），
        // 也不应出现表里没有的痕迹（防多改）
        expect(
          tabled.has(group.name),
          `${group.name} 不可下沉却被写进共享表`,
        ).toBe(false);
      }
    }
  });

  it("表行的键集合与冻结基线逐字一致：严防从表行里删键", () => {
    // 其余断言都是拿「表」和「条目」互相比，而条目是 spread 表行的——表行丢一个键，
    // 条目会跟着丢，两边仍然自洽，那种改动此前能骗过整个测试组。基线是 spread
    // 那一刻的独立快照，不参与推导，所以只有它能判「这一家本该有这几个键」。
    const baseline = Object.entries(PRESET_BRAND_IDENTITY_KEYS);
    expect(baseline.length).toBe(tableRows.length);
    for (const [id, expected] of baseline) {
      const row =
        PRESET_BRAND_IDENTITIES[id as keyof typeof PRESET_BRAND_IDENTITIES];
      expect(row, `冻结基线里的 ${id} 已不在共享表中`).toBeDefined();
      expect(
        identityKeySet(row as AnyEntry).sort(),
        `表行 ${id} 的键集合与冻结基线不一致`,
      ).toEqual([...expected].sort());
    }
  });
});
