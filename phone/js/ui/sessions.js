// @ts-check
// The session list (design 10.5, 2): connection state at the top (in the header), then the groups
// with "Needs you" first. Each row: name, folder's last segment, status word, the program's message
// and the time since the status changed. Offline, the last list received is shown, with its time.

import { el, ago } from "./dom.js";
import { groupOf, statusWord } from "../protocol.js";

/** The groups in the order the phone shows them (design 10.5). */
const GROUPS = /** @type {const} */ ([
  ["needs", "Needs you"],
  ["working", "Working"],
  ["idle", "Idle or done"],
  ["asleep", "Asleep"],
  ["exited", "Exited"],
]);

/**
 * The last path segment of a folder, for a short label ("C:/code/api" gives "api").
 * @param {string} folder
 * @returns {string}
 */
export function folderLabel(folder) {
  const parts = folder.split(/[\\/]+/).filter((p) => p.length > 0);
  return parts[parts.length - 1] ?? folder;
}

/**
 * @param {HTMLElement[]} out
 * @param {any} ctx
 */
export function renderSessions(out, ctx) {
  const list = ctx.list();
  /** @type {import("../protocol.js").SessionInfo[]} */
  const sessions = list ? list.list() : [];
  const hello = ctx.state.hello;
  out.push(
    el(
      "div",
      { className: "toolbar" },
      el("h2", { text: "Sessions" }),
      hello?.level === "operator" ? el("a", { className: "button", attrs: { href: "#/new" }, text: "New session" }) : null,
      el("button", {
        text: "Refresh",
        attrs: { disabled: !ctx.state.client },
        on: {
          click: () => {
            ctx.request("snapshot").catch((/** @type {Error} */ e) => ctx.notice(e.message));
          },
        },
      }),
    ),
  );
  if (list && list.unread > 0) {
    out.push(el("p", { className: "notice-line", text: `${list.unread} unread ${list.unread === 1 ? "result" : "results"} in the inbox.` }));
  }
  if (sessions.length === 0) {
    out.push(
      el("p", {
        className: "muted",
        text: ctx.state.client ? "No sessions on this PC yet." : "No list yet. It appears when the PC is reachable.",
      }),
    );
    return;
  }
  const now = Date.now();
  for (const [key, title] of GROUPS) {
    const rows = sessions
      .filter((s) => groupOf(s.status) === key)
      .sort((a, b) => (key === "needs" ? needsRank(a.status) - needsRank(b.status) : 0));
    if (rows.length === 0) continue;
    out.push(el("h3", { className: "group", text: `${title} (${rows.length})` }));
    out.push(
      el(
        "ul",
        { className: "rows" },
        rows.map((s) =>
          el(
            "li",
            {},
            el(
              "a",
              { className: `row row-${key}`, attrs: { href: `#/session/${encodeURIComponent(s.id)}` } },
              el("span", { className: "row-name", text: s.name }),
              el("span", { className: "row-meta", text: [folderLabel(s.folder), statusWord(s.status), ago(s.since, now)].filter(Boolean).join(" · ") }),
              s.message ? el("span", { className: "row-message", text: s.message }) : null,
            ),
          ),
        ),
      ),
    );
  }
}

/**
 * Approval first, then a question.
 * @param {string} status
 * @returns {number}
 */
function needsRank(status) {
  return status === "needs_approval" ? 0 : 1;
}
