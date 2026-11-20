// `pnpm check:agent`: how a DevCat Agent chat's log and live events become what the chat shows
// (features/agent/model.ts), for the parts a long turn leans on: a step that's tried again, a
// summary that fails, a pause at the step limit, what DevCat told the agent itself.
// Runs under plain Node (type stripping): no node:* imports, no @types/node.
import { ZERO, applyAll, hookLabel, itemsFromLog, retryLabel, toBlocks, type ChatView, type Item } from "./model.ts";
import type { AgentLiveEvent, AgentSessionEvent } from "../../lib/types.ts";

function eq(actual: unknown, expected: unknown, what: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}\n  expected ${e}\n  actual   ${a}`);
}

const chat = (): ChatView => ({ title: "", busy: false, error: null, items: [], usage: ZERO, context: null, contextTokens: 0 });
const usage = { input_tokens: 10, output_tokens: 2, cache_read_tokens: 0 };
/** What the chat shows, in a few words per item. */
const shape = (items: Item[]) => items.map((i) => (i.kind === "answer" ? `answer:${i.text}${i.streaming ? "…" : ""}` : i.kind === "tool" ? `tool:${i.name}:${i.status}` : i.kind === "retry" ? `retry:${i.attempt}/${i.of}` : i.kind === "compacted" ? `compacted:${i.state}${i.error ? `:${i.error}` : ""}` : i.kind === "end" ? `end:${i.end}` : i.kind));
const play = (events: AgentLiveEvent[]) => applyAll(chat(), events);

// --- a step that's tried again: what it streamed is void, and comes again
const start: AgentLiveEvent[] = [
  { type: "turnStart", text: "fix it" },
  { type: "text", text: "Let me " },
  { type: "toolStart", id: "t1", name: "read" },
];
let c = play([...start, { type: "retry", attempt: 1, of: 4, reason: "the connection closed before the model finished its answer" }]);
eq(shape(c.items), ["user", "retry:1/4"], "the half-written answer and the call that only started go, and the chat says why");
c = applyAll(c, [{ type: "retry", attempt: 2, of: 4, reason: "model provider: 529 Overloaded" }]);
eq(shape(c.items), ["user", "retry:2/4"], "one note, with the latest try");
c = applyAll(c, [
  { type: "text", text: "Let me look." },
  { type: "toolStart", id: "t2", name: "read" },
  { type: "step", content: [{ type: "text", text: "Let me look." }, { type: "tool_use", id: "t2", name: "read", input: { path: "a.txt" } }], usage },
]);
eq(shape(c.items), ["user", "answer:Let me look.", "tool:read:pending"], "it came through: the answer once, and the note is gone");
// a retry before anything was streamed takes nothing away, and earlier steps are never touched
c = applyAll(c, [{ type: "toolCall", id: "t2", name: "read", summary: "a.txt", input: { path: "a.txt" } }, { type: "toolResult", id: "t2", content: "x", is_error: false }, { type: "retry", attempt: 1, of: 4, reason: "model provider: 503 busy" }]);
eq(shape(c.items), ["user", "answer:Let me look.", "tool:read:done", "retry:1/4"], "what finished before stays");
c = applyAll(c, [{ type: "turnEnd", end: "error", error: "model provider: 503 busy", usage, title: "fix it", elapsedMs: 1000 }]);
eq(shape(c.items), ["user", "answer:Let me look.", "tool:read:done", "end:error"], "a turn that ends takes the note with it");
// while it's the last thing that happened it shows under the folded steps, like a step in progress
const blocks = toBlocks(play([...start.slice(0, 1), { type: "toolStart", id: "a", name: "glob" }, { type: "step", content: [{ type: "tool_use", id: "a", name: "glob", input: {} }], usage }, { type: "retry", attempt: 1, of: 4, reason: "x" }]).items);
eq(blocks.map((b) => (b.kind === "steps" ? b.items.map((i) => i.kind) : b.item.kind)), ["user", ["tool", "retry"]], "the note folds in with the steps");

// a message sent while the step streamed waits at the end: what's void before it still goes
c = play([...start, { type: "queued", text: "also the docs" }, { type: "retry", attempt: 1, of: 4, reason: "x" }, { type: "text", text: "Let me look." }, { type: "step", content: [{ type: "text", text: "Let me look." }], usage }]);
eq(shape(c.items), ["user", "user", "answer:Let me look."], "one answer, finished, and no call left pending");

eq([retryLabel("model provider: 429 rate limited"), retryLabel("model provider: 529 Overloaded"), retryLabel("the connection closed before the model finished its answer"), retryLabel("error decoding response body")],
  ["The provider is rate-limiting", "The provider is busy", "The connection to the model broke", "The connection to the model broke"], "why it's tried again, in a few words");

// --- a summary that fails in the middle of a turn: said at once, and the turn goes on
c = play([{ type: "turnStart", text: "go" }, { type: "compacting", tokens_before: 900 }, { type: "compactFailed", message: "model provider: 500 boom" }, { type: "text", text: "Going on." }]);
eq(shape(c.items), ["user", "compacted:failed:model provider: 500 boom", "answer:Going on.…"], "the row stops spinning and says why");
eq(c.busy, true, "the turn is still running");
c = play([{ type: "turnStart", text: "go" }, { type: "compacting", tokens_before: 900 }, { type: "compacted", summary: "S", tokens_before: 900, usage }]);
eq(shape(c.items), ["user", "compacted:done"], "a summary that works is as before");

// --- the log of a chat opened again
const log: AgentSessionEvent[] = [
  { event: "user", text: "port it" },
  { event: "assistant", content: [{ type: "text", text: "Working on it" }], model: "m", usage },
  { event: "user", text: "Your message was cut off at the output limit. Go on exactly where it stopped, without repeating what you already wrote.", hook: "DevCat" },
  { event: "assistant", content: [{ type: "text", text: "…done so far." }], model: "m", usage },
  { event: "interrupted", reason: "Stopped after 200 steps" },
  { event: "user", text: "continue" },
  { event: "interrupted", reason: "Stopped by the user" },
  { event: "user", text: "again" },
  { event: "interrupted", reason: "model provider: 500 boom" },
] as AgentSessionEvent[];
const items = itemsFromLog(log);
eq(shape(items), ["user", "answer:Working on it", "hook", "answer:…done so far.", "end:stepLimit", "user", "end:stopped", "user", "end:error"], "a pause at the step limit reads as a pause, not as an error");
eq(items.filter((i) => i.kind === "end").map((i) => (i.kind === "end" ? i.error : "")), [null, null, "model provider: 500 boom"], "only a failure carries an error");
eq([hookLabel("DevCat"), hookLabel("Stop")], ["DevCat told the agent", "Stop hook"], "what DevCat told the agent isn't a hook of the user's");

console.log("agent model: ok");
