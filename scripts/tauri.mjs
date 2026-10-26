// `pnpm tauri …`: the Tauri CLI, with src-tauri/tauri.dev.conf.json added to `tauri dev` so the dev
// build shows the DEV icon and runs as "DevCat Dev" next to an installed DevCat (APP_ID in lib.rs).
// `pnpm tauri dev --prod-data`: on the installed DevCat's data instead of its own (src-tauri/src/devdata.rs),
// only while that DevCat is closed.
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { runningDevcats } from "./devcat-running.mjs";

const args = process.argv.slice(2);
const env = { ...process.env };
if (args[0] === "dev") {
  const i = args.indexOf("--prod-data");
  if (i >= 0) {
    args.splice(i, 1);
    const { installed } = runningDevcats();
    if (installed.length) {
      console.error(`DevCat is running (${installed.join(", ")}).\nQuit it first (⌘Q / File → Quit, and \`devcat stop\` for a \`devcat serve\`): two DevCats on the same data undo each other's changes.`);
      process.exit(1);
    }
    env.DEVCAT_PROD_DATA = "1";
    console.log("DevCat Dev on the installed DevCat's data and saved passwords. Don't open the installed DevCat until you quit this one.");
  }
  args.splice(1, 0, "--config", fileURLToPath(new URL("../src-tauri/tauri.dev.conf.json", import.meta.url)));
}
const cli = fileURLToPath(new URL("../node_modules/@tauri-apps/cli/tauri.js", import.meta.url));
const r = spawnSync(process.execPath, [cli, ...args], { stdio: "inherit", env });
process.exit(r.status ?? 1);
