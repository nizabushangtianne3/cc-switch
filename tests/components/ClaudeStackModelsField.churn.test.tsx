import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { describe, expect, it, vi } from "vitest";
import type { FetchedModel } from "@/lib/api/model-fetch";
import {
  ClaudeStackModelsField,
  createClaudeStackModelRow,
} from "@/components/providers/forms/ClaudeStackModelsField";
import { Form } from "@/components/ui/form";

const imeInputSpy = vi.fn();

// 计数探针：每行一个显示名称输入。包 memo，否则父组件重渲染必然调用它，基准测不到
// memo 契约。探针渲染一个真 input，键入走真实受控流。
vi.mock("@/components/ui/ime-safe-input", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("@/components/ui/ime-safe-input")>();
  const { memo } = await vi.importActual<typeof import("react")>("react");
  return {
    ...actual,
    ImeSafeInput: memo((props: Record<string, unknown>) => {
      imeInputSpy(props);
      return (
        <input
          aria-label={props["aria-label"] as string}
          value={props.value as string}
          onChange={(event) =>
            (props.onValueChange as (value: string) => void)(event.target.value)
          }
        />
      );
    }),
  };
});

const noModels: FetchedModel[] = [];
const noFetchModels = () => {};

// harness 持有 rows state：键入某一行像真实表单那样逐字符触发父组件重渲染
function Harness() {
  const [rows, setRows] = useState(() => [
    createClaudeStackModelRow({ model: "m-one", displayName: "One" }),
    createClaudeStackModelRow({ model: "m-two", displayName: "Two" }),
    createClaudeStackModelRow({ model: "m-three", displayName: "Three" }),
  ]);
  const form = useForm();
  return (
    <Form {...form}>
      <ClaudeStackModelsField
        rows={rows}
        onRowsChange={setRows}
        fetchedModels={noModels}
        onFetchModels={noFetchModels}
        isFetchingModels={false}
      />
    </Form>
  );
}

const rendersFor = (value: string) =>
  imeInputSpy.mock.calls.filter(([props]) => props.value === value).length;

describe("ClaudeStackModelsField render churn", () => {
  it("typing in one row leaves the other rows untouched", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    expect(imeInputSpy).toHaveBeenCalledTimes(3);
    expect(rendersFor("Two")).toBe(1);

    await user.type(screen.getAllByLabelText("显示名称")[0], "X");

    // 只有被键入那一行自己重渲染（值变了），其余行一次都不多跑
    expect(rendersFor("Two")).toBe(1);
    expect(rendersFor("Three")).toBe(1);
    expect(imeInputSpy.mock.calls.length).toBeGreaterThan(3);
  });
});
