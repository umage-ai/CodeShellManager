// @ts-check
// Settings (design 10.5, 5): this device's name, its level (the PC decides it), hide names and
// notifications (sent to the PC), notifications on this phone, forgetting this phone, and about.

import { el } from "./dom.js";

let forgetAsk = false;

/**
 * @param {HTMLElement[]} out
 * @param {any} ctx
 */
export function renderSettings(out, ctx) {
  const device = ctx.state.devices.find((/** @type {any} */ d) => d.host_id === ctx.state.activeId) ?? ctx.state.devices[0];
  const hello = ctx.state.hello;
  out.push(el("h2", { text: "Settings" }));
  if (!device) {
    out.push(el("p", { className: "muted", text: "No PC is paired with this phone." }));
    return;
  }
  out.push(el("h3", { text: "This phone" }));
  out.push(
    el(
      "dl",
      { className: "facts" },
      el("dt", { text: "Name" }),
      el("dd", { text: device.name }),
      el("dt", { text: "Level" }),
      el("dd", { text: `${device.level === "operator" ? "Operator" : "Viewer"}. Your PC decides this.` }),
    ),
  );
  if (!hello) {
    out.push(el("p", { className: "muted", text: "Connect to the PC to change these settings." }));
  } else {
    out.push(
      el(
        "section",
        { className: "stack" },
        el("label", { className: "check" }, checkbox("hide-names", hello.hide_names, (on) => ctx.setPrefs({ hide_names: on, notify: hello.notify })), el("span", { text: "Hide session names in notifications" })),
        el("label", { className: "check" }, checkbox("notify", hello.notify, (on) => ctx.setPrefs({ hide_names: hello.hide_names, notify: on })), el("span", { text: "Notify me when a session needs me" })),
        el(
          "label",
          { className: "check" },
          checkbox("push", ctx.state.pushOn, (on) => ctx.setPush(on).catch((/** @type {Error} */ e) => ctx.notice(e.message))),
          el("span", { text: "Show notifications on this phone" }),
        ),
      ),
    );
  }
  out.push(el("h3", { text: "Forget this phone" }));
  out.push(
    el(
      "p",
      { className: "muted", text: "This removes the pairing from this phone only. The PC still lists the phone until someone revokes it there." },
    ),
  );
  out.push(
    forgetAsk
      ? el(
          "div",
          { className: "confirm", attrs: { role: "alert" } },
          el("p", { text: `Forget ${device.name} on this phone?` }),
          el(
            "div",
            { className: "actions" },
            el("button", {
              className: "danger",
              text: "Yes, forget it",
              on: {
                click: () => {
                  forgetAsk = false;
                  void ctx.forget(device);
                },
              },
            }),
            el("button", { text: "Keep it", on: { click: () => ((forgetAsk = false), ctx.refresh()) } }),
          ),
        )
      : el("div", { className: "actions" }, el("button", { className: "danger", text: "Forget this phone…", on: { click: () => ((forgetAsk = true), ctx.refresh()) } })),
  );
  out.push(el("h3", { text: "About" }));
  out.push(
    el(
      "dl",
      { className: "facts" },
      el("dt", { text: "App version" }),
      el("dd", { text: "0.1.0" }),
      el("dt", { text: "Relay" }),
      el("dd", { text: relayHost(device.relay) }),
      el("dt", { text: "Protocol" }),
      el("dd", { text: "1" }),
    ),
  );
  out.push(el("p", { className: "back" }, el("a", { attrs: { href: "#/pair" }, text: "Pair another PC or phone" })));
}

/**
 * @param {string} id
 * @param {boolean} checked
 * @param {(on: boolean) => unknown} onChange
 * @returns {HTMLInputElement}
 */
function checkbox(id, checked, onChange) {
  const input = /** @type {HTMLInputElement} */ (el("input", { attrs: { id, type: "checkbox", checked } }));
  input.checked = checked;
  input.addEventListener("change", () => void onChange(input.checked));
  return input;
}

/**
 * The host part of a relay URL, for display.
 * @param {string} url
 * @returns {string}
 */
function relayHost(url) {
  try {
    return new URL(url.replace(/^ws/, "http")).host;
  } catch {
    return "unknown";
  }
}
