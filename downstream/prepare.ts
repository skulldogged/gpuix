/**
 * Prepare this checkout for a native build: fetch the pinned Zed submodule and
 * apply the GPUI patches in `downstream/zed` to it.
 *
 * The patches form a stack: a later one can change lines an earlier one needs
 * as context, so once the last applied patch is found, everything before it
 * counts as applied. Running this again is a no-op.
 *
 *   bun vendor/gpuix/downstream/prepare.ts
 */
import { readdirSync } from "node:fs";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const zed = join(root, "zed");
const patches = join(import.meta.dir, "zed");

/** Applied in this order. Keep it in sync with the folder. */
const STACK = [
  "gpui-windows-shutdown.patch",
  "gpui-smooth-scroll.patch",
  "gpui-even-line-height.patch",
  "gpui-list-follow.patch",
  "gpui-nested-scroll.patch",
  "gpui-frame-work.patch",
];

function run(args: string[], cwd: string) {
  const result = Bun.spawnSync(args, { cwd, stdout: "inherit", stderr: "inherit" });
  if (result.exitCode !== 0) throw new Error(`${args.join(" ")} failed in ${cwd}`);
}

const unlisted = readdirSync(patches).filter((name) => name.endsWith(".patch") && !STACK.includes(name));
if (unlisted.length) throw new Error(`Patches missing from the stack: ${unlisted.join(", ")}`);

run(["git", "submodule", "update", "--init", "--depth", "1", "zed"], root);

const applied = (name: string) =>
  Bun.spawnSync(["git", "apply", "--reverse", "--check", join(patches, name)], { cwd: zed }).exitCode === 0;
let next = STACK.length;
while (next > 0 && !applied(STACK[next - 1])) next--;
for (const name of STACK.slice(next)) {
  run(["git", "apply", "--check", join(patches, name)], zed);
  run(["git", "apply", join(patches, name)], zed);
}
console.log(`GPUI patches applied (${STACK.length}).`);
