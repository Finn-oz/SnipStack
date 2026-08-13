import { createHash } from "node:crypto";
import {
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
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
 * 完整性:按 SHA-256 校验(哈希来自 oar-ocr 下载注册表,独立于资产本身的出处),
 * 字节数只作快速失败;先写 .part、校验哈希后再改名——被替换成同字节数的恶意模型会被
 * 哈希拦下,损坏/中断的残缺文件不会被当作已下载,更不会被打进签名安装包。
 * 升级模型版本时,RELEASE_BASE、字节数与哈希需与 packs.rs 的对应常量同步修改。
 */
const RELEASE_BASE =
  "https://github.com/GreatV/oar-ocr/releases/download/v0.3.0";

const MODELS: { name: string; bytes: number; sha256: string }[] = [
  {
    bytes: 4_826_518,
    name: "pp-ocrv5_mobile_det.onnx",
    sha256: "1eb7b4f7ab657ebd1c66d5f79bca7497f29768a2e3c15e52daecbba1a8e4a039",
  },
  {
    bytes: 16_562_373,
    name: "pp-ocrv5_mobile_rec.onnx",
    sha256: "243a0f06d826761323e9045e9b113ab2c191c3aa50565585e628300b8eda0224",
  },
];

const targetDir = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "src-tauri",
  "resources",
  "ocr",
);

const sha256Of = (path: string): string => {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
};

const download = async (name: string, bytes: number, sha256: string) => {
  const target = join(targetDir, name);
  if (existsSync(target) && statSync(target).size === bytes) {
    if (sha256Of(target) === sha256) {
      process.stdout.write(`skip ${name} (already present)\n`);
      return;
    }
    process.stdout.write(`refetch ${name} (hash mismatch)\n`);
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
  const digest = sha256Of(part);
  if (digest !== sha256) {
    rmSync(part, { force: true });
    throw new Error(
      `download ${name} failed verification: sha256 ${digest} != ${sha256}`,
    );
  }
  renameSync(part, target);
  process.stdout.write(
    `saved ${name} (${(size / 1024 / 1024).toFixed(1)} MiB)\n`,
  );
};

mkdirSync(targetDir, { recursive: true });
for (const model of MODELS) {
  await download(model.name, model.bytes, model.sha256);
}
