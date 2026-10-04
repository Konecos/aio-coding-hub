import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { writeDesktopClipboardText } from "../../services/desktop/clipboard";
import { saveDesktopFilePath } from "../../services/desktop/dialog";
import { diagnosticsSaveBody, type DiagnosticEvent } from "../../services/gateway/diagnostics";
import { Button } from "../../ui/Button";
import {
  diagnosticHex,
  previewDiagnosticBody,
  type DiagnosticBodyPreview,
} from "./diagnosticBodyPreview";

export function DiagnosticBody({ event, traceId }: { event: DiagnosticEvent; traceId: string }) {
  const [view, setView] = useState<"preview" | "hex" | "base64">("preview");
  const [saving, setSaving] = useState(false);
  const [imageFailed, setImageFailed] = useState(false);
  const source = useMemo(
    () => ({
      body: event.body,
      body_encoding: event.body_encoding,
      metadata: event.metadata,
      complete: event.complete,
      truncated: event.truncated,
      preview_truncated: event.preview_truncated,
    }),
    [
      event.body,
      event.body_encoding,
      event.metadata,
      event.complete,
      event.truncated,
      event.preview_truncated,
    ]
  );
  const [result, setResult] = useState<{
    source: typeof source;
    preview?: DiagnosticBodyPreview;
    error?: string;
  }>();
  useEffect(() => {
    const controller = new AbortController();
    setImageFailed(false);
    previewDiagnosticBody(source, controller.signal).then(
      (preview) => {
        if (!controller.signal.aborted) setResult({ source, preview });
      },
      () => {
        if (!controller.signal.aborted)
          setResult({ source, error: "无法读取正文编码，请查看原始 Base64。" });
      }
    );
    return () => controller.abort();
  }, [source]);
  const current = result?.source === source ? result : undefined;
  const preview = current?.preview;
  const binary = event.body_encoding === "base64";
  const activeView = binary ? view : "preview";
  const hex = useMemo(() => (preview ? diagnosticHex(preview.bytes) : ""), [preview]);
  const text =
    activeView === "base64" ? event.body : activeView === "hex" ? hex : (preview?.text ?? hex);
  const showImage = activeView === "preview" && preview?.imageMime && !imageFailed;

  async function copy(text: string) {
    try {
      await writeDesktopClipboardText(text);
      toast.success("已复制正文");
    } catch (error) {
      toast.error(String(error));
    }
  }

  async function save() {
    setSaving(true);
    try {
      const extension =
        preview?.compression === "gzip" || preview?.compression === "x-gzip"
          ? "gz"
          : (preview?.imageMime?.split("/")[1] ?? (binary ? "bin" : "txt"));
      const path = await saveDesktopFilePath({
        title: "保存已采集的原始正文",
        defaultPath: `communication-${event.phase}${event.truncated || !event.complete ? "-partial" : ""}.${extension}`,
      });
      if (!path) return;
      await diagnosticsSaveBody(traceId, event.id, path);
      toast.success("已保存原始正文（仅包含已驻留字节）");
    } catch (error) {
      toast.error(error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="mt-2 space-y-2 border-t pt-2">
      <div className="flex flex-wrap items-center gap-2">
        {binary && (
          <>
            <Button
              size="sm"
              variant="ghost"
              aria-pressed={view === "preview"}
              onClick={() => setView("preview")}
            >
              预览
            </Button>
            <Button
              size="sm"
              variant="ghost"
              aria-pressed={view === "hex"}
              onClick={() => setView("hex")}
              disabled={!preview}
            >
              十六进制
            </Button>
            <Button
              size="sm"
              variant="ghost"
              aria-pressed={view === "base64"}
              onClick={() => setView("base64")}
            >
              Base64
            </Button>
            <span className="text-muted-foreground">
              {preview?.compression ? `${preview.compression} 压缩 · ` : ""}
              {preview ? `已驻留 ${event.retained_bytes} 字节` : "二进制正文"}
            </span>
          </>
        )}
        <Button
          size="sm"
          variant="ghost"
          onClick={() => copy(text)}
          disabled={!text || !!showImage}
        >
          复制当前内容
        </Button>
        <Button size="sm" variant="ghost" onClick={save} disabled={saving || !preview}>
          {saving ? "保存中…" : "保存原始正文"}
        </Button>
      </div>
      {event.preview_truncated && (
        <p className="text-muted-foreground">
          当前仅预览正文开头，已驻留 {event.retained_bytes} 字节；保存原始正文可导出完整驻留内容。
        </p>
      )}
      {preview?.note && <p className="text-amber-600">{preview.note}</p>}
      {current?.error && (
        <p role="alert" className="text-amber-600">
          {current.error}
        </p>
      )}
      {imageFailed && <p className="text-amber-600">图片预览失败，可查看或保存原始字节。</p>}
      {!current && activeView !== "base64" ? (
        <p className="text-muted-foreground">正在解析正文…</p>
      ) : showImage ? (
        <img
          src={`data:${preview.imageMime};base64,${event.body}`}
          alt="驻留通信图片预览"
          className="max-h-72 max-w-full rounded object-contain"
          onError={() => setImageFailed(true)}
        />
      ) : (
        <pre
          className={`max-h-72 overflow-auto font-mono ${activeView === "hex" || (activeView === "preview" && preview?.text == null) ? "whitespace-pre" : "whitespace-pre-wrap break-all"}`}
        >
          {text || (current?.error ? "（正文无法解析）" : "（无正文）")}
        </pre>
      )}
    </div>
  );
}
