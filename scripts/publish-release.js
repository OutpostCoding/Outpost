// scripts/publish-release.js
// Automates the entire release publishing workflow:
// 1. Verifies signed installers exist for current version in tauri.conf.json
// 2. Tags current git commit with v<version> and pushes tag to GitHub
// 3. Creates or finds the GitHub Release for v<version>
// 4. Uploads all 4 installer files (.exe, .exe.sig, .msi, .msi.sig)
// 5. Generates latest.json with the exact GitHub API asset ID
// 6. Uploads latest.json to the release assets
// 7. Commits latest.json to the main branch and pushes
// 8. Syncs the git tag to the final commit

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execSync } from "node:child_process";

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
const conf = JSON.parse(
  fs.readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"),
);
const version = conf.version;
const repo = process.argv[2] || "OutpostCoding/Outpost";
const token = process.env.GITHUB_TOKEN;

if (!token) {
  console.error("❌ ERROR: GITHUB_TOKEN environment variable is not set!");
  console.error("Please set it in PowerShell first:");
  console.error('  $env:GITHUB_TOKEN = "github_pat_..."');
  process.exit(1);
}

console.log(`\n🚀 Publishing Outpost v${version} to ${repo}...\n`);

// 1. Check for signed bundles
const nsisDir = path.join(root, "src-tauri", "target", "release", "bundle", "nsis");
const msiDir = path.join(root, "src-tauri", "target", "release", "bundle", "msi");

if (!fs.existsSync(nsisDir) || !fs.existsSync(msiDir)) {
  console.error(`❌ Build directories not found. Did you run the signed build?`);
  process.exit(1);
}

const exe = fs.readdirSync(nsisDir).find(
  (f) => f.includes(version) && f.endsWith("_x64-setup.exe"),
);
const msi = fs.readdirSync(msiDir).find(
  (f) => f.includes(version) && f.endsWith(".msi"),
);

if (!exe || !fs.existsSync(path.join(nsisDir, exe + ".sig"))) {
  console.error(`❌ Missing NSIS setup exe or .sig for v${version} in ${nsisDir}`);
  process.exit(1);
}

if (!msi || !fs.existsSync(path.join(msiDir, msi + ".sig"))) {
  console.error(`❌ Missing MSI installer or .sig for v${version} in ${msiDir}`);
  process.exit(1);
}

console.log(`✓ Found signed installers:`);
console.log(`  - ${exe} (+ .sig)`);
console.log(`  - ${msi} (+ .sig)`);

// 2. Parse signature timestamp
const sigPath = path.join(nsisDir, exe + ".sig");
const signature = fs.readFileSync(sigPath, "utf8").trim();
const armored = Buffer.from(signature, "base64").toString("utf8");
const m = armored.match(/timestamp:(\d+)\tfile:(\S+)/);
if (!m) {
  console.error("❌ Could not parse trusted comment from .sig file");
  process.exit(1);
}
const pubDate = new Date(Number(m[1]) * 1000).toISOString().replace(".000Z", "Z");

// 3. Git tag creation & push
const tag = `v${version}`;
console.log(`\n🏷️  Syncing git tag ${tag}...`);
try {
  execSync(`git tag -f ${tag}`, { stdio: "pipe" });
  execSync(`git push origin ${tag} --force`, { stdio: "inherit" });
  console.log(`✓ Tag ${tag} pushed to origin`);
} catch (e) {
  console.error(`❌ Failed to push git tag:`, e.message);
  process.exit(1);
}

// 4. Create or fetch GitHub Release
console.log(`\n📦 Fetching or creating GitHub release ${tag}...`);
const headers = {
  Authorization: `Bearer ${token}`,
  "User-Agent": "outpost-release-publisher",
  Accept: "application/vnd.github+json",
};

let releaseRes = await fetch(`https://api.github.com/repos/${repo}/releases/tags/${tag}`, { headers });
let release;

if (releaseRes.status === 404) {
  console.log(`  Creating new release ${tag}...`);
  const createRes = await fetch(`https://api.github.com/repos/${repo}/releases`, {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({
      tag_name: tag,
      name: `Outpost v${version}`,
      body: `Release v${version}`,
      draft: false,
      prerelease: false,
    }),
  });
  if (!createRes.ok) {
    console.error(`❌ Failed to create release:`, await createRes.text());
    process.exit(1);
  }
  release = await createRes.json();
  console.log(`✓ Created release: ${release.html_url}`);
} else if (releaseRes.ok) {
  release = await releaseRes.json();
  console.log(`✓ Found existing release: ${release.html_url}`);
} else {
  console.error(`❌ Error querying release:`, await releaseRes.text());
  process.exit(1);
}

// Helper to upload an asset to the release
async function uploadAsset(filePath, fileName, contentType = "application/octet-stream") {
  const existing = (release.assets ?? []).find((a) => a.name === fileName);
  if (existing) {
    console.log(`  Deleting existing asset ${fileName} (${existing.id})...`);
    await fetch(`https://api.github.com/repos/${repo}/releases/assets/${existing.id}`, {
      method: "DELETE",
      headers,
    });
  }

  const fileData = fs.readFileSync(filePath);
  console.log(`  Uploading ${fileName} (${(fileData.length / 1024 / 1024).toFixed(2)} MB)...`);

  const uploadUrl = `https://uploads.github.com/repos/${repo}/releases/${release.id}/assets?name=${encodeURIComponent(fileName)}`;
  const uploadRes = await fetch(uploadUrl, {
    method: "POST",
    headers: {
      ...headers,
      "Content-Type": contentType,
      "Content-Length": fileData.length,
    },
    body: fileData,
  });

  if (!uploadRes.ok) {
    console.error(`❌ Failed to upload ${fileName}:`, await uploadRes.text());
    process.exit(1);
  }

  const uploaded = await uploadRes.json();
  console.log(`  ✓ Uploaded ${fileName} (ID: ${uploaded.id})`);
  return uploaded;
}

// 5. Upload installers and sigs
console.log(`\n📤 Uploading installer assets...`);
const uploadedExe = await uploadAsset(path.join(nsisDir, exe), exe);
await uploadAsset(path.join(nsisDir, exe + ".sig"), exe + ".sig", "text/plain");
await uploadAsset(path.join(msiDir, msi), msi);
await uploadAsset(path.join(msiDir, msi + ".sig"), msi + ".sig", "text/plain");

// 6. Generate latest.json
console.log(`\n📝 Generating update manifest (latest.json)...`);
// For public releases, use the direct GitHub releases download URL (no tokens or rate limits needed).
// If PRIVATE_REPO=1 is set, fall back to the authenticated release assets API URL.
const downloadUrl = process.env.PRIVATE_REPO
  ? `https://api.github.com/repos/${repo}/releases/assets/${uploadedExe.id}`
  : `https://github.com/${repo}/releases/download/v${version}/${exe}`;
const manifest = {
  version,
  notes: `Outpost ${version}`,
  pub_date: pubDate,
  platforms: {
    "windows-x86_64": {
      signature,
      url: downloadUrl,
    },
  },
};

const rootManifestPath = path.join(root, "latest.json");
const targetManifestPath = path.join(root, "src-tauri", "target", "release", "latest.json");

fs.writeFileSync(rootManifestPath, JSON.stringify(manifest, null, 2) + "\n");
fs.writeFileSync(targetManifestPath, JSON.stringify(manifest, null, 2) + "\n");
console.log(`✓ Wrote ${rootManifestPath}`);
console.log(`✓ Wrote ${targetManifestPath}`);

// 7. Upload latest.json to GitHub release as well
console.log(`\n📤 Uploading latest.json to release assets...`);
await uploadAsset(targetManifestPath, "latest.json", "application/json");

// 8. Commit and push latest.json to main
console.log(`\n🔄 Committing and pushing latest.json to main...`);
try {
  execSync("git add latest.json", { stdio: "pipe" });
  try {
    execSync(`git commit -m "chore: update release manifest for v${version}"`, { stdio: "pipe" });
    console.log(`✓ Committed latest.json`);
  } catch {
    console.log(`  (latest.json already up to date in git)`);
  }
  execSync("git push origin main", { stdio: "inherit" });
  console.log(`✓ Pushed to origin/main`);

  // Move the tag to include the latest.json commit so git history is 100% clean
  execSync(`git tag -f ${tag}`, { stdio: "pipe" });
  execSync(`git push origin ${tag} --force`, { stdio: "pipe" });
  console.log(`✓ Tag ${tag} synced to manifest commit`);
} catch (e) {
  console.error(`❌ Failed git commit/push:`, e.message);
  process.exit(1);
}

console.log(`\n🎉 Outpost v${version} has been published successfully!`);
console.log(`🔗 Release: ${release.html_url}`);
console.log(`✨ Installed apps will now discover this update automatically!\n`);
