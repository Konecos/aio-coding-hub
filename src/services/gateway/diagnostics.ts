import { commands, type DiagnosticEvent, type DiagnosticSnapshot } from "../../generated/bindings";
import { invokeGeneratedIpc } from "../generatedIpc";

export type {
  DiagnosticEvent,
  DiagnosticSnapshot,
  DiagnosticTrace,
} from "../../generated/bindings";

export function diagnosticsSnapshot() {
  return invokeGeneratedIpc<DiagnosticSnapshot>({
    title: "读取通信驻留失败",
    cmd: "gateway_diagnostics_snapshot",
    invoke: () => commands.gatewayDiagnosticsSnapshot(),
  });
}

export function diagnosticsEvents(traceId: string) {
  return invokeGeneratedIpc<DiagnosticEvent[]>({
    title: "读取通信内容失败",
    cmd: "gateway_diagnostics_events",
    invoke: () => commands.gatewayDiagnosticsEvents(traceId),
  });
}

export function diagnosticsSaveBody(traceId: string, eventId: string, path: string) {
  return invokeGeneratedIpc<null, boolean>({
    title: "保存通信正文失败",
    cmd: "gateway_diagnostics_save_body",
    nullResultBehavior: "return_fallback",
    fallback: true,
    invoke: () => commands.gatewayDiagnosticsSaveBody(traceId, eventId, path),
  });
}

export function diagnosticsConfigure(enabled: boolean, retentionDays: number) {
  if (!Number.isInteger(retentionDays) || retentionDays < 1 || retentionDays > 365) {
    return Promise.reject(new Error("驻留时间必须为 1–365 天的整数"));
  }
  return invokeGeneratedIpc<null, boolean>({
    title: "保存信息驻留设置失败",
    cmd: "gateway_diagnostics_configure",
    // Generated Result<(), String> carries null on success.
    nullResultBehavior: "return_fallback",
    fallback: true,
    invoke: () => commands.gatewayDiagnosticsConfigure(enabled, retentionDays),
  });
}

export function diagnosticsClear() {
  return invokeGeneratedIpc<null, boolean>({
    title: "清空通信驻留失败",
    cmd: "gateway_diagnostics_clear",
    nullResultBehavior: "return_fallback",
    fallback: true,
    invoke: () => commands.gatewayDiagnosticsClear(),
  });
}
