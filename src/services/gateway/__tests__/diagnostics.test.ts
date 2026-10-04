import { describe, expect, it, vi } from "vitest";
import { commands } from "../../../generated/bindings";
import {
  diagnosticsClear,
  diagnosticsConfigure,
  diagnosticsEvents,
  diagnosticsSnapshot,
} from "../diagnostics";

vi.mock("../../../generated/bindings", () => ({
  commands: {
    gatewayDiagnosticsConfigure: vi.fn(),
    gatewayDiagnosticsClear: vi.fn(),
    gatewayDiagnosticsSnapshot: vi.fn(),
    gatewayDiagnosticsEvents: vi.fn(),
  },
}));
vi.mock("../../consoleLog", () => ({ logToConsole: vi.fn() }));

describe("diagnostics IPC", () => {
  it("rejects invalid retention days before invoking IPC", async () => {
    for (const days of [0, 366, 1.5, NaN, Infinity]) {
      await expect(diagnosticsConfigure(true, days)).rejects.toThrow("1–365");
    }
    expect(commands.gatewayDiagnosticsConfigure).not.toHaveBeenCalled();
  });
  it("accepts successful void results and forwards arguments", async () => {
    vi.mocked(commands.gatewayDiagnosticsConfigure).mockResolvedValue({ status: "ok", data: null });
    vi.mocked(commands.gatewayDiagnosticsClear).mockResolvedValue({ status: "ok", data: null });
    await expect(diagnosticsConfigure(true, 15)).resolves.toBe(true);
    expect(commands.gatewayDiagnosticsConfigure).toHaveBeenCalledWith(true, 15);
    await expect(diagnosticsClear()).resolves.toBe(true);
  });
  it("unwraps snapshots and events and surfaces backend failures", async () => {
    vi.mocked(commands.gatewayDiagnosticsSnapshot).mockResolvedValue({
      status: "error",
      error: "disk full",
    });
    await expect(diagnosticsSnapshot()).rejects.toThrow("disk full");
    vi.mocked(commands.gatewayDiagnosticsEvents).mockResolvedValue({ status: "ok", data: [] });
    await expect(diagnosticsEvents("trace")).resolves.toEqual([]);
    expect(commands.gatewayDiagnosticsEvents).toHaveBeenCalledWith("trace");
  });
});
