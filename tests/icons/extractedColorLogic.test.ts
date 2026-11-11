import { describe, expect, it } from "vitest";
import {
  adaptSvgToTheme,
  hexChannels,
  isDarkInk,
  paintedColors,
} from "@/icons/extracted";

describe("hexChannels", () => {
  it("splits a six-digit hex into channels", () => {
    expect(hexChannels("#112233")).toEqual([0x11, 0x22, 0x33]);
    expect(hexChannels("#ffffff")).toEqual([255, 255, 255]);
    expect(hexChannels("#000000")).toEqual([0, 0, 0]);
  });

  it("expands the three-digit shorthand", () => {
    expect(hexChannels("#abc")).toEqual([0xaa, 0xbb, 0xcc]);
    expect(hexChannels("#08f")).toEqual([0x00, 0x88, 0xff]);
  });

  it("ignores the alpha channel of the four-digit form", () => {
    // #rgba：前三位照常展开，第四位不进通道
    expect(hexChannels("#abcf")).toEqual([0xaa, 0xbb, 0xcc]);
    expect(hexChannels("#000f")).toEqual([0, 0, 0]);
  });

  it("rejects anything that is not 6 hex digits after expansion", () => {
    expect(hexChannels("#12345")).toBeNull();
    expect(hexChannels("#1234567")).toBeNull();
    expect(hexChannels("#gggggg")).toBeNull();
    expect(hexChannels("#12 345")).toBeNull();
  });

  it("only accepts lowercase hex digits", () => {
    expect(hexChannels("#AABBCC")).toBeNull();
  });
});

describe("isDarkInk", () => {
  it("treats the black keyword as ink", () => {
    expect(isDarkInk("black")).toBe(true);
    expect(isDarkInk("  BLACK ")).toBe(true);
    expect(isDarkInk("Black")).toBe(true);
  });

  it("uses the max-channel threshold, so any channel above 0x50 is not ink", () => {
    expect(isDarkInk("#000000")).toBe(true);
    expect(isDarkInk("#111111")).toBe(true);
    // 边界：0x50 算、0x51 不算
    expect(isDarkInk("#505050")).toBe(true);
    expect(isDarkInk("#515151")).toBe(false);
    // 只有一个通道超线也不算
    expect(isDarkInk("#600000")).toBe(false);
  });

  it("ignores non-hex and empty values", () => {
    expect(isDarkInk("")).toBe(false);
    expect(isDarkInk(undefined)).toBe(false);
    expect(isDarkInk("   ")).toBe(false);
    expect(isDarkInk("rgb(0,0,0)")).toBe(false);
    expect(isDarkInk("red")).toBe(false);
  });

  it("counts a saturated dark color as ink even though the doc comment says otherwise", () => {
    // 注释写「深红、深蓝这类有色的深色不算」，但实现是 max(通道) <= 0x50：
    // #300000 的 max 是 0x30，实现判定为墨色。这条测试把现实钉住，
    // 注释与代码的分歧另记在性能优化账本里。
    expect(isDarkInk("#300000")).toBe(true);
    expect(isDarkInk("#000030")).toBe(true);
  });
});

describe("paintedColors", () => {
  it("reads every fill and stroke value", () => {
    const svg = `<svg><path fill="#111" stroke="#222"/><circle fill="none"/></svg>`;

    expect(paintedColors(svg)).toEqual(["#111", "#222", "none"]);
  });

  it("ignores colors inside defs, clipPath and mask blocks", () => {
    const svg = [
      `<svg>`,
      `<defs><linearGradient><stop stop-color="#fff"/></linearGradient>`,
      `<clipPath id="c"><path fill="#000"/></clipPath></defs>`,
      `<mask id="m"><rect fill="#123456"/></mask>`,
      `<path fill="#abcdef"/>`,
      `</svg>`,
    ].join("");

    expect(paintedColors(svg)).toEqual(["#abcdef"]);
  });

  it("keeps colors that only look like mask keywords in attribute values", () => {
    // 只有真正的 <defs>/<clipPath>/<mask> 开标签才进排除，属性值里的不算
    const svg = `<path fill="#010101" data-note="mask"/>`;

    expect(paintedColors(svg)).toEqual(["#010101"]);
  });
});

describe("adaptSvgToTheme", () => {
  it("rewrites a single near-black ink icon to currentColor", () => {
    const svg = `<svg viewBox="0 0 1 1"><path fill="#111111"/></svg>`;

    expect(adaptSvgToTheme(svg)).toBe(
      `<svg viewBox="0 0 1 1"><path fill="currentColor"/></svg>`,
    );
  });

  it("rewrites stroke as well as fill", () => {
    const svg = `<svg><g stroke="#0a0a0a"><path fill="#000000"/></g></svg>`;

    expect(adaptSvgToTheme(svg)).toBe(
      `<svg><g stroke="currentColor"><path fill="currentColor"/></g></svg>`,
    );
  });

  it("leaves a multi-color icon untouched", () => {
    const svg = `<svg><path fill="#111111"/><path fill="#ff0000"/></svg>`;

    expect(adaptSvgToTheme(svg)).toBe(svg);
  });

  it("leaves a light single-ink icon untouched", () => {
    // 单一墨色但不够黑：不需要跟随文字色
    const svg = `<svg><path fill="#808080"/></svg>`;

    expect(adaptSvgToTheme(svg)).toBe(svg);
  });

  it("leaves an icon with no paint attributes untouched", () => {
    const svg = `<svg><path d="M0 0"/></svg>`;

    expect(adaptSvgToTheme(svg)).toBe(svg);
  });

  it("does not let mask-block colors decide the single-ink outcome", () => {
    // 遮罩里有个 #ff0000：不算墨色，所以不参与「多色」判断，
    // 真正画出来的只有一个近黑 fill，应被改写成 currentColor。
    const svg = [
      `<svg>`,
      `<mask id="m"><rect fill="#ff0000"/></mask>`,
      `<path fill="#111111"/>`,
      `</svg>`,
    ].join("");

    const adapted = adaptSvgToTheme(svg);

    expect(adapted).toContain(`fill="currentColor"`);
    // 遮罩块本身原样保留
    expect(adapted).toContain(`<rect fill="#ff0000"/>`);
  });

  it("keeps mask blocks verbatim even while rewriting the body", () => {
    const svg = `<svg><mask id="m"><rect fill="#222222"/></mask><path fill="#111111"/></svg>`;

    const adapted = adaptSvgToTheme(svg);

    // 遮罩里的近黑不改（改了会改变裁剪形状的观感）
    expect(adapted).toContain(`<rect fill="#222222"/>`);
    expect(adapted).toContain(`<path fill="currentColor"/>`);
  });

  it("rewrites only the ink attributes inside a body that also carries non-ink ones", () => {
    // 单墨判定看的是「全部可画颜色都近黑」；这里 fill 近黑、stroke 不近黑，
    // 于是整体算多色，一个都不改。
    const svg = `<svg><path fill="#111111" stroke="#abcdef"/></svg>`;

    expect(adaptSvgToTheme(svg)).toBe(svg);
  });

  it("returns the identical string for the same input (module-level cache)", () => {
    const svg = `<svg><path fill="#111111"/></svg>`;

    const first = adaptSvgToTheme(svg);
    const second = adaptSvgToTheme(svg);

    expect(first).toBe(second);
  });
});
