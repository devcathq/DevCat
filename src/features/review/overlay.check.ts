// `pnpm check:review` (second part): when a DevCat overlay covers the review's page area.
// Runs under plain Node (type stripping): no node:* imports, no @types/node.
import { coveringRect } from "./overlay.ts";

function eq(actual: unknown, expected: unknown, what: string) {
  if (actual !== expected) throw new Error(`${what}\n  expected ${String(expected)}\n  actual   ${String(actual)}`);
}

const rect = (left: number, top: number, width: number, height: number) =>
  ({ left, top, width, height, right: left + width, bottom: top + height, x: left, y: top }) as DOMRect;

const page = rect(100, 50, 800, 600); // 100..900 × 50..650

eq(coveringRect(page, []), false, "no overlays");
eq(coveringRect(page, [rect(0, 0, 2000, 2000)]), true, "a full-window dialog backdrop");
eq(coveringRect(page, [rect(850, 600, 200, 200)]), true, "a toast overlapping a corner");
eq(coveringRect(page, [rect(300, 200, 10, 10)]), true, "a small menu inside the page");
eq(coveringRect(page, [rect(0, 0, 50, 50), rect(880, 640, 40, 40)]), true, "any one of several");
eq(coveringRect(page, [rect(0, 0, 100, 600)]), false, "touches the left edge only");
eq(coveringRect(page, [rect(900, 50, 100, 100)]), false, "touches the right edge only");
eq(coveringRect(page, [rect(100, 0, 800, 50)]), false, "touches the top edge only");
eq(coveringRect(page, [rect(100, 650, 800, 30)]), false, "touches the bottom edge only");
eq(coveringRect(page, [rect(1000, 700, 100, 100)]), false, "fully outside");
eq(coveringRect(page, [rect(300, 200, 0, 0)]), false, "a 0-size overlay inside the page");
eq(coveringRect(page, [rect(300, 200, 0, 100)]), false, "a 0-width overlay");
eq(coveringRect(rect(100, 50, 0, 0), [rect(0, 0, 2000, 2000)]), false, "a 0-size page");

console.log("overlay: ok");
