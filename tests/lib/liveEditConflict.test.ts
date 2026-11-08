import { describe, expect, it } from "vitest";
import {
  LIVE_EDIT_CONFLICT_CODE,
  parseLiveEditConflict,
} from "@/lib/errors/liveEditConflict";

const conflictJson = (keys: unknown) =>
  JSON.stringify({ code: LIVE_EDIT_CONFLICT_CODE, keys });

describe("parseLiveEditConflict", () => {
  it("parses the conflicting key paths from a backend error", () => {
    const error = new Error(conflictJson(["hooks", "env.DEBUG"]));
    expect(parseLiveEditConflict(error)).toEqual({
      keys: ["hooks", "env.DEBUG"],
    });
  });

  it("accepts a plain string message and an object payload", () => {
    const message = conflictJson(["mcp"]);
    expect(parseLiveEditConflict(message)).toEqual({ keys: ["mcp"] });
    expect(parseLiveEditConflict({ message })).toEqual({ keys: ["mcp"] });
    expect(parseLiveEditConflict({ payload: message })).toEqual({
      keys: ["mcp"],
    });
  });

  it("stringifies non-string keys", () => {
    expect(parseLiveEditConflict(conflictJson([1, true, null]))).toEqual({
      keys: ["1", "true", "null"],
    });
  });

  it("returns null when the message is not a conflict", () => {
    expect(parseLiveEditConflict(new Error("network down"))).toBeNull();
    expect(parseLiveEditConflict(undefined)).toBeNull();
    expect(parseLiveEditConflict("")).toBeNull();
  });

  it("returns null when the payload mentions the code but is not valid JSON", () => {
    expect(
      parseLiveEditConflict(`${LIVE_EDIT_CONFLICT_CODE} {oops`),
    ).toBeNull();
  });

  it("returns null when the JSON carries a different code", () => {
    expect(parseLiveEditConflict(JSON.stringify({ code: "OTHER" }))).toBeNull();
  });

  it("returns null when keys is missing or not an array", () => {
    expect(
      parseLiveEditConflict(JSON.stringify({ code: LIVE_EDIT_CONFLICT_CODE })),
    ).toBeNull();
    expect(
      parseLiveEditConflict(
        JSON.stringify({ code: LIVE_EDIT_CONFLICT_CODE, keys: "hooks" }),
      ),
    ).toBeNull();
  });
});
