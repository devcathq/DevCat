// `pnpm check:osc52`: what an OSC 52 sequence copies, and what is ignored (reads above all).
// Runs under plain Node (type stripping): no node:* imports, no @types/node.
import { osc52Text } from "./osc52.ts";

function eq(actual: unknown, expected: unknown, what: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}\n  expected ${e}\n  actual   ${a}`);
}

const b64 = (s: string) => btoa(String.fromCharCode(...new TextEncoder().encode(s)));

eq(osc52Text(`c;${b64("hello")}`), "hello", "clipboard selection");
eq(osc52Text(`;${b64("hello")}`), "hello", "empty selection list (xterm's default)");
eq(osc52Text(`pc;${b64("two")}`), "two", "several selections");
eq(osc52Text(`c;${b64("héllo → 🐱\nline 2")}`), "héllo → 🐱\nline 2", "UTF-8 and newlines");
eq(osc52Text(`c;${b64("ab").replace(/=+$/, "")}`), "ab", "unpadded base64");
eq(osc52Text(`c;${b64("a long line to wrap").replace(/(.{8})/g, "$1\n")}`), "a long line to wrap", "base64 with line breaks");

eq(osc52Text("c;?"), null, "read query is never answered");
eq(osc52Text("?"), null, "bare query");
eq(osc52Text(";?"), null, "query with an empty selection list");
eq(osc52Text("c;"), null, "clear (empty payload)");
eq(osc52Text("c;!!notbase64"), null, "invalid base64");
eq(osc52Text(`c;${btoa("\xff\xfe")}`), null, "not UTF-8");
eq(osc52Text(b64("hello")), null, "no selection field");

console.log("osc52: ok");
