// Browser side of the encrypted session (spec: crates/remote-proto/src/web.rs). Pages served over
// plain http can't use the browser's WebCrypto, so this uses the audited @noble libraries:
// HKDF-SHA256 for the keys, ChaCha20-Poly1305 for the frames, X25519 for pairing by code.
import { chacha20poly1305 } from "@noble/ciphers/chacha.js";
import { x25519 } from "@noble/curves/ed25519.js";
import { hkdf } from "@noble/hashes/hkdf.js";
import { sha256 } from "@noble/hashes/sha2.js";

const enc = new TextEncoder();

export function b64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function b64urlDecode(s: string): Uint8Array {
  const b = atob(s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4));
  return Uint8Array.from(b, (c) => c.charCodeAt(0));
}

export function randomBytes(n: number): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(n));
}

function seqBytes(seq: bigint): Uint8Array {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, seq);
  return b;
}

function nonceFor(seq: bigint): Uint8Array {
  const n = new Uint8Array(12);
  n.set(seqBytes(seq), 4);
  return n;
}

/** One direction: a key and its frame counter. */
export class Half {
  private seq = 0n;
  private key: Uint8Array;
  constructor(key: Uint8Array) {
    this.key = key;
  }

  seal(plaintext: Uint8Array): Uint8Array {
    const s = seqBytes(this.seq);
    const ct = chacha20poly1305(this.key, nonceFor(this.seq), s).encrypt(plaintext);
    this.seq++;
    const out = new Uint8Array(8 + ct.length);
    out.set(s);
    out.set(ct, 8);
    return out;
  }

  /** null if out of order, replayed, tampered or under another key. */
  open(frame: Uint8Array): Uint8Array | null {
    if (frame.length < 24) return null;
    const seq = new DataView(frame.buffer, frame.byteOffset, 8).getBigUint64(0);
    if (seq !== this.seq) return null;
    try {
      const plain = chacha20poly1305(this.key, nonceFor(seq), frame.subarray(0, 8)).decrypt(frame.subarray(8));
      this.seq++;
      return plain;
    } catch {
      return null;
    }
  }
}

export type Mode = "pair" | "device" | "code";

/** A pairing code as typed: letters and digits only, upper case (Rust `normalize_code`). */
export function normalizeCode(code: string): string {
  return code.replace(/[^0-9a-z]/gi, "").toUpperCase();
}

/** Pairing by code: a fresh X25519 key; `secret` gives what the `code` mode's keys come from
 *  (shared secret ‖ code), throwing on a key that isn't usable. */
export function codeKey() {
  const sk = x25519.utils.randomSecretKey();
  return {
    public: x25519.getPublicKey(sk),
    secret: (hostKey: Uint8Array, code: string) => codeSecret(x25519.getSharedSecret(sk, hostKey), code),
  };
}

function codeSecret(shared: Uint8Array, code: string): Uint8Array {
  const c = enc.encode(normalizeCode(code));
  const out = new Uint8Array(shared.length + c.length);
  out.set(shared);
  out.set(c, shared.length);
  return out;
}

/** The browser's keys: `send` = browser → computer, `recv` = computer → browser. */
export function browserKeys(secret: Uint8Array, clientNonce: Uint8Array, hostNonce: Uint8Array, mode: Mode) {
  const salt = new Uint8Array(clientNonce.length + hostNonce.length);
  salt.set(clientNonce);
  salt.set(hostNonce, clientNonce.length);
  const k = (dir: string) => hkdf(sha256, secret, salt, enc.encode(`devcat-web/1 ${mode} ${dir}`), 32);
  return { send: new Half(k("c2h")), recv: new Half(k("h2c")) };
}

/** Same frames as the Rust known-answer tests (crates/remote-proto/src/web.rs). */
export const KNOWN_FRAME = "AAAAAAAAAABO6TjSdXFSiXF7k2BZrBtoGllhdWPwQkElkhpx";
export function knownFrame(): string {
  const k = browserKeys(new Uint8Array(32).fill(7), new Uint8Array(16).fill(1), new Uint8Array(16).fill(2), "pair");
  return b64url(k.send.seal(enc.encode('{"t":"ping"}')));
}

export const KNOWN_CODE_FRAME = "AAAAAAAAAADzgV6usRZ3cRk_v3g6W1E3C2ad5DH-i0PUJO2O";
export function knownCodeFrame(): string {
  const k = browserKeys(codeSecret(new Uint8Array(32).fill(7), "ab3k-7q"), new Uint8Array(16).fill(1), new Uint8Array(16).fill(2), "code");
  return b64url(k.send.seal(enc.encode('{"t":"ping"}')));
}

/** Two X25519 keys agree (RFC 7748 §6.1's Alice and Bob). */
export function x25519Agrees(): boolean {
  const hex = (h: string) => Uint8Array.from(h.match(/../g)!, (b) => parseInt(b, 16));
  const alice = hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
  const bobPub = hex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
  return b64url(x25519.getSharedSecret(alice, bobPub)) === b64url(hex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742"));
}
