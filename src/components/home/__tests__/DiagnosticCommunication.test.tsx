import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DiagnosticCommunicationDialog,
  DiagnosticRetentionControl,
} from "../DiagnosticCommunication";
import { createQueryWrapper, createTestQueryClient } from "../../../test/utils/reactQuery";
import { setTauriRuntime } from "../../../test/utils/tauriRuntime";
import {
  diagnosticsClear,
  diagnosticsConfigure,
  diagnosticsEvents,
  diagnosticsSnapshot,
  diagnosticsSaveBody,
  type DiagnosticSnapshot,
  type DiagnosticEvent,
} from "../../../services/gateway/diagnostics";
import { writeDesktopClipboardText } from "../../../services/desktop/clipboard";
import { saveDesktopFilePath } from "../../../services/desktop/dialog";

vi.mock("../../../services/gateway/diagnostics", () => ({
  diagnosticsSnapshot: vi.fn(),
  diagnosticsEvents: vi.fn(),
  diagnosticsConfigure: vi.fn(),
  diagnosticsClear: vi.fn(),
  diagnosticsSaveBody: vi.fn(),
}));
vi.mock("../../../services/desktop/clipboard", () => ({ writeDesktopClipboardText: vi.fn() }));
vi.mock("../../../services/desktop/dialog", () => ({ saveDesktopFilePath: vi.fn() }));

function snapshot(): DiagnosticSnapshot {
  return {
    enabled: false,
    retention_days: 15,
    stored_bytes: 1024,
    dropped_messages: 0,
    last_error: null,
    traces: [
      {
        trace_id: "trace-one",
        cli_key: "codex",
        method: "POST",
        path: "/v1/responses",
        created_at_ms: 1770000000000,
        status: 503,
        capture_limited: false,
      },
    ],
  };
}

function event(overrides: Partial<DiagnosticEvent> = {}): DiagnosticEvent {
  return {
    id: "one",
    phase: "upstream_response",
    metadata: "HTTP 503",
    body: "upstream failed",
    body_encoding: "utf8",
    created_at_ms: 1770000000000,
    bytes_seen: 15,
    complete: true,
    truncated: false,
    note: null,
    ...overrides,
  };
}

function setup(data = snapshot()) {
  vi.mocked(diagnosticsSnapshot).mockResolvedValue(data);
  vi.mocked(diagnosticsEvents).mockResolvedValue([event()]);
  vi.mocked(diagnosticsConfigure).mockImplementation(async (enabled, days) => {
    data.enabled = enabled;
    data.retention_days = days;
    return true;
  });
  vi.mocked(diagnosticsClear).mockImplementation(async () => {
    data.traces = [];
    vi.mocked(diagnosticsEvents).mockResolvedValue([]);
    return true;
  });
  vi.mocked(writeDesktopClipboardText).mockResolvedValue(true);
  const client = createTestQueryClient();
  const wrapper = createQueryWrapper(client);
  return { client, wrapper };
}

describe("communication retention", () => {
  afterEach(() => {
    vi.resetAllMocks();
  });

  it("opens from the home control and persists the enabled state", async () => {
    setTauriRuntime();
    const { wrapper } = setup();
    render(<DiagnosticRetentionControl />, { wrapper });
    const toggle = screen.getByRole("switch", { name: "信息驻留" });
    await waitFor(() => expect(toggle).toBeEnabled());
    fireEvent.click(toggle);
    await waitFor(() => expect(diagnosticsConfigure).toHaveBeenCalledWith(true, 15));
    await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"));
    fireEvent.click(screen.getByRole("button", { name: "通信查看" }));
    expect(await screen.findByText("供应商 → 网关", { exact: false })).toBeInTheDocument();
    expect(screen.getByText("upstream failed")).toBeInTheDocument();
  });

  it("validates and persists retention days", async () => {
    const { wrapper } = setup();
    render(<DiagnosticCommunicationDialog open onOpenChange={vi.fn()} />, { wrapper });
    const input = screen.getByRole("spinbutton", { name: "驻留天数" });
    expect(input).toHaveValue(15);
    await screen.findByText("upstream failed");
    fireEvent.change(input, { target: { value: "0" } });
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
    fireEvent.change(input, { target: { value: "1.5" } });
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
    fireEvent.change(input, { target: { value: "30" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(diagnosticsConfigure).toHaveBeenCalledWith(false, 30));
    await waitFor(() => expect(input).toHaveValue(30));
  });

  it("keeps the selected trace stable while polling and can pause and resume", async () => {
    const data = snapshot();
    const { wrapper } = setup(data);
    render(
      <DiagnosticCommunicationDialog
        open
        onOpenChange={vi.fn()}
        initialTraceId="selected-detail"
      />,
      { wrapper }
    );
    await screen.findByText("upstream failed");
    expect(diagnosticsEvents).toHaveBeenCalledWith("selected-detail");
    data.traces.unshift({ ...data.traces[0], trace_id: "new-request" });
    vi.mocked(diagnosticsEvents).mockResolvedValue([event({ body: "live update" })]);
    await screen.findByText("live update", {}, { timeout: 3000 });
    expect(diagnosticsEvents).toHaveBeenLastCalledWith("selected-detail");
    fireEvent.click(screen.getByRole("button", { name: "暂停刷新" }));
    const calls = vi.mocked(diagnosticsEvents).mock.calls.length;
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 1200));
    });
    expect(diagnosticsEvents).toHaveBeenCalledTimes(calls);
    fireEvent.click(screen.getByRole("button", { name: "恢复实时" }));
    await waitFor(
      () => expect(vi.mocked(diagnosticsEvents).mock.calls.length).toBeGreaterThan(calls),
      { timeout: 3000 }
    );
  });

  it("copies selected communication and clears cached bodies after confirmation", async () => {
    const { wrapper } = setup();
    render(<DiagnosticCommunicationDialog open onOpenChange={vi.fn()} />, { wrapper });
    await screen.findByText("upstream failed");
    fireEvent.click(screen.getByRole("button", { name: "复制通信" }));
    await waitFor(() => expect(writeDesktopClipboardText).toHaveBeenCalled());
    const copied = JSON.parse(vi.mocked(writeDesktopClipboardText).mock.calls[0][0]);
    expect(copied.trace_id).toBe("trace-one");
    expect(copied.events[0].body).toBe("upstream failed");
    fireEvent.click(screen.getByRole("button", { name: "清空驻留" }));
    expect(diagnosticsClear).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "确认清空" }));
    await waitFor(() => expect(diagnosticsClear).toHaveBeenCalled());
    await waitFor(() => expect(screen.queryByText("upstream failed")).not.toBeInTheDocument());
  });

  it("reports disk errors, missing data, binary and truncated communication", async () => {
    const data = { ...snapshot(), dropped_messages: 2, last_error: "disk full" };
    data.traces[0].capture_limited = true;
    const { wrapper } = setup(data);
    vi.mocked(diagnosticsEvents).mockResolvedValue([
      event({ truncated: true, body_encoding: "base64", body: "AAEC", note: "stream cancelled" }),
    ]);
    render(<DiagnosticCommunicationDialog open onOpenChange={vi.fn()} />, { wrapper });
    await screen.findByText(/00000000\s+00 01 02/);
    expect(screen.queryByText("AAEC")).not.toBeInTheDocument();
    expect(screen.getByText(/disk full/)).toBeInTheDocument();
    expect(screen.getByText(/已丢弃 2 条更新/)).toBeInTheDocument();
    expect(screen.getByText(/后续通信未驻留/)).toBeInTheDocument();
    expect(screen.getByText(/内容已截断或缺失/)).toBeInTheDocument();
    expect(screen.getByText(/Base64/)).toBeInTheDocument();
    expect(screen.getByText("stream cancelled")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Base64" }));
    expect(screen.getByText("AAEC")).toBeInTheDocument();
  });

  it("switches binary views, copies the selected view and saves captured raw bytes", async () => {
    const { wrapper } = setup();
    vi.mocked(diagnosticsEvents).mockResolvedValue([
      event({ body_encoding: "base64", body: "AAEC", bytes_seen: 3 }),
    ]);
    vi.mocked(saveDesktopFilePath).mockResolvedValue("D:\\capture.bin");
    vi.mocked(diagnosticsSaveBody).mockResolvedValue(true);
    render(<DiagnosticCommunicationDialog open onOpenChange={vi.fn()} />, { wrapper });
    await screen.findByText(/00000000\s+00 01 02/);
    fireEvent.click(screen.getByRole("button", { name: "Base64" }));
    fireEvent.click(screen.getByRole("button", { name: "复制当前内容" }));
    await waitFor(() => expect(writeDesktopClipboardText).toHaveBeenCalledWith("AAEC"));
    fireEvent.click(screen.getByRole("button", { name: "十六进制" }));
    expect(screen.queryByText("AAEC")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "保存原始正文" }));
    await waitFor(() =>
      expect(diagnosticsSaveBody).toHaveBeenCalledWith("trace-one", "one", "D:\\capture.bin")
    );
  });

  it("does not save when the file dialog is cancelled", async () => {
    const { wrapper } = setup();
    vi.mocked(saveDesktopFilePath).mockResolvedValue(null);
    render(<DiagnosticCommunicationDialog open onOpenChange={vi.fn()} />, { wrapper });
    await screen.findByText("upstream failed");
    fireEvent.click(screen.getByRole("button", { name: "保存原始正文" }));
    await waitFor(() => expect(saveDesktopFilePath).toHaveBeenCalled());
    expect(diagnosticsSaveBody).not.toHaveBeenCalled();
  });

  it("falls back to hex if a recognized image cannot be rendered", async () => {
    const { wrapper } = setup();
    vi.mocked(diagnosticsEvents).mockResolvedValue([
      event({ body_encoding: "base64", body: "iVBORw0KGgo=", bytes_seen: 8 }),
    ]);
    render(<DiagnosticCommunicationDialog open onOpenChange={vi.fn()} />, { wrapper });
    const image = await screen.findByRole("img", { name: "驻留通信图片预览" });
    expect(image).toHaveAttribute("src", "data:image/png;base64,iVBORw0KGgo=");
    fireEvent.error(image);
    expect(screen.getByText(/图片预览失败/)).toBeInTheDocument();
    expect(screen.getByText(/89 50 4e 47/)).toBeInTheDocument();
  });

  it("surfaces IPC read errors without pretending there is an empty capture", async () => {
    const { wrapper } = setup();
    vi.mocked(diagnosticsSnapshot).mockRejectedValue(new Error("storage unavailable"));
    render(<DiagnosticCommunicationDialog open onOpenChange={vi.fn()} />, { wrapper });
    expect(await screen.findByText(/读取驻留信息失败：storage unavailable/)).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "通信信息驻留" })).toBeDisabled();
  });
});
