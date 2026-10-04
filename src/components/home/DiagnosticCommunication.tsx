import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import {
  useDiagnosticsClear,
  useDiagnosticsConfigure,
  useDiagnosticsEvents,
  useDiagnosticsSnapshot,
} from "../../query/diagnostics";
import type { DiagnosticEvent } from "../../services/gateway/diagnostics";
import { writeDesktopClipboardText } from "../../services/desktop/clipboard";
import { Button } from "../../ui/Button";
import { Dialog } from "../../ui/Dialog";
import { Input } from "../../ui/Input";
import { Switch } from "../../ui/Switch";

const PHASE_LABELS: Record<string, string> = {
  client_request: "客户端 → 网关",
  upstream_request: "网关 → 供应商",
  upstream_response: "供应商 → 网关",
  client_response: "网关 → 客户端",
  transport_error: "连接 / 发送错误",
};

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export function DiagnosticRetentionControl() {
  return isTauri() ? <RetentionControl /> : null;
}

function RetentionControl() {
  const snapshot = useDiagnosticsSnapshot();
  const configure = useDiagnosticsConfigure();
  const [open, setOpen] = useState(false);
  return (
    <>
      <div
        className="flex items-center gap-1.5"
        title={
          snapshot.error
            ? `读取驻留状态失败：${errorText(snapshot.error)}，请打开通信查看重试`
            : `保留通信正文到本地，可能包含提示词和代码；驻留 ${snapshot.data?.retention_days ?? 15} 天`
        }
      >
        <span className="text-xs text-muted-foreground">信息驻留</span>
        <Switch
          size="sm"
          aria-label="信息驻留"
          checked={snapshot.data?.enabled ?? false}
          disabled={!snapshot.data || configure.isPending}
          onCheckedChange={(enabled) =>
            configure.mutate(
              { enabled, days: snapshot.data?.retention_days ?? 15 },
              { onError: (error) => toast.error(errorText(error)) }
            )
          }
        />
      </div>
      <Button variant="ghost" size="sm" onClick={() => setOpen(true)}>
        通信查看
      </Button>
      {open && <DiagnosticCommunicationDialog open onOpenChange={setOpen} />}
    </>
  );
}

export function DiagnosticCommunicationDialog({
  open,
  onOpenChange,
  initialTraceId,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  initialTraceId?: string;
}) {
  const [live, setLive] = useState(true);
  const [selectedTrace, setSelectedTrace] = useState<string | null>(initialTraceId ?? null);
  const [daysDraft, setDaysDraft] = useState<string | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const snapshot = useDiagnosticsSnapshot(open && live);
  const traceId = selectedTrace ?? snapshot.data?.traces[0]?.trace_id ?? null;
  useEffect(() => {
    if (selectedTrace == null && traceId != null) setSelectedTrace(traceId);
  }, [selectedTrace, traceId]);
  const events = useDiagnosticsEvents(open ? traceId : null, open && live);
  const configure = useDiagnosticsConfigure();
  const clear = useDiagnosticsClear();
  const days = daysDraft ?? String(snapshot.data?.retention_days ?? 15);
  const validDays = /^\d+$/.test(days) && Number(days) >= 1 && Number(days) <= 365;
  const busy = configure.isPending || clear.isPending;

  async function copy() {
    try {
      await writeDesktopClipboardText(
        JSON.stringify({ trace_id: traceId, events: events.data ?? [] }, null, 2)
      );
      toast.success("已复制通信内容");
    } catch (error) {
      toast.error(errorText(error));
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange} title="通信信息驻留" className="max-w-5xl">
      <div className="space-y-3">
        <p className="text-xs text-muted-foreground">
          开启后采集新请求，通信正文保存在本地，可能包含提示词和代码。认证头及 URL
          查询值已脱敏；关闭后已有内容仍可查看。
        </p>
        <div className="flex flex-wrap items-center gap-3">
          <label className="flex items-center gap-2 text-sm">
            信息驻留
            <Switch
              aria-label="通信信息驻留"
              checked={snapshot.data?.enabled ?? false}
              disabled={!snapshot.data || busy}
              onCheckedChange={(enabled) =>
                configure.mutate(
                  { enabled, days: snapshot.data?.retention_days ?? 15 },
                  { onError: (error) => toast.error(errorText(error)) }
                )
              }
            />
          </label>
          <label className="flex items-center gap-2 text-sm">
            驻留天数
            <Input
              aria-label="驻留天数"
              type="number"
              min={1}
              max={365}
              step={1}
              className="w-20"
              value={days}
              onChange={(e) => setDaysDraft(e.target.value)}
            />
          </label>
          <Button
            size="sm"
            disabled={!snapshot.data || !validDays || busy}
            onClick={() =>
              configure.mutate(
                { enabled: snapshot.data?.enabled ?? false, days: Number(days) },
                {
                  onSuccess: () => {
                    setDaysDraft(null);
                    toast.success("驻留时间已保存");
                  },
                  onError: (error) => toast.error(errorText(error)),
                }
              )
            }
          >
            保存
          </Button>
          <Button size="sm" variant="secondary" onClick={() => setLive((value) => !value)}>
            {live ? "暂停刷新" : "恢复实时"}
          </Button>
          <Button size="sm" variant="ghost" onClick={copy} disabled={!events.data?.length}>
            复制通信
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => setConfirmClear(true)}
            disabled={!snapshot.data || busy}
          >
            清空驻留
          </Button>
        </div>
        {!validDays && (
          <p role="alert" className="text-xs text-red-500">
            驻留时间必须为 1–365 天的整数。
          </p>
        )}
        {confirmClear && (
          <div className="flex flex-wrap items-center gap-2 rounded-lg border p-3 text-sm">
            清空所有已驻留通信内容？
            <Button
              size="sm"
              disabled={busy}
              onClick={() =>
                clear.mutate(undefined, {
                  onSuccess: () => {
                    setConfirmClear(false);
                    setSelectedTrace(null);
                    toast.success("已清空驻留内容");
                  },
                  onError: (error) => toast.error(errorText(error)),
                })
              }
            >
              确认清空
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={() => setConfirmClear(false)}
            >
              取消
            </Button>
          </div>
        )}
        <p className="text-xs text-muted-foreground">
          {live ? "每秒刷新" : "刷新已暂停"} · 已存储{" "}
          {((snapshot.data?.stored_bytes ?? 0) / 1024 / 1024).toFixed(2)} MiB · 每条正文最多 64
          KiB，每请求最多 64 条 · 总量最多 50 MiB / 1000 个请求，超限清理最旧内容；下方显示最近 100
          个请求。
        </p>
        {snapshot.error && (
          <p role="alert" className="text-sm text-red-500">
            读取驻留信息失败：{errorText(snapshot.error)}
          </p>
        )}
        {snapshot.data?.last_error && (
          <p role="alert" className="text-sm text-red-500">
            写入失败，通信可能不完整：{snapshot.data.last_error}
          </p>
        )}
        {!!snapshot.data?.dropped_messages && (
          <p role="alert" className="text-sm text-amber-600">
            采集队列拥塞，已丢弃 {snapshot.data.dropped_messages} 条更新，通信可能不完整。
          </p>
        )}
        {snapshot.data?.traces.find((trace) => trace.trace_id === traceId)?.capture_limited && (
          <p role="alert" className="text-sm text-amber-600">
            此请求通信条数已达到 64 条上限，后续通信未驻留。
          </p>
        )}
        <div className="grid gap-3 md:grid-cols-[230px_minmax(0,1fr)]">
          <div className="max-h-[55vh] overflow-auto space-y-1 rounded-lg border p-2">
            {!snapshot.isLoading && !snapshot.error && !snapshot.data?.traces.length && (
              <p className="text-sm text-muted-foreground">
                暂无驻留通信。开启后重新发起请求即可采集。
              </p>
            )}
            {snapshot.data?.traces.map((trace) => (
              <button
                type="button"
                key={trace.trace_id}
                aria-pressed={traceId === trace.trace_id}
                className={`w-full rounded-md p-2 text-left text-xs ${traceId === trace.trace_id ? "bg-secondary" : "hover:bg-secondary/50"}`}
                onClick={() => setSelectedTrace(trace.trace_id)}
              >
                <div className="font-semibold">
                  {trace.cli_key} · {trace.status == null ? "处理中 / 未完成" : trace.status}
                </div>
                <div className="break-all">
                  {trace.method} {trace.path}
                </div>
                <div className="text-muted-foreground">
                  {new Date(trace.created_at_ms).toLocaleString()}
                </div>
              </button>
            ))}
          </div>
          <div className="max-h-[55vh] overflow-auto space-y-3 min-w-0">
            {traceId && <p className="break-all font-mono text-xs">Trace ID: {traceId}</p>}
            {events.isLoading && <p className="text-sm text-muted-foreground">加载通信中…</p>}
            {events.error && (
              <p role="alert" className="text-sm text-red-500">
                读取通信失败：{errorText(events.error)}
              </p>
            )}
            {traceId && !events.isLoading && !events.error && !events.data?.length && (
              <p className="text-sm text-muted-foreground">
                该请求无驻留通信，可能未开启采集或已被留存策略清理。
              </p>
            )}
            {events.data?.map((event) => (
              <CommunicationEvent key={event.id} event={event} />
            ))}
          </div>
        </div>
      </div>
    </Dialog>
  );
}

function CommunicationEvent({ event }: { event: DiagnosticEvent }) {
  return (
    <details open className="rounded-lg border p-3 text-xs">
      <summary className="cursor-pointer font-semibold">
        {PHASE_LABELS[event.phase] ?? event.phase} ·{" "}
        {new Date(event.created_at_ms).toLocaleTimeString()} ·{" "}
        {event.complete ? "采集结束" : "接收中"} · {event.bytes_seen} bytes
        {event.truncated ? " · 内容已截断或缺失" : ""}
      </summary>
      {event.note && <p className="mt-2 text-amber-600">{event.note}</p>}
      {event.body_encoding === "base64" && (
        <p className="mt-2 text-muted-foreground">二进制 / 压缩内容，以 Base64 显示。</p>
      )}
      <pre className="mt-2 whitespace-pre-wrap break-all font-mono">{event.metadata}</pre>
      <pre className="mt-2 max-h-72 overflow-auto whitespace-pre-wrap break-all border-t pt-2 font-mono">
        {event.body || "（无正文）"}
      </pre>
    </details>
  );
}
