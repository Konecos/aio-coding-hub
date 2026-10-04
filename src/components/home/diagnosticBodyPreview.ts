import type { DiagnosticEvent } from "../../services/gateway/diagnostics";

export const DECODED_BODY_LIMIT = 256 * 1024;
const HEX_PREVIEW_LIMIT = 512;

export type DiagnosticBodyPreview = {
  bytes: Uint8Array;
  text?: string;
  imageMime?: string;
  compression?: string;
  note?: string;
};

type BodySource = Pick<
  DiagnosticEvent,
  "body" | "body_encoding" | "metadata" | "complete" | "truncated" | "preview_truncated"
>;

// Diagnostic metadata stores headers as `[name: value, name: value]`.
export function diagnosticHeader(metadata: string, name: string): string | undefined {
  return new RegExp(`(?:^|\\n|\\[|, )${name}:\\s*([^\\r\\n\\]]*?)(?=,\\s*[a-z0-9-]+:|\\]|$)`, "im")
    .exec(metadata)?.[1]
    .trim();
}

export function diagnosticBodyBytes(event: BodySource): Uint8Array {
  if (event.body_encoding !== "base64") return new TextEncoder().encode(event.body);
  return Uint8Array.from(atob(event.body), (char) => char.charCodeAt(0));
}

export function formatDiagnosticText(text: string): string {
  try {
    JSON.parse(text);
  } catch {
    return text;
  }
  // Format tokens instead of reserializing: diagnostics must preserve large
  // numbers, duplicate keys and the original string escapes.
  const tokens = text.match(/"(?:\\.|[^"\\])*"|[^\s]/g) ?? [];
  const parts: string[] = [];
  let depth = 0;
  let size = 0;
  for (let index = 0; index < tokens.length; index++) {
    const token = tokens[index];
    let part = token;
    if (token === "{" || token === "[") {
      depth++;
      if (depth > 40) return text;
      if (tokens[index + 1] !== "}" && tokens[index + 1] !== "]") part += `\n${"  ".repeat(depth)}`;
    } else if (token === "}" || token === "]") {
      depth--;
      if (tokens[index - 1] !== "{" && tokens[index - 1] !== "[")
        part = `\n${"  ".repeat(depth)}${token}`;
    } else if (token === ",") {
      part += `\n${"  ".repeat(depth)}`;
    } else if (token === ":") {
      part += " ";
    }
    size += part.length;
    if (size > DECODED_BODY_LIMIT * 2) return text;
    parts.push(part);
  }
  return parts.join("");
}

export function diagnosticHex(bytes: Uint8Array): string {
  const rows: string[] = [];
  for (let offset = 0; offset < Math.min(bytes.length, HEX_PREVIEW_LIMIT); offset += 16) {
    const chunk = bytes.subarray(offset, offset + 16);
    const hex = Array.from(chunk, (byte) => byte.toString(16).padStart(2, "0")).join(" ");
    const ascii = Array.from(chunk, (byte) =>
      byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : "."
    ).join("");
    rows.push(`${offset.toString(16).padStart(8, "0")}  ${hex.padEnd(47)}  |${ascii}|`);
  }
  if (bytes.length > HEX_PREVIEW_LIMIT) {
    rows.push(`… 仅预览前 ${HEX_PREVIEW_LIMIT} 字节，当前已加载 ${bytes.length} 字节`);
  }
  return rows.join("\n");
}

function imageMime(bytes: Uint8Array): string | undefined {
  const startsWith = (...signature: number[]) =>
    signature.every((byte, index) => bytes[index] === byte);
  if (startsWith(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)) return "image/png";
  if (startsWith(0xff, 0xd8, 0xff)) return "image/jpeg";
  const prefix = String.fromCharCode(...bytes.subarray(0, 12));
  if (prefix.startsWith("GIF87a") || prefix.startsWith("GIF89a")) return "image/gif";
  if (prefix.startsWith("RIFF") && prefix.slice(8) === "WEBP") return "image/webp";
  return undefined;
}

function readableText(bytes: Uint8Array, incomplete: boolean): string | undefined {
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes, { stream: incomplete });
    return /[\x00-\x08\x0b\x0c\x0e-\x1f]/.test(text) ? undefined : formatDiagnosticText(text);
  } catch {
    return undefined;
  }
}

async function decompress(
  bytes: Uint8Array,
  format: CompressionFormat,
  signal: AbortSignal
): Promise<Uint8Array> {
  const reader = new Blob([bytes as Uint8Array<ArrayBuffer>])
    .stream()
    .pipeThrough(new DecompressionStream(format))
    .getReader();
  const cancel = () => void reader.cancel().catch(() => {});
  signal.addEventListener("abort", cancel, { once: true });
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (!signal.aborted) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > DECODED_BODY_LIMIT) throw new Error("解压预览超过 256 KiB 上限");
      chunks.push(value);
    }
    const result = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) {
      result.set(chunk, offset);
      offset += chunk.length;
    }
    return result;
  } finally {
    signal.removeEventListener("abort", cancel);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

export async function previewDiagnosticBody(
  event: BodySource,
  signal: AbortSignal
): Promise<DiagnosticBodyPreview> {
  const bytes = diagnosticBodyBytes(event);
  const preview: DiagnosticBodyPreview = { bytes };
  if (!bytes.length) return preview;
  const encoding = diagnosticHeader(event.metadata, "content-encoding")?.toLowerCase();
  const gzipMagic = bytes[0] === 0x1f && bytes[1] === 0x8b;
  const compression =
    encoding && encoding !== "identity" ? encoding : gzipMagic ? "gzip" : undefined;
  let decoded = bytes;
  if (compression) {
    preview.compression = compression;
    if (event.preview_truncated) {
      return { ...preview, note: "当前只加载了压缩正文开头，完整驻留内容可通过保存原始正文导出。" };
    }
    if (compression !== "gzip" && compression !== "deflate" && compression !== "x-gzip") {
      return { ...preview, note: `暂不支持 ${compression} 解压，可查看或保存原始字节。` };
    }
    if (!event.complete || event.truncated) {
      return { ...preview, note: "压缩内容尚未采集完整，无法可靠解压；可查看或保存已采集字节。" };
    }
    if (typeof DecompressionStream === "undefined") {
      return { ...preview, note: "当前环境不支持解压预览，可查看或保存原始字节。" };
    }
    try {
      decoded = await decompress(bytes, compression === "x-gzip" ? "gzip" : compression, signal);
    } catch (error) {
      const limit = error instanceof Error && error.message.includes("256 KiB");
      return {
        ...preview,
        note: limit ? error.message : "解压失败，内容可能损坏；已保留原始字节。",
      };
    }
  }
  // Render only recognized raster formats; HTML and SVG remain plain text.
  const mime = imageMime(decoded);
  if (mime && !compression && event.complete && !event.truncated && !event.preview_truncated) {
    preview.imageMime = mime;
  } else {
    preview.text = readableText(
      decoded,
      !event.complete || event.truncated || event.preview_truncated
    );
    if (mime)
      preview.note = event.preview_truncated
        ? "图片预览只包含正文开头，请保存完整驻留正文后查看。"
        : "图片内容不完整或经过压缩，请保存原始字节后查看。";
    else if (preview.text == null)
      preview.note = "无法识别为可读文本或图片，显示原始字节的十六进制摘要。";
  }
  return preview;
}
