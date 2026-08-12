import {
  createWriteStream,
  existsSync,
  mkdirSync,
  renameSync,
  rmSync,
  statSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

/**
 * 下载截屏取字所需的 PP-OCRv5 mobile ONNX 模型到 src-tauri/resources/ocr/。
 * 模型文件不入 git(见 .gitignore),开发与 CI 构建前先跑一次本脚本。
 * 来源:oar-ocr GitHub Releases(Apache-2.0,转换自 PaddleOCR 官方模型)。
 *
 * 完整性:按精确字节数校验(与 src-tauri/src/ocr/packs.rs 同一策略),
 * 先写 .part 再改名——中断留下的残缺文件不会被当作已下载,更不会被打进安装包。
 * 升级模型版本时,RELEASE_BASE 与字节数需与 packs.rs 的 GITHUB_BASE/PACKS 同步修改。
 */
const RELEASE_BASE =
  "https://github.com/GreatV/oar-ocr/releases/download/v0.3.0";

const MODELS: { name: string; bytes: number }[] = [
  { bytes: 4_826_518, name: "pp-ocrv5_mobile_det.onnx" },
  { bytes: 16_562_373, name: "pp-ocrv5_mobile_rec.onnx" },
];

const targetDir = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "src-tauri",
  "resources",
  "ocr",
);

const download = async (name: string, bytes: number) => {
  const target = join(targetDir, name);
  if (existsSync(target)) {
    const size = statSync(target).size;
    if (size === bytes) {
      process.stdout.write(`skip ${name} (already present)\n`);
      return;
    }
    process.stdout.write(
      `refetch ${name} (${size} bytes, expected ${bytes})\n`,
    );
  }
  const url = `${RELEASE_BASE}/${name}`;
  process.stdout.write(`fetch ${url}\n`);
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok || !response.body) {
    throw new Error(`download ${name} failed: HTTP ${response.status}`);
  }
  const part = `${target}.part`;
  await pipeline(Readable.fromWeb(response.body), createWriteStream(part));
  const size = statSync(part).size;
  if (size !== bytes) {
    rmSync(part, { force: true });
    throw new Error(
      `download ${name} corrupt: got ${size} bytes, expected ${bytes}`,
    );
  }
  renameSync(part, target);
  process.stdout.write(
    `saved ${name} (${(size / 1024 / 1024).toFixed(1)} MiB)\n`,
  );
};

mkdirSync(targetDir, { recursive: true });
for (const model of MODELS) {
  await download(model.name, model.bytes);
}
