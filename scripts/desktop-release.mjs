#!/usr/bin/env node

import { cp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const releaseDir = path.join(root, "desktop-release");
const defaultDownloadBaseUrl = "http://127.0.0.1:41731/downloads/turbo";
const defaultUpdaterEndpoint = `${defaultDownloadBaseUrl}/latest.json`;
const installerName = "ai-cove-turbo-macos.dmg";
const updaterArchiveName = "ai-cove-turbo-macos-aarch64.app.tar.gz";
const updaterSignatureName = `${updaterArchiveName}.sig`;
const updaterPlatform = "darwin-aarch64";
const ciPlatforms = [
  {
    platform: "darwin-aarch64",
    installerSuffix: ".dmg",
    installerName,
    updaterSuffix: ".app.tar.gz",
    updaterName: updaterArchiveName,
  },
  {
    platform: "windows-x86_64",
    installerSuffix: ".exe",
    installerName: "ai-cove-turbo-windows.exe",
    updaterSuffix: ".exe",
    updaterName: "ai-cove-turbo-windows.exe",
  },
];

function run(command, args, env = process.env) {
  const result = spawnSync(command, args, {
    cwd: root,
    env,
    encoding: "utf8",
    stdio: "inherit",
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed with exit code ${result.status ?? "unknown"}`);
  }
}

async function listFiles(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const entryPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await listFiles(entryPath)));
    } else if (entry.isFile()) {
      files.push(entryPath);
    }
  }
  return files;
}

async function findBundleArtifact(bundleDir, suffix) {
  const matches = (await listFiles(bundleDir)).filter((filePath) => {
    const name = path.basename(filePath);
    return name.endsWith(suffix) && !name.endsWith(`${suffix}.sig`);
  });
  if (matches.length !== 1) {
    throw new Error(`expected one ${suffix} under ${bundleDir}, found ${matches.length}`);
  }
  return matches[0];
}

async function readVersion() {
  const packageJson = JSON.parse(await readFile(path.join(root, "package.json"), "utf8"));
  const cargo = await readFile(path.join(root, "src-tauri", "Cargo.toml"), "utf8");
  const tauriConfig = JSON.parse(await readFile(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"));
  const cargoVersion = cargo.match(/^version = "([^"]+)"$/mu)?.[1];
  if (!cargoVersion || packageJson.version !== cargoVersion || tauriConfig.version !== packageJson.version) {
    throw new Error("Turbo desktop version facts are not aligned");
  }
  return packageJson.version;
}

async function ensureSigningKey() {
  const keyPath = path.resolve(
    process.env.TURBO_LOCAL_UPDATER_KEY_PATH ?? path.join(root, "src-tauri", "target", "turbo-local-updater.key"),
  );
  const publicKeyPath = `${keyPath}.pub`;
  const keyExists = fs.existsSync(keyPath);
  const publicKeyExists = fs.existsSync(publicKeyPath);
  if (keyExists !== publicKeyExists) {
    throw new Error(`local updater key pair is incomplete: ${keyPath}`);
  }
  if (!keyExists) {
    await mkdir(path.dirname(keyPath), { recursive: true });
    const tauri = path.join(root, "node_modules", ".bin", process.platform === "win32" ? "tauri.cmd" : "tauri");
    run(tauri, ["signer", "generate", "--ci", "--write-keys", keyPath]);
  }
  const localPublicKey = (await readFile(publicKeyPath, "utf8")).trim();
  if (!localPublicKey && !process.env.TURBO_UPDATER_PUBLIC_KEY?.trim()) {
    throw new Error(`local updater public key is empty: ${publicKeyPath}`);
  }
  return { keyPath, publicKey: process.env.TURBO_UPDATER_PUBLIC_KEY?.trim() || localPublicKey };
}

function artifactUrl(baseUrl, archiveName, version) {
  return `${baseUrl.replace(/\/+$/u, "")}/${archiveName}?v=${encodeURIComponent(version)}`;
}

export function createLocalBuildInvocation({ privateKey, publicKey, env = process.env }) {
  const buildEnv = {
    ...env,
    APPLE_SIGNING_IDENTITY: env.APPLE_SIGNING_IDENTITY?.trim() || "-",
    CI: "true",
    TAURI_SIGNING_PRIVATE_KEY: privateKey,
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? "",
    TURBO_UPDATER_ENDPOINT: env.TURBO_UPDATER_ENDPOINT ?? defaultUpdaterEndpoint,
    TURBO_UPDATER_PUBLIC_KEY: env.TURBO_UPDATER_PUBLIC_KEY?.trim() || publicKey,
  };
  delete buildEnv.TAURI_CONFIG;
  delete buildEnv.TAURI_SIGNING_PRIVATE_KEY_PATH;
  return {
    args: [
      "build",
      "--ci",
      "--config",
      JSON.stringify({
        bundle: { createUpdaterArtifacts: true },
        plugins: { updater: { pubkey: publicKey } },
      }),
    ],
    env: buildEnv,
  };
}

function manifestFor({ version, baseUrl, signature }) {
  return {
    version,
    notes: `AI Cove Turbo ${version}`,
    pub_date: new Date().toISOString(),
    platforms: {
      [updaterPlatform]: {
        signature,
        url: artifactUrl(baseUrl, updaterArchiveName, version),
      },
    },
  };
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export async function validateDesktopRelease({ releaseDir: directory = releaseDir, requiredPlatforms = [updaterPlatform] }) {
  let manifest;
  try {
    manifest = JSON.parse(await readFile(path.join(directory, "latest.json"), "utf8"));
  } catch (error) {
    throw new Error(`invalid desktop updater manifest: ${error instanceof Error ? error.message : String(error)}`);
  }
  if (!isRecord(manifest) || typeof manifest.version !== "string" || !isRecord(manifest.platforms)) {
    throw new Error("desktop updater manifest is missing version or platforms");
  }
  for (const platform of requiredPlatforms) {
    if (!Object.prototype.hasOwnProperty.call(manifest.platforms, platform)) {
      throw new Error(`missing updater platform: ${platform}`);
    }
  }
  for (const [platform, entry] of Object.entries(manifest.platforms)) {
    if (!isRecord(entry) || typeof entry.signature !== "string" || typeof entry.url !== "string") {
      throw new Error(`incomplete updater platform entry: ${platform}`);
    }
    const archiveName = path.basename(new URL(entry.url).pathname);
    const signatureName = `${archiveName}.sig`;
    if (!archiveName) {
      throw new Error(`missing updater archive name for ${platform}`);
    }
    if (!fs.existsSync(path.join(directory, archiveName))) {
      throw new Error(`missing updater archive for ${platform}: ${archiveName}`);
    }
    if (!fs.existsSync(path.join(directory, signatureName))) {
      throw new Error(`missing updater signature file for ${platform}: ${signatureName}`);
    }
    const signature = (await readFile(path.join(directory, signatureName), "utf8")).trim();
    if (!signature || signature !== entry.signature.trim()) {
      throw new Error(`updater signature mismatch for ${platform}`);
    }
    const platformInstallerName = platform.startsWith("darwin-")
      ? installerName
      : platform.startsWith("windows-")
        ? "ai-cove-turbo-windows.exe"
        : null;
    if (platformInstallerName && !fs.existsSync(path.join(directory, platformInstallerName))) {
      throw new Error(`missing desktop installer for ${platform}: ${platformInstallerName}`);
    }
  }
  return { version: manifest.version, platforms: Object.keys(manifest.platforms).sort() };
}

export async function assembleDesktopRelease({
  version,
  releaseDir: directory = releaseDir,
  baseUrl = process.env.TURBO_DOWNLOAD_BASE_URL ?? defaultDownloadBaseUrl,
  installerPath,
  updaterArchivePath,
  updaterSignaturePath,
}) {
  const signature = (await readFile(updaterSignaturePath, "utf8")).trim();
  if (!signature) {
    throw new Error("updater signature is empty");
  }
  await rm(directory, { force: true, recursive: true });
  await mkdir(directory, { recursive: true });
  await cp(installerPath, path.join(directory, installerName));
  await cp(updaterArchivePath, path.join(directory, updaterArchiveName));
  await cp(updaterSignaturePath, path.join(directory, updaterSignatureName));
  await writeFile(
    path.join(directory, "latest.json"),
    `${JSON.stringify(manifestFor({ version, baseUrl, signature }), null, 2)}\n`,
    "utf8",
  );
  return validateDesktopRelease({ releaseDir: directory });
}

export async function assembleCiDesktopRelease({
  version,
  inputDir,
  releaseDir: directory,
  baseUrl = process.env.DOWNLOAD_BASE_URL ?? defaultDownloadBaseUrl,
}) {
  const files = await listFiles(inputDir);
  const platforms = {};
  await rm(directory, { force: true, recursive: true });
  await mkdir(directory, { recursive: true });

  for (const spec of ciPlatforms) {
    const installerPath = await findBundleArtifact(inputDir, spec.installerSuffix);
    const updaterArchivePath = await findBundleArtifact(inputDir, spec.updaterSuffix);
    const updaterSignaturePath = `${updaterArchivePath}.sig`;
    if (!files.includes(updaterSignaturePath)) {
      throw new Error(`missing generated updater signature: ${updaterSignaturePath}`);
    }
    const signature = (await readFile(updaterSignaturePath, "utf8")).trim();
    if (!signature) {
      throw new Error(`updater signature is empty for ${spec.platform}`);
    }
    await cp(installerPath, path.join(directory, spec.installerName));
    if (spec.updaterName !== spec.installerName) {
      await cp(updaterArchivePath, path.join(directory, spec.updaterName));
    }
    await cp(updaterSignaturePath, path.join(directory, `${spec.updaterName}.sig`));
    platforms[spec.platform] = {
      signature,
      url: artifactUrl(baseUrl, spec.updaterName, version),
    };
  }

  await writeFile(
    path.join(directory, "latest.json"),
    `${JSON.stringify({ version, notes: `AI Cove Turbo ${version}`, pub_date: new Date().toISOString(), platforms }, null, 2)}\n`,
    "utf8",
  );
  return validateDesktopRelease({
    releaseDir: directory,
    requiredPlatforms: ciPlatforms.map(({ platform }) => platform),
  });
}

export async function buildLocalRelease() {
  if (process.platform !== "darwin" || process.arch !== "arm64") {
    throw new Error("local Turbo release requires an arm64 macOS host");
  }
  const version = await readVersion();
  const signing = await ensureSigningKey();
  const privateKey = (await readFile(signing.keyPath, "utf8")).trim();
  const invocation = createLocalBuildInvocation({ privateKey, publicKey: signing.publicKey });
  const tauri = path.join(root, "node_modules", ".bin", "tauri");
  run(tauri, invocation.args, invocation.env);
  const bundleDir = path.join(root, "src-tauri", "target", "release", "bundle");
  const installerPath = await findBundleArtifact(path.join(bundleDir, "dmg"), ".dmg");
  const updaterArchivePath = await findBundleArtifact(path.join(bundleDir, "macos"), ".app.tar.gz");
  const updaterSignaturePath = `${updaterArchivePath}.sig`;
  if (!fs.existsSync(updaterSignaturePath)) {
    throw new Error(`missing generated updater signature: ${updaterSignaturePath}`);
  }
  const appPath = path.join(bundleDir, "macos", "AI Cove Turbo.app");
  if (fs.existsSync(appPath)) {
    run("codesign", ["--verify", "--deep", "--strict", "--verbose=2", appPath]);
  }
  const result = await assembleDesktopRelease({
    version,
    installerPath,
    updaterArchivePath,
    updaterSignaturePath,
  });
  process.stdout.write(`[desktop:release:local] ${result.version} -> ${releaseDir}\n`);
  return result;
}

export async function main(args = process.argv.slice(2)) {
  if (args[0] === "assemble-ci") {
    const version = await readVersion();
    const directory = path.resolve(root, args[2] ?? "desktop-release");
    const result = await assembleCiDesktopRelease({
      version,
      inputDir: path.resolve(root, args[1] ?? "release-inputs"),
      releaseDir: directory,
    });
    process.stdout.write(`[desktop:assemble-ci] ${result.version} -> ${directory}\n`);
    return result;
  }
  if (args[0] === "validate") {
    const result = await validateDesktopRelease({ releaseDir: args[1] ?? releaseDir });
    process.stdout.write(`[desktop:validate-release] ${result.version} valid for ${result.platforms.join(", ")}\n`);
    return result;
  }
  return buildLocalRelease();
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  });
}
