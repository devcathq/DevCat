// Screenshots of DevCat's demo mode (made-up data) for the README and the Remotion video.
// Needs: `pnpm dev:demo` (port 1426) + `cargo run --example devbridge` + `./setup-demo.sh`.
// Usage: node capture.mjs [scene…]   → public/shots/<scene>.png
import { chromium } from "playwright-core";
import { mkdirSync } from "node:fs";

const APP = process.env.DEMO_URL ?? "http://localhost:1426/";
const ROOT = process.env.DEMO_ROOT ?? "/tmp/devcat-demo";
const OUT = new URL("./public/shots/", import.meta.url).pathname;
mkdirSync(OUT, { recursive: true });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Each scene starts from a fresh page; `d` is window.__demo (src/dev/demoHooks.ts). */
const scenes = {
  welcome: async () => {},

  ai: async (page) => {
    await page.evaluate((root) => {
      const d = window.__demo;
      d.useUi.getState().setSidebar("claude");
      d.openClaudeWorkspace(`${root}/storefront`);
    }, ROOT);
    await sleep(2500);
  },

  diff: async (page) => {
    await page.evaluate((root) => {
      const d = window.__demo;
      d.useUi.getState().setSidebar("claude");
      d.openClaudeWorkspace(`${root}/storefront`, { view: "diff" });
    }, ROOT);
    await sleep(2500);
  },

  table: async (page) => {
    await page.evaluate(() => {
      const d = window.__demo;
      d.useUi.getState().setSidebar("connections");
      d.openTab({ type: "table-data", connId: "c-pg", table: { database: "shop", schema: "public", name: "orders" } }, "orders");
    });
    await sleep(1200);
    await expandTree(page, ["shop-db", "shop", "public", "Tables"]);
    const filter = page.locator('input[placeholder="contains…"]').nth(3);
    if (await filter.count()) {
      await filter.fill("delivered");
      await filter.press("Enter");
    }
    await sleep(800);
  },

  query: async (page) => {
    await page.evaluate(() => {
      const d = window.__demo;
      d.useUi.getState().setSidebar("connections");
      d.openTab({
        type: "query", connId: "c-pg", database: "shop", schema: "public",
        initialSql: "-- Revenue by country, last 30 days\nSELECT country,\n       count(*)              AS orders,\n       sum(total)            AS revenue,\n       round(avg(total), 2)  AS avg_order\nFROM orders\nWHERE paid AND created_at > now() - interval '30 days'\nGROUP BY country\nORDER BY revenue DESC;\n",
      }, "Query · shop-db");
    });
    await sleep(1500);
    await expandTree(page, ["shop-db", "shop", "public", "Tables"]);
    await page.locator(".monaco-editor").first().click();
    await page.keyboard.press("Meta+Enter");
    await sleep(1200);
  },

  git: async (page) => {
    await page.evaluate((root) => {
      const d = window.__demo;
      d.useUi.getState().setSidebar("git");
      d.openGitRepo(`${root}/storefront`);
    }, ROOT);
    await sleep(2500);
    await page.getByText("Merge branch 'fix/checkout-rounding'", { exact: true }).first().click();
    await sleep(1500);
  },

  network: async (page) => {
    await page.evaluate(() => {
      const d = window.__demo;
      d.useUi.getState().setSidebar("network");
      d.openTab({ type: "network", profileId: "n-k8s" }, "shop-db (k8s)", "network:n-k8s");
    });
    await sleep(2000);
  },

  k8s: async (page) => {
    await page.evaluate(() => {
      const d = window.__demo;
      d.useUi.getState().setSidebar("connections");
      d.openTab({ type: "k8s", connId: "c-k8s" }, "prod-cluster", "k8s:c-k8s");
    });
    await sleep(2500);
  },

  // DevCat Agent, opened from the shop-db connection (made-up chat: src/dev/demo.ts)
  agent: async (page) => {
    await page.evaluate((root) => {
      const d = window.__demo;
      d.useUi.getState().setSidebar("claude");
      d.openTab({ type: "claude", cwd: `${root}/storefront`, connId: "c-pg", providerId: "p-agent", sessionId: null, view: "chat" }, "shop-db · Agent", "agent-conn:c-pg");
    }, ROOT);
    // the chat (and its Markdown) loads in parts: wait for the answer to be there
    await page.getByText("Three customers were refunded", { exact: false }).first().waitFor({ timeout: 15000 });
    await sleep(1500);
  },

  bots: async (page) => {
    await page.evaluate(() => window.__demo.openIntegrations());
    await sleep(1500);
  },

  memory: async (page) => {
    await page.evaluate(() => window.__demo.openMemory());
    await sleep(1500);
  },

  notes: async (page) => {
    await page.evaluate(() => {
      const d = window.__demo;
      d.useUi.getState().setSidebar("notes");
      const n = d.useNotes.getState().notes.find((x) => x.id === "note-1");
      if (n) d.openNote(n);
    });
    await sleep(1500);
    await page.getByRole("button", { name: "Preview" }).first().click();
    await sleep(1000);
  },
};

/** Click tree rows by label, in order (each one expands the next level). */
async function expandTree(page, labels) {
  for (const l of labels) {
    const row = page.getByText(l, { exact: true }).first();
    if (!(await row.count())) continue;
    // the expand arrow sits just left of the label
    const box = await row.boundingBox();
    if (!box) continue;
    await page.mouse.click(box.x - 38, box.y + box.height / 2);
    await sleep(400);
  }
}

/** Scenes that open their own URL instead of the main window. */
const urls = {
  ghost: () => `${APP}?tab=${encodeURIComponent(JSON.stringify({ type: "claude", cwd: "", providerId: "p-agent", sessionId: null, view: "chat", ghost: true, title: "Ghost chat" }))}`,
};

const wanted = process.argv.slice(2).length ? process.argv.slice(2) : [...Object.keys(scenes), ...Object.keys(urls)];
// No WebGL: headless Chrome draws xterm's GPU renderer blown up at 2x, its DOM renderer is right.
const browser = await chromium.launch({ channel: "chrome", headless: true, args: ["--disable-webgl", "--disable-webgl2"] });
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2, colorScheme: "dark" });
for (const name of wanted) {
  const page = await ctx.newPage();
  page.on("pageerror", (e) => console.warn(`[${name}] page error: ${e.message}`));
  if (urls[name]) {
    await page.goto(urls[name]());
    await sleep(2500);
    await page.mouse.move(1439, 899);
    await page.screenshot({ path: `${OUT}${name}.png` });
    console.log(`✓ ${name}`);
    await page.close();
    continue;
  }
  await page.goto(APP);
  await page.waitForFunction(() => !!window.__demo);
  await page.evaluate((root) => {
    localStorage.setItem("devcat.sidebar.collapsed", "0");
    window.__demo.useWorkspace.getState().setCwd(`${root}/storefront`);
  }, ROOT);
  await sleep(800);
  await scenes[name](page);
  await page.mouse.move(1439, 899);
  await page.screenshot({ path: `${OUT}${name}.png` });
  console.log(`✓ ${name}`);
  await page.close();
}
await browser.close();
