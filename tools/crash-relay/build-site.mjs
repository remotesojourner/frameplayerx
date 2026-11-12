import { cp, mkdir, realpath, rm } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = await realpath(fileURLToPath(new URL("../../", import.meta.url)));
const dist = join(root, "dist");
const output = join(dist, "website");

// Keep website files separate from the release archives also built in dist/.
await mkdir(dist, { recursive: true });
if (await realpath(dist) !== dist) throw new Error("dist/ must be inside this checkout");
const existingOutput = await realpath(output).catch((error) => {
  if (error.code !== "ENOENT") throw error;
});
if (existingOutput && existingOutput !== output) throw new Error("dist/website/ must be inside dist/");
await rm(output, { recursive: true, force: true });
await cp(join(root, "site"), output, { recursive: true });
await mkdir(join(output, "assets"), { recursive: true });
await cp(join(root, "assets", "logo.svg"), join(output, "assets", "logo.svg"));
