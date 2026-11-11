import { describe, expect, it, vi } from "vitest";
import {
  defaultClientHost,
  formatUptime,
  isValidClientHost,
  isValidListenAddress,
} from "@/components/settings/sections/RoutingSection";

describe("isValidListenAddress", () => {
  it("accepts the two wildcard words", () => {
    expect(isValidListenAddress("localhost")).toBe(true);
    expect(isValidListenAddress("0.0.0.0")).toBe(true);
  });

  it("accepts a well-formed IPv4 address", () => {
    expect(isValidListenAddress("127.0.0.1")).toBe(true);
    expect(isValidListenAddress("192.168.1.100")).toBe(true);
    expect(isValidListenAddress("255.255.255.255")).toBe(true);
  });

  it("rejects an IPv4 address with an octet above 255", () => {
    // 正则只看「1~3 位数字」，上限靠逐段比较兜
    expect(isValidListenAddress("256.1.1.1")).toBe(false);
    expect(isValidListenAddress("999.999.999.999")).toBe(false);
  });

  it("rejects anything that is not four octets", () => {
    expect(isValidListenAddress("1.2.3")).toBe(false);
    expect(isValidListenAddress("1.2.3.4.5")).toBe(false);
    expect(isValidListenAddress("1.2.3.")).toBe(false);
  });

  it("accepts an IPv6 literal that survives bracket wrapping", () => {
    expect(isValidListenAddress("::1")).toBe(true);
    expect(isValidListenAddress("2001:db8::1")).toBe(true);
  });

  it("rejects an IPv6 literal the URL parser cannot take", () => {
    expect(isValidListenAddress("::gg")).toBe(false);
    expect(isValidListenAddress("12345::")).toBe(false);
  });

  it("rejects a bare hostname even though it has no colons", () => {
    expect(isValidListenAddress("example.com")).toBe(false);
  });
});

describe("isValidClientHost", () => {
  it("treats an empty value as follow-the-listener", () => {
    expect(isValidClientHost("")).toBe(true);
  });

  it("rejects the wildcard addresses clients cannot dial", () => {
    // 与 isValidListenAddress 相反：监听侧通配合法，客户端侧连不上
    expect(isValidClientHost("0.0.0.0")).toBe(false);
    expect(isValidClientHost("::")).toBe(false);
  });

  it("accepts a concrete IP on either family", () => {
    expect(isValidClientHost("127.0.0.1")).toBe(true);
    expect(isValidClientHost("::1")).toBe(true);
  });

  it("rejects a broken IP even though it looks numeric", () => {
    expect(isValidClientHost("999.1.1.1")).toBe(false);
    expect(isValidClientHost("1.2.3")).toBe(false);
  });

  it("accepts a normal hostname", () => {
    expect(isValidClientHost("example.com")).toBe(true);
    expect(isValidClientHost("proxy.internal")).toBe(true);
    expect(isValidClientHost("a1-b2.c3")).toBe(true);
  });

  it("rejects a hostname that breaks a label rule", () => {
    expect(isValidClientHost("-lead.com")).toBe(false);
    expect(isValidClientHost("trail-.com")).toBe(false);
    expect(isValidClientHost("double..dot")).toBe(false);
    expect(isValidClientHost("under_score")).toBe(false);
    expect(isValidClientHost("sp ace")).toBe(false);
  });

  it("rejects a label past the 63-character ceiling", () => {
    expect(isValidClientHost(`${"a".repeat(64)}.com`)).toBe(false);
    expect(isValidClientHost(`${"a".repeat(63)}.com`)).toBe(true);
  });
});

describe("defaultClientHost", () => {
  it("loops back the IPv4 wildcard", () => {
    expect(defaultClientHost("0.0.0.0")).toBe("127.0.0.1");
  });

  it("loops back the IPv6 wildcard", () => {
    expect(defaultClientHost("::")).toBe("::1");
  });

  it("passes a concrete listener through untouched", () => {
    expect(defaultClientHost("127.0.0.1")).toBe("127.0.0.1");
    expect(defaultClientHost("localhost")).toBe("localhost");
    expect(defaultClientHost("::1")).toBe("::1");
  });
});

describe("formatUptime", () => {
  /** 只回 key + 参数，断言走哪个分支。 */
  const t = vi.fn((key: string, opts?: Record<string, unknown>) =>
    opts ? `${key}(${JSON.stringify(opts)})` : key,
  ) as unknown as Parameters<typeof formatUptime>[1];

  it("reports hours and minutes together once past an hour", () => {
    formatUptime(3661, t);

    expect(t).toHaveBeenCalledWith("routingSettings.uptimeHours", {
      hours: 1,
      minutes: 1,
    });
  });

  it("reports only minutes below an hour", () => {
    formatUptime(125, t);

    expect(t).toHaveBeenCalledWith("routingSettings.uptimeMinutes", {
      minutes: 2,
    });
  });

  it("clamps a negative remainder to zero minutes", () => {
    formatUptime(-30, t);

    expect(t).toHaveBeenCalledWith("routingSettings.uptimeMinutes", {
      minutes: 0,
    });
  });

  it("drops the minutes from the hourly branch when they are zero", () => {
    formatUptime(7200, t);

    expect(t).toHaveBeenCalledWith("routingSettings.uptimeHours", {
      hours: 2,
      minutes: 0,
    });
  });
});
it("rejects a name that passes every label rule but exceeds 253 total", () => {
  // 4 个各 63 字符的标签 + 3 个点 = 255 > 253：每条标签都合法，
  // 是总长前瞻 (?=.{1,253}$) 否掉的
  const long = [63, 63, 63, 63].map((n) => "a".repeat(n)).join(".");
  expect(long.length).toBe(255);
  expect(isValidClientHost(long)).toBe(false);

  // 对照组：3 个同规格标签 + 2 个点 = 191，合法
  const ok = [63, 63, 63].map((n) => "a".repeat(n)).join(".");
  expect(ok.length).toBe(191);
  expect(isValidClientHost(ok)).toBe(true);
});

it("rejects a single label past the 63-character label rule", () => {
  // 253 个字符的单标签：总长没过 253，但单个标签规则只允许 63
  expect(isValidClientHost("a".repeat(253))).toBe(false);
  expect(isValidClientHost("a".repeat(63))).toBe(true);
});
