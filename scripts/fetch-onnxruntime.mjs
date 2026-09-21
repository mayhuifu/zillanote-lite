// Downloads the ONNX Runtime static library the speaker models run on into
// vendor/onnxruntime/, where .cargo/config.toml tells the `ort` crate to find it. The
// crate can download it by itself, but not in an offline build, which is how this
// project builds.
//
//   node scripts/fetch-onnxruntime.mjs [--force]
//
// The URLs and digests are the ones `ort-sys` 2.0.0-rc.10 pins in its dist.txt
// (ONNX Runtime 1.22.0). Moving to another `ort` version means taking them from there.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const VERSION = "1.22.0";
const BASE = `https://cdn.pyke.io/0/pyke:ort-rs/ms@${VERSION}`;

const ASSETS = {
  "darwin-arm64": {
    name: "aarch64-apple-darwin.tgz",
    sha256: "00fbfd6f08bac2a4e28c66723af900d58d1b4b1c73efba6290637cd3019883d5",
  },
  "win32-x64": {
    name: "x86_64-pc-windows-msvc.tgz",
    sha256: "540d19b3379fda6fb8f7280d8c15efde20ed225a67a357a6dae38c4300fe190d",
  },
};

// Windows' own tar (bsdtar). Git for Windows brings a GNU tar that may come first on the
// PATH and reads neither .zip nor a path with a drive letter.
const TAR =
  process.platform === "win32"
    ? path.join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe")
    : "tar";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const targetDir = path.join(scriptDir, "..", "vendor", "onnxruntime");
const stampPath = path.join(targetDir, ".fetched");

const asset = ASSETS[`${process.platform}-${process.arch}`];
if (!asset) {
  console.error(
    `[fetch-onnxruntime] no pinned build for ${process.platform}-${process.arch}: add its line from ort-sys's dist.txt.`,
  );
  process.exit(1);
}

const stamp = `${VERSION} ${asset.sha256}`;
const upToDate =
  fs.existsSync(path.join(targetDir, "lib")) &&
  fs.existsSync(stampPath) &&
  fs.readFileSync(stampPath, "utf8").trim() === stamp;
if (upToDate && !process.argv.includes("--force")) {
  console.log(`[fetch-onnxruntime] ${VERSION} already in place.`);
  process.exit(0);
}

function run(command, args) {
  const result = spawnSync(command, args, { stdio: "inherit" });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed`);
  }
}

const workDir = fs.mkdtempSync(path.join(os.tmpdir(), "onnxruntime-"));
try {
  const url = `${BASE}/${asset.name}`;
  const archivePath = path.join(workDir, asset.name);
  console.log(`[fetch-onnxruntime] downloading ${url}`);
  // curl follows the machine's proxy settings, which Node's fetch does not.
  run("curl", ["-fL", "--retry", "5", "--retry-all-errors", "-o", archivePath, url]);

  const actual = createHash("sha256").update(fs.readFileSync(archivePath)).digest("hex");
  if (actual !== asset.sha256) {
    throw new Error(`checksum mismatch for ${asset.name}: expected ${asset.sha256}, got ${actual}`);
  }

  // The archive holds one folder, onnxruntime/, with lib/ inside.
  fs.rmSync(targetDir, { recursive: true, force: true });
  fs.mkdirSync(path.dirname(targetDir), { recursive: true });
  run(TAR, ["-xf", archivePath, "-C", path.dirname(targetDir)]);
  fs.writeFileSync(stampPath, `${stamp}\n`);

  console.log(`[fetch-onnxruntime] ${VERSION} in ${path.relative(process.cwd(), targetDir)}`);
} finally {
  fs.rmSync(workDir, { recursive: true, force: true });
}
