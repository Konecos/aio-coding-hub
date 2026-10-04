import { Blob as NodeBlob } from "node:buffer";
import { DecompressionStream as NodeDecompressionStream } from "node:stream/web";
import { deflateSync, gzipSync } from "node:zlib";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DECODED_BODY_LIMIT,
  diagnosticBodyBytes,
  diagnosticHeader,
  diagnosticHex,
  formatDiagnosticText,
  previewDiagnosticBody,
} from "../diagnosticBodyPreview";

function body(bytes: Uint8Array, metadata = "", overrides = {}) {
  return {
    body: Buffer.from(bytes).toString("base64"),
    body_encoding: "base64",
    metadata,
    complete: true,
    truncated: false,
    ...overrides,
  };
}

function preview(event: ReturnType<typeof body>) {
  return previewDiagnosticBody(event, new AbortController().signal);
}

describe("diagnostic body previews", () => {
  beforeEach(() => {
    vi.stubGlobal("Blob", NodeBlob);
    vi.stubGlobal("DecompressionStream", NodeDecompressionStream);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("reads real diagnostic headers without confusing the first line with a header", () => {
    const metadata =
      "HTTP 200\n[content-type: application/json; charset=utf-8, Content-Encoding: GZip]";
    expect(diagnosticHeader(metadata, "content-encoding")).toBe("GZip");
    expect(diagnosticHeader(metadata, "content-type")).toBe("application/json; charset=utf-8");
    expect(
      diagnosticHeader(
        "[content-encoding: gzip, br, content-type: application/json]",
        "content-encoding"
      )
    ).toBe("gzip, br");
    expect(
      diagnosticHeader("POST /content-encoding: gzip\n[]", "content-encoding")
    ).toBeUndefined();
  });

  it("decodes gzip and deflate to readable JSON while preserving compressed bytes", async () => {
    for (const [encoding, compress] of [
      ["gzip", gzipSync],
      ["deflate", deflateSync],
    ] as const) {
      const bytes = compress(Buffer.from('{"message":"中文","ok":true}'));
      const result = await preview(body(bytes, `HTTP 200\n[content-encoding: ${encoding}]`));
      expect(JSON.parse(result.text!)).toEqual({ message: "中文", ok: true });
      expect(result.text).toContain('\n  "message"');
      expect(result.bytes).toEqual(new Uint8Array(bytes));
      expect(result.compression).toBe(encoding);
    }
  });

  it("recognizes gzip magic when headers are absent and keeps SSE text intact", async () => {
    const text = 'data: {"delta":"你好"}\n\ndata: [DONE]\n\n';
    expect((await preview(body(gzipSync(text)))).text).toBe(text);
  });

  it("formats JSON without changing large numbers, duplicate keys or escapes", () => {
    const json = '{"id":9007199254740993,"id":1e999,"value":"\\u4e2d","empty":[]}';
    const formatted = formatDiagnosticText(json);
    expect(formatted).toContain('"id": 9007199254740993');
    expect(formatted).toContain('"id": 1e999');
    expect(formatted).toContain('"value": "\\u4e2d"');
    expect(formatted).toContain('"empty": []');
    const nested = "[".repeat(100) + "0" + "]".repeat(100);
    expect(formatDiagnosticText(nested)).toBe(nested);
  });

  it("bounds decompression and rejects corrupt compressed content", async () => {
    const oversized = await preview(body(gzipSync("x".repeat(DECODED_BODY_LIMIT + 1))));
    expect(oversized.text).toBeUndefined();
    expect(oversized.note).toContain("256 KiB");
    const corrupt = await preview(body(new Uint8Array([0x1f, 0x8b, 0, 1])));
    expect(corrupt.note).toContain("解压失败");
    expect(corrupt.bytes).toEqual(new Uint8Array([0x1f, 0x8b, 0, 1]));
  });

  it("does not decode incomplete, truncated or unsupported compression", async () => {
    const bytes = gzipSync("hello");
    for (const overrides of [{ complete: false }, { truncated: true }]) {
      expect((await preview(body(bytes, "", overrides))).note).toContain("尚未采集完整");
    }
    expect((await preview(body(bytes, "[content-encoding: br]"))).note).toContain("暂不支持 br");
    vi.stubGlobal("DecompressionStream", undefined);
    expect((await preview(body(bytes))).note).toContain("当前环境不支持");
  });

  it("previews complete raster images and keeps incomplete images and SVG out of the image renderer", async () => {
    const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    expect((await preview(body(png))).imageMime).toBe("image/png");
    expect((await preview(body(png, "", { truncated: true }))).imageMime).toBeUndefined();
    const svg = await preview(body(new TextEncoder().encode('<svg onload="alert(1)"/>')));
    expect(svg.imageMime).toBeUndefined();
    expect(svg.text).toContain("<svg");
  });

  it("shows offsets and ASCII for unknown binary and caps the hex preview", async () => {
    const bytes = new Uint8Array([0, 1, 2, 0x41, 0xff]);
    expect((await preview(body(bytes))).text).toBeUndefined();
    expect(diagnosticHex(bytes)).toContain("00000000  00 01 02 41 ff");
    expect(diagnosticHex(bytes)).toContain("|...A.|");
    expect(diagnosticHex(new Uint8Array(1024))).toContain("仅预览前 512 字节，共保留 1024 字节");
    expect(await preview(body(new Uint8Array([0xff, 0xfe, 0x41])))).toMatchObject({
      text: undefined,
    });
    expect(diagnosticBodyBytes(body(bytes))).toEqual(bytes);
  });

  it("preserves empty bodies and reports invalid base64", async () => {
    expect((await preview(body(new Uint8Array()))).bytes.length).toBe(0);
    await expect(preview({ ...body(new Uint8Array()), body: "not!!base64" })).rejects.toThrow();
  });
});
