// Generates the Tauri v2 updater manifest (latest.json) from the signed NSIS
// bundle produced by `npm run tauri build`.
//
// Why this exists: a plain local `tauri build` emits the installers and their
// .sig files but NOT latest.json (tauri-action generates it in CI as a
// post-build step). We publish via GitHub Releases, so we build it here.
//
// PRIVATE REPO NOTE: GitHub's browser-facing download URLs
// (/releases/latest/download/... and /releases/download/<tag>/...) ignore the
// Authorization header for private repositories and return 404 even with a
// valid token. The only authenticated path is the GitHub API:
//   - the app fetches the manifest from the repo CONTENTS API
//     (https://api.github.com/repos/<repo>/contents/latest.json)
//   - the app downloads the installer from the RELEASE-ASSETS API
//     (https://api.github.com/repos/<repo>/releases/assets/<asset_id>)
// So with GITHUB_TOKEN set, this script resolves the setup exe's asset id from
// the release and writes the API url into the manifest. Without a token it
// falls back to the browser URL (fine for public repos / forks).
//
// Usage (from the project root):
//   $env:GITHUB_TOKEN="github_pat_..."      # read access to the repo
//   node scripts/generate-update-manifest.js [owner/repo]
//
// Requires: a SIGNED build (NSIS exe + .sig in src-tauri/target/release/bundle/nsis/)
//           and — for the API url — the release v<version> must already exist
//           with the setup exe uploaded (run AFTER uploading the installers).
// Writes:   src-tauri/target/release/latest.json  (upload to the release: archival)
//           latest.json (repo root)               (COMMIT + PUSH — the app fetches this)
//
// The GitHub release tag MUST be v<version> and published (not a draft).

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execSync } from "node:child_process";

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
const conf = JSON.parse(
  fs.readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"),
);
const version = conf.version;

function getOriginRepo() {
  try {
    const url = execSync("git remote get-url origin", { encoding: "utf8" }).trim();
    const match = url.match(/github\.com[:/]([^/]+)\/([^/.]+)(?:\.git)?$/);
    if (match) return `${match[1]}/${match[2]}`;
  } catch {}
  return "OutpostCoding/Outpost";
}

const repo = process.argv[2] || getOriginRepo();
const token = process.env.GITHUB_TOKEN;

const nsisDir = path.join(root, "src-tauri", "target", "release", "bundle", "nsis");
const exe = fs.readdirSync(nsisDir).find(
  (f) => f.includes(version) && f.endsWith("_x64-setup.exe"),
);
if (!exe) {
  console.error(`No NSIS setup exe matching v${version} in ${nsisDir}`);
  process.exit(1);
}
const sigPath = path.join(nsisDir, exe + ".sig");
if (!fs.existsSync(sigPath)) {
  console.error(`Missing signature: ${exe}.sig (was the build signed?)`);
  process.exit(1);
}

// The .sig is base64 of the minisign armor; its trusted comment carries the
// signed filename and the signing timestamp. Use both: verify + pub_date.
const signature = fs.readFileSync(sigPath, "utf8").trim();
const armored = Buffer.from(signature, "base64").toString("utf8");
const m = armored.match(/timestamp:(\d+)\tfile:(\S+)/);
if (!m) {
  console.error("Could not parse the trusted comment from the .sig file");
  process.exit(1);
}
if (m[2] !== exe) {
  console.error(`Signature is for ${m[2]}, expected ${exe} — refusing to proceed`);
  process.exit(1);
}
const pubDate = new Date(Number(m[1]) * 1000).toISOString().replace(".000Z", "Z");

// Resolve the download url. Private repo → the release-assets API (needs
// GITHUB_TOKEN and the release v<version> to exist with the exe uploaded).
let url;
if (token) {
  const tag = `v${version}`;
  const res = await fetch(
    `https://api.github.com/repos/${repo}/releases/tags/${tag}`,
    {
      headers: {
        Authorization: `Bearer ${token}`,
        "User-Agent": "outpost-update-manifest",
        Accept: "application/vnd.github+json",
      },
    },
  );
  if (!res.ok) {
    console.error(
      `GitHub API ${res.status} for release ${tag}: ${(await res.text()).slice(0, 200)}`,
    );
    console.error(
      "Is the release created and the setup exe uploaded? Is GITHUB_TOKEN valid?",
    );
    process.exit(1);
  }
  const release = await res.json();
  const asset = (release.assets ?? []).find((a) => a.name === exe);
  if (!asset) {
    console.error(`Asset ${exe} not found on release ${tag}`);
    process.exit(1);
  }
  url = `https://api.github.com/repos/${repo}/releases/assets/${asset.id}`;
  console.log(`Resolved asset id ${asset.id} for ${exe} (${asset.size} bytes)`);
} else {
  url = `https://github.com/${repo}/releases/download/v${version}/${exe}`;
  console.warn(
    "No GITHUB_TOKEN set — using the browser download URL. " +
      "This will NOT work for a private repo; set GITHUB_TOKEN for the API url.",
  );
}

const manifest = {
  version,
  notes: `Outpost ${version}`,
  pub_date: pubDate,
  platforms: {
    "windows-x86_64": {
      signature,
      url,
    },
  },
};

const out = path.join(root, "src-tauri", "target", "release", "latest.json");
fs.writeFileSync(out, JSON.stringify(manifest, null, 2) + "\n");
console.log(`Wrote ${out}`);
if (token) {
  // The app fetches the manifest from the repo's main branch (contents API),
  // so the committed copy is the one that matters — push it.
  const repoOut = path.join(root, "latest.json");
  fs.writeFileSync(repoOut, JSON.stringify(manifest, null, 2) + "\n");
  console.log(
    `Wrote ${repoOut}  → next: git add latest.json && git commit -m "update manifest" && git push`,
  );
}
console.log(`  version: ${version}`);
console.log(`  pub_date: ${pubDate}`);
console.log(`  url:     ${url}`);
