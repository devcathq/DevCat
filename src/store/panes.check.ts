// `pnpm check:panes`: the split pane tree and which tab each pane shows (store/panes.ts).
// Runs under plain Node (type stripping): no node:* imports, no @types/node.
import { initialPanes, moveTab, nextPane, openBeside, paneIds, paneTabs, placeTab, removeNode, removeTab, splitNode, splitTab, type Panes } from "./panes.ts";

function eq(actual: unknown, expected: unknown, what: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}\n  expected ${e}\n  actual   ${a}`);
}

// --- the tree
const A = { pane: "A" };
const row = splitNode(A, "A", "B", "right");
eq(row, { split: "s-B", dir: "row", children: [{ pane: "A" }, { pane: "B" }] }, "split right makes a row");
eq(paneIds(splitNode(row, "A", "C", "right")), ["A", "C", "B"], "same direction joins the row, next to the target");
eq(paneIds(splitNode(row, "A", "C", "left")), ["C", "A", "B"], "left goes before");
const nested = splitNode(row, "B", "C", "bottom");
eq(nested, { split: "s-B", dir: "row", children: [{ pane: "A" }, { split: "s-C", dir: "col", children: [{ pane: "B" }, { pane: "C" }] }] }, "other direction nests");
eq(paneIds(splitNode(A, "A", "B", "top")), ["B", "A"], "top goes before");
eq(removeNode(nested, "C"), row, "a split left with one child gives way to it");
eq(removeNode(A, "A"), null, "nothing left");
// row[A, col[row[B, D], C]] minus C: the col collapses to row[B, D], which merges into the outer row
const deep = splitNode(nested, "B", "D", "right");
eq(paneIds(deep), ["A", "B", "D", "C"], "deep split order");
eq(removeNode(deep, "C"), { split: "s-B", dir: "row", children: [{ pane: "A" }, { pane: "B" }, { pane: "D" }] }, "same-direction splits merge on collapse");
eq(nextPane(nested, "C"), "A", "next pane wraps");
eq(nextPane(A, "A"), null, "no next pane alone");

// --- tabs in panes
let ids = ["t1", "t2", "t3"];
let p: Panes = initialPanes("A", ids);
eq(p.paneActive, { A: "t3" }, "last tab active at start");
p = placeTab(p, "t4");
ids = [...ids, "t4"];
eq([p.paneOf.t4, p.paneActive.A], ["A", "t4"], "new tab goes to the focused pane");

p = splitTab(p, ids, "t2", "right", "B");
eq(paneIds(p.layout), ["A", "B"], "split made pane B");
eq([p.paneOf.t2, p.paneActive.B, p.focusedPane, p.paneActive.A], ["B", "t2", "B", "t4"], "t2 moved to B, focused; A keeps its active tab");
eq(paneTabs(ids, p, "A"), ["t1", "t3", "t4"], "A's tabs in tab order");
eq(splitTab(p, ids, "t2", "bottom", "X"), p, "a pane's only tab doesn't split off its own pane");

// a pane's only tab dropped on another pane's edge: splits there, its old pane goes
const edge = splitTab(p, ids, "t2", "bottom", "C", "A");
eq(edge.layout, { split: "s-C", dir: "col", children: [{ pane: "A" }, { pane: "C" }] }, "edge drop splits the target; the empty source collapses");
eq([edge.paneOf.t2, edge.focusedPane, "B" in edge.paneActive], ["C", "C", false], "tab in the new pane, focused; B forgotten");

p = placeTab(p, "t5");
ids = [...ids, "t5"];
eq([p.paneOf.t5, p.paneActive.B], ["B", "t5"], "opened in the focused pane B");

// closing: the neighbour in the same pane takes over
p = removeTab(p, ids, "t5");
ids = ids.filter((t) => t !== "t5");
eq([p.paneActive.B, p.focusedPane], ["t2", "B"], "close: previous tab in the pane");
p = { ...p, paneActive: { ...p.paneActive, A: "t1" } };
let q = removeTab(p, ids, "t1");
eq(q.paneActive.A, "t3", "close: the next tab when there is one");
q = removeTab(p, ids, "t4");
eq(q.paneActive.A, "t1", "closing an inactive tab keeps the active one");

// the last tab of a pane: the pane goes, focus moves to its neighbour
q = removeTab(p, ids, "t2");
eq([paneIds(q.layout), q.focusedPane, q.paneOf.t2], [["A"], "A", undefined], "empty pane removed, focus to neighbour");
eq(q.layout, { pane: "A" }, "tree collapses to a single pane");

// moving between panes
q = moveTab(p, ids, "t2", "A");
eq([paneIds(q.layout), q.paneOf.t2, q.paneActive.A, q.focusedPane], [["A"], "A", "t2", "A"], "move the only tab: source pane goes");
q = moveTab(p, ids, "t3", "B");
eq([q.paneOf.t3, q.paneActive.B, q.paneActive.A, q.focusedPane], ["B", "t3", "t1", "B"], "move into B, A keeps going");
eq(moveTab(p, ids, "t3", "nope"), p, "no such pane");

// the last pane stays, empty
let solo: Panes = initialPanes("A", ["w"]);
solo = removeTab(solo, ["w"], "w");
eq([solo.layout, solo.paneActive.A, solo.focusedPane], [{ pane: "A" }, null, "A"], "always one pane");

// a new tab beside a pane (split terminal)
q = openBeside(p, "t9", "bottom", "C", "B");
eq([paneIds(q.layout), q.paneOf.t9, q.focusedPane, q.paneActive.B], [["A", "B", "C"], "C", "C", "t2"], "openBeside");
eq(q.layout, { split: "s-B", dir: "row", children: [{ pane: "A" }, { split: "s-C", dir: "col", children: [{ pane: "B" }, { pane: "C" }] }] }, "openBeside nests a col");

// closing the last tab of a pane the user isn't in leaves the focus where it is
const three = openBeside(p, "t9", "bottom", "C", "B"); // A | B over C, focus on C
const unfocused = removeTab({ ...three, focusedPane: "A" }, [...ids, "t9"], "t9");
eq([paneIds(unfocused.layout), unfocused.focusedPane, "C" in unfocused.paneActive], [["A", "B"], "A", false], "unfocused pane removed, focus kept");
// the focused pane is always one of the tree's panes, and every tab's pane has an active tab
for (const st of [q, three, unfocused, edge]) {
  if (!paneIds(st.layout).includes(st.focusedPane)) throw new Error("focusedPane not in the tree");
  for (const pane of Object.values(st.paneOf)) if (!paneIds(st.layout).includes(pane)) throw new Error(`tab in missing pane ${pane}`);
  for (const [pane, a] of Object.entries(st.paneActive)) if (a && st.paneOf[a] !== pane) throw new Error(`paneActive ${pane} -> ${a} not in it`);
}

console.log("panes: ok");
