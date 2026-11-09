// `pnpm mobile:check-crypto`: the browser crypto must produce the Rust known-answer frame.
import { KNOWN_CODE_FRAME, KNOWN_FRAME, knownCodeFrame, knownFrame, x25519Agrees } from "./crypto.ts";

const got = knownFrame();
if (got !== KNOWN_FRAME) throw new Error(`browser crypto differs from Rust: ${got} != ${KNOWN_FRAME}`);
const code = knownCodeFrame();
if (code !== KNOWN_CODE_FRAME) throw new Error(`browser code-pairing keys differ from Rust: ${code} != ${KNOWN_CODE_FRAME}`);
if (!x25519Agrees()) throw new Error("X25519 doesn't match RFC 7748");
console.log("browser crypto matches the Rust known answers");
