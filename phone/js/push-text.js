// Classic script (not a module): the text of a "needs you" push. Loaded by sw.js with importScripts,
// and by the tests with a vm context. It defines one global, `CsmPush`. The payload is the decrypted
// PushPayload of msg.rs; a payload that does not match is not shown with names, only the generic text.
(function (root) {
  "use strict";

  var TITLE = "CodeShellManager Phone";
  var TAG = "needs-you";
  var GENERIC = "A session needs you";

  /**
   * @param {unknown} payload
   * @returns {{ n: number, s: string | null } | null}
   */
  function parse(payload) {
    if (!payload || typeof payload !== "object") return null;
    var p = payload;
    if (p.t !== "needs_you") return null;
    if (!Number.isSafeInteger(p.n) || p.n < 1 || p.n > 100000) return null;
    var s = null;
    if (p.s !== undefined) {
      if (typeof p.s !== "string" || p.s.length < 1 || p.s.length > 128 || /[\u0000-\u001f\u007f]/.test(p.s)) return null;
      s = p.s;
    }
    if (p.sid !== undefined && (typeof p.sid !== "string" || p.sid.length > 64)) return null;
    return { n: p.n, s: s };
  }

  /**
   * @param {{ n: number, s: string | null }} p
   * @returns {string}
   */
  function text(p) {
    if (p.s !== null && p.n === 1) return p.s + " needs you";
    if (p.s !== null) return p.s + " and " + (p.n - 1) + " more need you";
    return p.n === 1 ? "1 session needs you" : p.n + " sessions need you";
  }

  /**
   * The notification to show for a push event's data.
   * @param {unknown} payload
   * @returns {{ title: string, body: string, tag: string }}
   */
  function notification(payload) {
    var p = parse(payload);
    return { title: TITLE, body: p ? text(p) : GENERIC, tag: TAG };
  }

  root.CsmPush = { TITLE: TITLE, TAG: TAG, GENERIC: GENERIC, parse: parse, text: text, notification: notification };
})(typeof self !== "undefined" ? self : globalThis);
