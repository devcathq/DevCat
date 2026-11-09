/** The hosted browser version (`vite --mode hosted`, GitHub Pages): reaches the computer through
 *  its relay from any network. */
export const IS_HOSTED = import.meta.env.MODE === "hosted";
/** A browser version (served by DevCat on the computer, or hosted), not the installed app. */
export const IS_WEB = import.meta.env.MODE === "web" || IS_HOSTED;

/** What the pairing screen gets for a code typed in the browser version: `code:<code>`. */
export const CODE_LINK = "code:";
/** Pairing by typing the code: on the page the computer served (it knows which computer it came
 *  from), not in the app or the hosted page (they'd need the computer's id and address too). */
export const CAN_PAIR_BY_CODE = import.meta.env.MODE === "web";

/** Does this look like a DevCat pairing link (app link, or the browser link from the QR code)? */
export function isPairLink(s: string): boolean {
  const t = s.trim();
  return t.startsWith("devcat://pair") || (/^https?:\/\//.test(t) && /[#&]id=[0-9a-f]{64}/i.test(t) && /[#&]code=/.test(t));
}
