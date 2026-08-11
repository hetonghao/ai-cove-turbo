import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const version = "0.1.0-beta.1";

test("本地构建把 updater 配置和私钥内容交给 Tauri CLI", async () => {
  const { createLocalBuildInvocation } = await import("../scripts/desktop-release.mjs");

  const invocation = createLocalBuildInvocation({
    privateKey: "private-key-content",
    publicKey: "base64-public-key-content",
    env: {
      APPLE_SIGNING_IDENTITY: "",
      TAURI_CONFIG: "stale-env-config",
      TAURI_SIGNING_PRIVATE_KEY_PATH: "/tmp/stale-key-path",
      TAURI_UPDATER_ENDPOINT: "ignored-variable",
      TURBO_UPDATER_ENDPOINT: "http://127.0.0.1:41731/downloads/turbo/latest.json",
    },
  });

  assert.deepEqual(invocation.args.slice(0, 2), ["build", "--ci"]);
  assert.equal(invocation.args[2], "--config");
  assert.deepEqual(JSON.parse(invocation.args[3]), {
    bundle: { createUpdaterArtifacts: true },
    plugins: { updater: { pubkey: "base64-public-key-content" } },
  });
  assert.equal(invocation.env.TAURI_SIGNING_PRIVATE_KEY, "private-key-content");
  assert.equal(invocation.env.TAURI_SIGNING_PRIVATE_KEY_PATH, undefined);
  assert.equal(invocation.env.TAURI_CONFIG, undefined);
  assert.equal(invocation.env.CI, "true");
  assert.equal(invocation.env.TURBO_UPDATER_ENDPOINT, "http://127.0.0.1:41731/downloads/turbo/latest.json");
});

async function createFakeBuild() {
  const root = await mkdtemp(path.join(os.tmpdir(), "turbo-release-test-"));
  const installer = path.join(root, "generated.dmg");
  const archive = path.join(root, "generated.app.tar.gz");
  const signature = path.join(root, "generated.app.tar.gz.sig");
  await writeFile(installer, "dmg bytes");
  await writeFile(archive, "updater bytes");
  await writeFile(signature, "signed updater payload\n");
  return { root, installer, archive, signature };
}

test("本地 assembly 生成 darwin updater manifest 并保留可下载的稳定文件名", async () => {
  const { assembleDesktopRelease } = await import("../scripts/desktop-release.mjs");
  const fakeBuild = await createFakeBuild();
  const releaseDir = path.join(fakeBuild.root, "desktop-release");

  try {
    await assembleDesktopRelease({
      version,
      releaseDir,
      installerPath: fakeBuild.installer,
      updaterArchivePath: fakeBuild.archive,
      updaterSignaturePath: fakeBuild.signature,
    });

    const manifest = JSON.parse(await readFile(path.join(releaseDir, "latest.json"), "utf8"));
    const updater = manifest.platforms["darwin-aarch64"];
    assert.equal(manifest.version, version);
    assert.equal(
      updater.url,
      "http://127.0.0.1:41731/downloads/turbo/ai-cove-turbo-macos-aarch64.app.tar.gz?v=0.1.0-beta.1",
    );
    assert.equal(new URL(updater.url).pathname.split("/").pop(), "ai-cove-turbo-macos-aarch64.app.tar.gz");
    assert.equal(updater.signature, "signed updater payload");
    assert.equal(await readFile(path.join(releaseDir, "ai-cove-turbo-macos-aarch64.app.tar.gz.sig"), "utf8"), "signed updater payload\n");
    assert.equal(await readFile(path.join(releaseDir, "ai-cove-turbo-macos.dmg"), "utf8"), "dmg bytes");
  } finally {
    await rm(fakeBuild.root, { recursive: true, force: true });
  }
});

test("CI assembly 从双平台 updater 实物生成合并 manifest", async () => {
  const { assembleCiDesktopRelease } = await import("../scripts/desktop-release.mjs");
  const testRoot = await mkdtemp(path.join(os.tmpdir(), "turbo-ci-release-test-"));
  const inputDir = path.join(testRoot, "release-inputs");
  const releaseDir = path.join(testRoot, "desktop-release");
  const darwinDir = path.join(inputDir, "darwin");
  const windowsDir = path.join(inputDir, "windows");

  try {
    await mkdir(darwinDir, { recursive: true });
    await mkdir(windowsDir, { recursive: true });
    await writeFile(path.join(darwinDir, "AI Cove Turbo_0.1.0-beta.1_aarch64.dmg"), "dmg");
    await writeFile(path.join(darwinDir, "AI Cove Turbo.app.tar.gz"), "mac updater");
    await writeFile(path.join(darwinDir, "AI Cove Turbo.app.tar.gz.sig"), "mac signature\n");
    await writeFile(path.join(windowsDir, "AI Cove Turbo_0.1.0-beta.1_x64-setup.exe"), "exe");
    await writeFile(
      path.join(windowsDir, "AI Cove Turbo_0.1.0-beta.1_x64-setup.exe.zip"),
      "windows updater",
    );
    await writeFile(
      path.join(windowsDir, "AI Cove Turbo_0.1.0-beta.1_x64-setup.exe.zip.sig"),
      "windows signature\n",
    );

    await assembleCiDesktopRelease({
      version,
      inputDir,
      releaseDir,
      baseUrl: "https://ai-cove.com/downloads/turbo",
    });

    const manifest = JSON.parse(await readFile(path.join(releaseDir, "latest.json"), "utf8"));
    assert.equal(manifest.version, version);
    assert.equal(manifest.platforms["darwin-aarch64"].signature, "mac signature");
    assert.equal(
      manifest.platforms["darwin-aarch64"].url,
      "https://ai-cove.com/downloads/turbo/ai-cove-turbo-macos-aarch64.app.tar.gz?v=0.1.0-beta.1",
    );
    assert.equal(manifest.platforms["windows-x86_64"].signature, "windows signature");
    assert.equal(
      manifest.platforms["windows-x86_64"].url,
      "https://ai-cove.com/downloads/turbo/ai-cove-turbo-windows-x86_64.exe.zip?v=0.1.0-beta.1",
    );
    assert.equal(await readFile(path.join(releaseDir, "ai-cove-turbo-windows.exe"), "utf8"), "exe");
  } finally {
    await rm(testRoot, { recursive: true, force: true });
  }
});

test("manifest signature 与实际 sig 不一致时 validation 失败", async () => {
  const { assembleDesktopRelease, validateDesktopRelease } = await import("../scripts/desktop-release.mjs");
  const fakeBuild = await createFakeBuild();
  const releaseDir = path.join(fakeBuild.root, "desktop-release");

  try {
    await assembleDesktopRelease({
      version,
      releaseDir,
      installerPath: fakeBuild.installer,
      updaterArchivePath: fakeBuild.archive,
      updaterSignaturePath: fakeBuild.signature,
    });
    await writeFile(path.join(releaseDir, "ai-cove-turbo-macos-aarch64.app.tar.gz.sig"), "tampered\n");
    await assert.rejects(
      validateDesktopRelease({ releaseDir }),
      /updater signature mismatch for darwin-aarch64/u,
    );
  } finally {
    await rm(fakeBuild.root, { recursive: true, force: true });
  }
});

test("manifest 引用的 updater archive 缺失时 validation 失败", async () => {
  const { assembleDesktopRelease, validateDesktopRelease } = await import("../scripts/desktop-release.mjs");
  const fakeBuild = await createFakeBuild();
  const releaseDir = path.join(fakeBuild.root, "desktop-release");

  try {
    await assembleDesktopRelease({
      version,
      releaseDir,
      installerPath: fakeBuild.installer,
      updaterArchivePath: fakeBuild.archive,
      updaterSignaturePath: fakeBuild.signature,
    });
    await rm(path.join(releaseDir, "ai-cove-turbo-macos-aarch64.app.tar.gz"));
    await assert.rejects(
      validateDesktopRelease({ releaseDir }),
      /missing updater archive for darwin-aarch64/u,
    );
  } finally {
    await rm(fakeBuild.root, { recursive: true, force: true });
  }
});
