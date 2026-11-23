// @ts-check
// Session detail and the new-session form (design 10.5, 3 and 4). Viewers see what the PC lets them
// see. Operators get the buttons for the fixed op set. Every destructive button says what it does in
// words, and Close asks first; a busy session asks again before it is closed by force.

import { el, field } from "./dom.js";
import { ClientError } from "../client.js";
import {
  parseCommand,
  parseFolder,
  parseList,
  parseNewSession,
  parseProfile,
  parseSession,
  statusWord,
} from "../protocol.js";
import { folderLabel } from "./sessions.js";

/** The session being looked at, and what has been asked of it. */
let detail = {
  id: "",
  session: /** @type {any} */ (null),
  error: "",
  commands: /** @type {Array<{ command: string, label: string }> | null} */ (null),
  showCommands: false,
  closeAsk: false,
  busyAsk: false,
};

/** The new-session form, kept across re-renders. */
let newForm = {
  folders: /** @type {Array<{ path: string, name: string, pinned: boolean }> | null} */ (null),
  profiles: /** @type {Array<{ name: string, label: string }> | null} */ (null),
  folder: "",
  profile: "",
  name: "",
  remote: false,
  loadError: "",
};

/**
 * Text for a failed request: CSM's own words for a tool failure, the plain message otherwise.
 * @param {unknown} e
 * @returns {string}
 */
function textOf(e) {
  if (e instanceof ClientError) return e.code === "failed" && e.detail ? e.detail : e.message;
  return "Something went wrong. Try again.";
}

/**
 * @param {any} ctx
 * @param {string} op
 * @param {Record<string, unknown>} args
 * @param {string} done what to say when it worked
 */
async function act(ctx, op, args, done) {
  try {
    await ctx.request(op, args);
    ctx.notice(done);
    await load(ctx, String(args.session_id ?? detail.id));
  } catch (e) {
    if (e instanceof ClientError && e.code === "busy" && op === "session.close") {
      detail.busyAsk = true;
      ctx.refresh();
      return;
    }
    ctx.notice(textOf(e));
  }
}

/**
 * Reads the session from the PC.
 * @param {any} ctx
 * @param {string} id
 */
async function load(ctx, id) {
  try {
    const data = await ctx.request("session.get", { session_id: id });
    const parsed = parseSession(data);
    if (!parsed) throw new ClientError("bad_request");
    if (detail.id === id) {
      detail.session = parsed;
      detail.error = "";
    }
  } catch (e) {
    if (detail.id === id) {
      detail.session = null;
      detail.error = textOf(e);
    }
  }
  ctx.refresh();
}

/**
 * @param {HTMLElement[]} out
 * @param {any} ctx
 * @param {string} id
 */
export function renderSession(out, ctx, id) {
  if (detail.id !== id) {
    detail = { id, session: null, error: "", commands: null, showCommands: false, closeAsk: false, busyAsk: false };
    if (ctx.state.client) void load(ctx, id);
  }
  const s = detail.session;
  out.push(el("p", { className: "back" }, el("a", { attrs: { href: "#/sessions" }, text: "‹ Sessions" })));
  if (!s) {
    out.push(el("p", { className: detail.error ? "error" : "muted", text: detail.error || "Loading…" }));
    return;
  }
  out.push(el("h2", { text: s.name }));
  out.push(
    el(
      "dl",
      { className: "facts" },
      el("dt", { text: "Status" }),
      el("dd", { text: statusWord(s.status) }),
      el("dt", { text: "Folder" }),
      el("dd", { text: `${folderLabel(s.folder)} · ${s.folder}` }),
      s.branch ? el("dt", { text: "Branch" }) : null,
      s.branch ? el("dd", { text: s.branch }) : null,
      s.message ? el("dt", { text: "Message" }) : null,
      s.message ? el("dd", { text: s.message }) : null,
    ),
  );
  const level = ctx.state.hello?.level;
  if (level !== "operator") {
    out.push(el("p", { className: "muted", text: "You can see this session. Ask the person at the PC to make this phone an operator to change it." }));
    return;
  }
  out.push(operateView(ctx, s, id));
}

/**
 * The operator buttons, by what the session is doing now.
 * @param {any} ctx
 * @param {any} s
 * @param {string} id
 * @returns {HTMLElement}
 */
function operateView(ctx, s, id) {
  const asleep = s.status === "asleep";
  const exited = s.status === "exited";
  /** @type {HTMLElement[]} */
  const parts = [];
  parts.push(
    el(
      "div",
      { className: "actions" },
      asleep ? el("button", { className: "primary", text: "Wake it up: start the program again", on: { click: () => void act(ctx, "session.wake", { session_id: id }, "Waking the session.") } }) : null,
      !asleep && !exited ? el("button", { text: "Sleep: stop the program, keep the session", on: { click: () => void act(ctx, "session.sleep", { session_id: id }, "The session is asleep.") } }) : null,
      !exited ? el("button", { text: "Restart: stop and start the program", on: { click: () => void act(ctx, "session.restart", { session_id: id }, "The session restarted.") } }) : null,
      el("button", {
        text: "Clear status",
        on: { click: () => void act(ctx, "session.status_clear", { session_id: id }, "The status is cleared.") },
      }),
    ),
  );
  if (!detail.closeAsk && !detail.busyAsk) {
    parts.push(
      el("div", { className: "actions" }, el("button", { className: "danger", text: "Close the session…", on: { click: () => ((detail.closeAsk = true), ctx.refresh()) } })),
    );
  }
  if (detail.closeAsk) {
    parts.push(
      el(
        "div",
        { className: "confirm", attrs: { role: "alert" } },
        el("p", { text: "Close this session? Its program stops, and the session is removed from the list." }),
        el(
          "div",
          { className: "actions" },
          el("button", {
            className: "danger",
            text: "Close it",
            on: {
              click: () => {
                detail.closeAsk = false;
                void act(ctx, "session.close", { session_id: id, force: false }, "The session is closed.");
              },
            },
          }),
          el("button", { text: "Keep it", on: { click: () => ((detail.closeAsk = false), ctx.refresh()) } }),
        ),
      ),
    );
  }
  if (detail.busyAsk) {
    parts.push(
      el(
        "div",
        { className: "confirm", attrs: { role: "alert" } },
        el("p", { text: "This session is busy. Close it anyway?" }),
        el(
          "div",
          { className: "actions" },
          el("button", {
            className: "danger",
            text: "Close anyway",
            on: {
              click: () => {
                detail.busyAsk = false;
                void act(ctx, "session.close", { session_id: id, force: true }, "The session is closed.");
              },
            },
          }),
          el("button", { text: "Keep it", on: { click: () => ((detail.busyAsk = false), ctx.refresh()) } }),
        ),
      ),
    );
  }
  parts.push(commandsView(ctx, id));
  return el("section", { className: "stack" }, parts);
}

/**
 * Remote Control and the other allowed slash commands (from the PC's list).
 * @param {any} ctx
 * @param {string} id
 * @returns {HTMLElement}
 */
function commandsView(ctx, id) {
  const commands = detail.commands;
  if (!detail.showCommands) {
    return el("div", { className: "actions" }, el("button", {
      text: "Send a command…",
      on: {
        click: async () => {
          detail.showCommands = true;
          try {
            const data = await ctx.request("command.list");
            detail.commands = parseList(data, "commands", parseCommand) ?? [];
          } catch (e) {
            detail.commands = [];
            ctx.notice(textOf(e));
          }
          ctx.refresh();
        },
      },
    }));
  }
  if (!commands || commands.length === 0) {
    return el("p", { className: "muted", text: "The PC allows no commands from a phone. Ask the person at the PC to allow them in Phone remote settings." });
  }
  return el(
    "div",
    { className: "stack" },
    el("h3", { text: "Send a command" }),
    commands.map((c) =>
      el("button", {
        text: c.command === "/remote-control" ? "Turn on Remote Control" : `${c.label || c.command} (${c.command})`,
        on: { click: () => void act(ctx, "session.command", { session_id: id, command: c.command, force: false }, "Sent.") },
      }),
    ),
    el("button", { text: "Hide commands", on: { click: () => ((detail.showCommands = false), ctx.refresh()) } }),
  );
}

/**
 * @param {HTMLElement[]} out
 * @param {any} ctx
 */
export function renderNewSession(out, ctx) {
  out.push(el("p", { className: "back" }, el("a", { attrs: { href: "#/sessions" }, text: "‹ Sessions" })));
  out.push(el("h2", { text: "New session" }));
  if (ctx.state.hello?.level !== "operator") {
    out.push(el("p", { className: "muted", text: "Only an operator phone can start sessions. Ask the person at the PC." }));
    return;
  }
  if (!newForm.folders && ctx.state.client) void loadChoices(ctx);
  if (!newForm.folders || !newForm.profiles) {
    out.push(el("p", { className: newForm.loadError ? "error" : "muted", text: newForm.loadError || "Loading the folders and profiles…" }));
    return;
  }
  if (newForm.folders.length === 0) {
    out.push(el("p", { className: "muted", text: "No folder is on the PC's list for phones. Add one in Phone remote settings on the PC." }));
    return;
  }
  const folder = el(
    "select",
    { attrs: { id: "folder", "aria-label": "Folder" }, on: { change: (e) => (newForm.folder = /** @type {HTMLSelectElement} */ (e.target).value) } },
    newForm.folders.map((f) => el("option", { attrs: { value: f.path, selected: f.path === newForm.folder }, text: `${f.name} (${f.path})` })),
  );
  const profile = el(
    "select",
    { attrs: { id: "profile", "aria-label": "Profile" }, on: { change: (e) => (newForm.profile = /** @type {HTMLSelectElement} */ (e.target).value) } },
    newForm.profiles.map((p) => el("option", { attrs: { value: p.name, selected: p.name === newForm.profile }, text: p.label || p.name })),
  );
  const name = field("Name (optional)", { id: "session-name", value: newForm.name, maxLength: 128 });
  name.addEventListener("input", () => (newForm.name = name.value));
  const remote = el("input", {
    attrs: { id: "remote", type: "checkbox", checked: newForm.remote },
    on: { change: (e) => (newForm.remote = /** @type {HTMLInputElement} */ (e.target).checked) },
  });
  out.push(
    el(
      "section",
      { className: "stack" },
      el("label", { attrs: { for: "folder" }, text: "Folder" }),
      folder,
      el("label", { attrs: { for: "profile" }, text: "Profile" }),
      profile,
      name,
      el("label", { className: "check" }, remote, el("span", { text: "Turn on Remote Control when the session is ready" })),
      el(
        "div",
        { className: "actions" },
        el("button", {
          className: "primary",
          text: "Start the session",
          on: { click: () => void startSession(ctx) },
        }),
      ),
    ),
  );
}

/**
 * @param {any} ctx
 */
async function loadChoices(ctx) {
  try {
    const [f, p] = await Promise.all([ctx.request("folder.list"), ctx.request("profile.list")]);
    newForm.folders = parseList(f, "folders", parseFolder) ?? [];
    newForm.profiles = parseList(p, "profiles", parseProfile) ?? [];
    if (!newForm.folder && newForm.folders[0]) newForm.folder = newForm.folders[0].path;
    if (!newForm.profile && newForm.profiles[0]) newForm.profile = newForm.profiles[0].name;
    newForm.loadError = "";
  } catch (e) {
    newForm.loadError = textOf(e);
    newForm.folders = null;
  }
  ctx.refresh();
}

/**
 * @param {any} ctx
 */
async function startSession(ctx) {
  const name = newForm.name.replace(/[\u0000-\u001f\u007f]/g, "").trim();
  if (!newForm.folder || !newForm.profile) {
    ctx.notice("Choose a folder and a profile.");
    return;
  }
  try {
    const data = await ctx.request("session.new", {
      folder: newForm.folder,
      profile: newForm.profile,
      ...(name ? { name } : {}),
      remote_control: newForm.remote,
    });
    const made = parseNewSession(data);
    if (!made) throw new ClientError("bad_request");
    newForm = { ...newForm, name: "", remote: false };
    ctx.go(`/session/${encodeURIComponent(made.id)}`);
    ctx.notice(made.remote_control === "pending" ? "Started. Remote Control turns on when the session is ready." : "Started.");
  } catch (e) {
    ctx.notice(textOf(e));
  }
}

