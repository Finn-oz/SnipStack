import { createWriteStream, existsSync, mkdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

/**
 * 下载截屏取字所需的 PP-OCRv5 mobile ONNX 模型到 src-tauri/resources/ocr/。
 * 模型文件不入 git(见 .gitignore),开发与 CI 构建前先跑一次本脚本。
 * 来源:oar-ocr GitHub Releases(Apache-2.0,转换自 PaddleOCR 官方模型)。
 */
const RELEASE_BASE =
  "https://github.com/GreatV/oar-ocr/releases/download/v0.3.0";

const MODELS: { name: string; minBytes: number }[] = [
  { minBytes: 4_000_000, name: "pp-ocrv5_mobile_det.onnx" },
  { minBytes: 15_000_000, name: "pp-ocrv5_mobile_rec.onnx" },
];

const targetDir = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "src-tauri",
  "resources",
  "ocr",
);

const download = async (name: string, minBytes: number) => {
  const target = join(targetDir, name);
  if (existsSync(target) && statSync(target).size >= minBytes) {
    process.stdout.write(`skip ${name} (already present)\n`);
    return;
  }
  const url = `${RELEASE_BASE}/${name}`;
  process.stdout.write(`fetch ${url}\n`);
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok || !response.body) {
    throw new Error(`download ${name} failed: HTTP ${response.status}`);
  }
  await pipeline(Readable.fromWeb(response.body), createWriteStream(target));
  const size = statSync(target).size;
  if (size < minBytes) {
    throw new Error(`download ${name} incomplete: ${size} bytes`);
  }
  process.stdout.write(
    `saved ${name} (${(size / 1024 / 1024).toFixed(1)} MiB)\n`,
  );
};

mkdirSync(targetDir, { recursive: true });
for (const model of MODELS) {
  await download(model.name, model.minBytes);
}
