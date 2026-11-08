import { render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// CodeMirror 6 在 jsdom 里无法真实建 View，按契约 mock：记录构造参数、
// 暴露触发 updateListener 的手段，正好把「回吐 / 不回吐」的规则钉死。
type Listener = (update: unknown) => void;

const editorState = vi.hoisted(() => ({
  views: [] as Array<{
    args: { state: { doc: { toString: () => string } } };
    state: { doc: { toString: () => string } };
    dispatch: (tr: unknown) => void;
  }>,
  updateListeners: [] as Listener[],
  lastExtensions: [] as unknown[],
}));

vi.mock("codemirror", () => {
  class FakeEditorView {
    args!: { state: { doc: { toString: () => string } } };
    __doc = "";
    state!: {
      doc: { toString: () => string };
      changes: (spec: unknown) => unknown;
      selection: {
        ranges: Array<{ anchor: number; head: number; empty: boolean }>;
        mainIndex: number;
        map: () => { ranges: Array<{ anchor: number; head: number }> };
      };
      update: (spec: unknown) => unknown;
    };
    updateListener?: Listener;
    destroy!: () => void;
    constructor(args: { state: { doc: { toString: () => string } } }) {
      this.args = args;
      this.__doc = args.state.doc.toString();
      this.state = {
        doc: { toString: () => this.__doc },
        changes: (spec: unknown) => ({ changes: spec }),
        // 同步 effect 会把变化映射到选区；只验契约，不模拟真实映射。
        selection: {
          ranges: [{ anchor: 0, head: 0, empty: true }],
          mainIndex: 0,
          map: () => ({ ranges: [{ anchor: 0, head: 0 }] }),
        },
        // 外部同步事务：annotation 命中 ExternalSync，listener 必须跳过回吐。
        update: (spec: unknown) => ({
          ...(spec as Record<string, unknown>),
          annotation: () => true,
        }),
      };
      editorState.views.push(this as never);
      this.destroy = () => {};
    }
    static baseTheme() {
      return { __baseTheme: true };
    }
    static theme() {
      return { __theme: true };
    }
    static editable = { of: () => ({ __editable: true }) };
    static contentAttributes = { of: () => ({ __contentAttributes: true }) };
    static updateListener = {
      of: (listener: Listener) => {
        editorState.updateListeners.push(listener);
        return { __updateListener: true };
      },
    };
    dispatch(transaction: unknown) {
      this.updateListener?.({
        docChanged: true,
        transactions: [transaction],
        state: this.state,
      });
    }
  }
  return { EditorView: FakeEditorView, basicSetup: {} };
});

vi.mock("@codemirror/lang-json", () => ({ json: () => ({ __json: true }) }));
vi.mock("@codemirror/lang-javascript", () => ({
  javascript: () => ({ __javascript: true }),
}));
vi.mock("@codemirror/theme-one-dark", () => ({ oneDark: {} }));
vi.mock("@codemirror/view", () => ({ placeholder: () => ({}) }));
vi.mock("@codemirror/lint", () => ({
  linter: (fn: unknown) => ({ __linter: fn }),
}));

vi.mock("@codemirror/state", () => {
  return {
    Annotation: {
      define: () => ({
        of: (value: boolean) => ({ __externalSync: value }),
      }),
    },
    EditorSelection: {
      cursor: (head: number) => ({ anchor: head, head, empty: true }),
      range: (anchor: number, head: number) => ({ anchor, head, empty: false }),
      create: (ranges: unknown[], mainIndex: number) => ({ ranges, mainIndex }),
    },
    EditorState: {
      // jsdom 里只用到 create 组装 doc/extensions，真实编译产物才完整。
      create: (spec: { doc?: string; extensions?: unknown[] }) => ({
        doc: { toString: () => spec.doc ?? "" },
        extensions: spec.extensions ?? [],
      }),
      readOnly: { of: (value: boolean) => ({ readOnly: value }) },
    },
  };
});

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: { defaultValue?: string }) =>
      options?.defaultValue ?? key,
  }),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

import JsonEditor from "@/components/JsonEditor";

// ExternalSync 是组件内 Annotation.define 的返回值（上面 mock 的 Symbol），
// 事务带它 = 外部同步，updateListener 必须跳过。
const externalSyncTransaction = { annotation: () => true };
const userTransaction = { annotation: () => false };

describe("JsonEditor behavior", () => {
  beforeEach(() => {
    editorState.views.length = 0;
    editorState.updateListeners.length = 0;
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("seeds the editor with the incoming value", () => {
    render(<JsonEditor value='{"a":1}' onChange={vi.fn()} />);
    expect(editorState.views).toHaveLength(1);
    // 组件经 EditorState.create({ doc: value }) 把初值交给 View。
    expect(editorState.views[0].args.state.doc.toString()).toBe('{"a":1}');
  });

  it("reports user edits through onChange", () => {
    const onChange = vi.fn();
    render(<JsonEditor value="{}" onChange={onChange} />);
    expect(editorState.updateListeners).toHaveLength(1);

    editorState.views[0].state.doc = { toString: () => '{"b":2}' };
    editorState.updateListeners[0]({
      docChanged: true,
      transactions: [userTransaction],
      state: editorState.views[0].state,
    });

    expect(onChange).toHaveBeenCalledWith('{"b":2}');
  });

  it("does not echo back an external sync dispatch", () => {
    const onChange = vi.fn();
    render(<JsonEditor value="{}" onChange={onChange} />);

    editorState.updateListeners[0]({
      docChanged: true,
      transactions: [externalSyncTransaction],
      state: editorState.views[0].state,
    });

    expect(onChange).not.toHaveBeenCalled();
  });

  it("does not report edits while read-only", () => {
    const onChange = vi.fn();
    render(<JsonEditor value="{}" onChange={onChange} readOnly />);

    editorState.updateListeners[0]({
      docChanged: true,
      transactions: [userTransaction],
      state: editorState.views[0].state,
    });

    expect(onChange).not.toHaveBeenCalled();
  });
});
