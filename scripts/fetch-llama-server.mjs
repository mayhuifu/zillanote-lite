// Downloads the pinned llama.cpp release that serves Qwen3-ASR into
// app/resources/llama-server/, where the app looks before PATH and Homebrew.
//
//   node scripts/fetch-llama-server.mjs [--force]
//
// The build is pinned because llama.cpp's audio support is young: b10964 is the build
// the Qwen3-ASR spike and the live tests ran against. Moving to a newer one means
// re-running those tests, then updating TAG and the digests below from the release page.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const TAG = "b10964";

// SHA-256 digests as published on the GitHub release.
const ASSETS = {
  "darwin-arm64": {
    name: `llama-${TAG}-bin-macos-arm64.tar.gz`,
    sha256: "033c845c1df9bf945ff37bb193238b40910b2244be3e1e637b2ceb5878f1a6f5",
  },
  "darwin-x64": {
    name: `llama-${TAG}-bin-macos-x64.tar.gz`,
    sha256: "03430a394d0a169a5e6d8f01c09f48cf58eb026af6fc95940a4a528e2e50cf38",
  },
  "win32-x64": {
    name: `llama-${TAG}-bin-win-cpu-x64.zip`,
    sha256: "917f39c076402c421224824607397af20f53625a60defc20e8dd22446bf4c5d7",
  },
  "win32-arm64": {
    name: `llama-${TAG}-bin-win-cpu-arm64.zip`,
    sha256: "4b6a004b076eea47c318bea35cf1db2ff2bf037738b04645646ae8d7c3159478",
  },
};

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const targetDir = path.join(scriptDir, "..", "app", "resources", "llama-server");
const stampPath = path.join(targetDir, ".fetched");
const binaryName =
  process.platform === "win32" ? "llama-server.exe" : "llama-server";

const asset = ASSETS[`${process.platform}-${process.arch}`];
if (!asset) {
  console.log(
    `[fetch-llama-server] no pinned build for ${process.platform}-${process.arch}; the app will look for llama-server on PATH.`,
  );
  process.exit(0);
}

const stamp = `${TAG} ${asset.sha256}`;
const upToDate =
  fs.existsSync(path.join(targetDir, binaryName)) &&
  fs.existsSync(stampPath) &&
  fs.readFileSync(stampPath, "utf8").trim() === stamp;
if (upToDate && !process.argv.includes("--force")) {
  console.log(`[fetch-llama-server] ${TAG} already in place.`);
  process.exit(0);
}

const url = `https://github.com/ggml-org/llama.cpp/releases/download/${TAG}/${asset.name}`;
const workDir = fs.mkdtempSync(path.join(os.tmpdir(), "llama-server-"));
const archivePath = path.join(workDir, asset.name);

function run(command, args) {
  const result = spawnSync(command, args, { stdio: "inherit" });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed`);
  }
}

// Every file below `dir`, so the archive's own folder layout does not matter.
function filesUnder(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const entryPath = path.join(dir, entry.name);
    return entry.isDirectory() ? filesUnder(entryPath) : [entryPath];
  });
}

// The macOS archive holds every library under three names (two of them symlinks) and
// the libraries of tools we do not ship. Following the load commands from the server
// keeps one copy of exactly what it loads, under the names it asks for.
function linkedClosure(binary) {
  const dir = path.dirname(binary);
  const seen = new Set([binary]);
  const queue = [binary];

  while (queue.length > 0) {
    const result = spawnSync("otool", ["-L", queue.pop()], {
      encoding: "utf8",
    });
    if (result.status !== 0) {
      throw new Error(`otool failed: ${result.stderr}`);
    }
    for (const match of result.stdout.matchAll(/@rpath\/(\S+)/g)) {
      const library = path.join(dir, match[1]);
      if (!seen.has(library)) {
        if (!fs.existsSync(library)) {
          throw new Error(`${match[1]} is linked but not in the archive`);
        }
        seen.add(library);
        queue.push(library);
      }
    }
  }

  return [...seen];
}

try {
  console.log(`[fetch-llama-server] downloading ${url}`);
  // curl follows the machine's proxy settings, which Node's fetch does not.
  run("curl", [
    "-fL",
    "--retry",
    "5",
    "--retry-all-errors",
    "-o",
    archivePath,
    url,
  ]);

  const actual = createHash("sha256")
    .update(fs.readFileSync(archivePath))
    .digest("hex");
  if (actual !== asset.sha256) {
    throw new Error(
      `checksum mismatch for ${asset.name}: expected ${asset.sha256}, got ${actual}`,
    );
  }

  const extractDir = path.join(workDir, "extracted");
  fs.mkdirSync(extractDir);
  // bsdtar, which ships with macOS and Windows 10+, reads both .tar.gz and .zip.
  // Git for Windows brings a GNU tar that may come first on the PATH and reads no .zip.
  const tar =
    process.platform === "win32"
      ? path.join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe")
      : "tar";
  run(tar, ["-xf", archivePath, "-C", extractDir]);

  const extracted = filesUnder(extractDir);
  const binary = extracted.find((file) => path.basename(file) === binaryName);
  if (!binary) {
    throw new Error(`${binaryName} is not in ${asset.name}`);
  }

  // The server loads its libraries from its own folder, so they travel with it. The
  // other command-line tools in the archive are left out.
  const wanted =
    process.platform === "darwin"
      ? linkedClosure(binary)
      : extracted.filter(
          (file) =>
            file === binary ||
            (path.dirname(file) === path.dirname(binary) &&
              file.endsWith(".dll")),
        );

  fs.mkdirSync(targetDir, { recursive: true });
  for (const entry of fs.readdirSync(targetDir)) {
    if (entry !== "README.md") {
      fs.rmSync(path.join(targetDir, entry), { recursive: true, force: true });
    }
  }
  for (const file of wanted) {
    // Symlinked library names are copied as the files they point to.
    fs.copyFileSync(file, path.join(targetDir, path.basename(file)));
  }
  if (process.platform !== "win32") {
    fs.chmodSync(path.join(targetDir, binaryName), 0o755);
  }
  fs.writeFileSync(stampPath, `${stamp}\n`);

  console.log(
    `[fetch-llama-server] ${TAG}: ${wanted.length} files in ${path.relative(process.cwd(), targetDir)}`,
  );
} finally {
  fs.rmSync(workDir, { recursive: true, force: true });
}
