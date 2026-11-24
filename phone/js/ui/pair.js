// @ts-check
// The pairing screen (design 10.5, 1). Paste the link, or scan the PC's code where the browser can
// read QR codes. The order follows the protocol: this phone sends its confirmation and name first;
// the PC then shows the check code, and the person compares it with the digits this phone shows
// while it waits, and allows there. The digits are never asked for before the PC can show them.

import { el, field } from "./dom.js";

/** The text typed so far, kept across re-renders. */
let draft = "";
/** The name typed so far. */
let draftName = "";

/**
 * A camera read in progress (BarcodeDetector). Kept outside the render so a redraw keeps it.
 * @type {{ stream: MediaStream, timer: ReturnType<typeof setInterval> } | null}
 */
let scanning = null;

/**
 * What the pairing screen says at each stage, as data, so the order is testable without a DOM.
 * `digits` is set only in the waiting stage, after `Confirm` and `Hello` have reached the PC.
 * @param {{ stage: string, check?: string, error?: string } | null} pairing
 * @returns {{ lines: string[], digits: string | null, action: "send" | null, error: boolean }}
 */
export function pairPrompt(pairing) {
  if (!pairing) return { lines: [], digits: null, action: null, error: false };
  if (pairing.stage === "connecting") return { lines: ["Connecting to the PC…"], digits: null, action: null, error: false };
  if (pairing.stage === "check") {
    return {
      lines: [
        "The PC is waiting. Press Send to the PC to send this phone's name.",
        "The PC then shows a four-digit check code. Compare it with the digits on the PC, and press Allow there only when they match.",
      ],
      digits: null,
      action: "send",
      error: false,
    };
  }
  if (pairing.stage === "waiting") {
    return {
      lines: ["Check that the PC shows these same four digits, then press Allow on the PC."],
      digits: pairing.check ?? null,
      action: null,
      error: false,
    };
  }
  return { lines: [pairing.error ?? "Pairing failed."], digits: null, action: null, error: true };
}

/**
 * The name the phone is paired with by default: the browser's platform, not a personal name.
 * @returns {string}
 */
function defaultName() {
  const nav = /** @type {{ userAgentData?: { platform?: string } }} */ (/** @type {unknown} */ (globalThis.navigator ?? {}));
  const platform = nav.userAgentData?.platform ?? "Phone";
  return `${platform} phone`.slice(0, 60);
}

/**
 * @param {HTMLElement[]} out
 * @param {any} ctx
 */
export function renderPair(out, ctx) {
  const pairing = ctx.state.pairing;
  out.push(el("h2", { text: "Pair a phone" }));
  if (!pairing) {
    out.push(
      el("p", {
        text: "On the PC, open Phone remote and press Start pairing. Then scan the code with this phone, or paste the link here.",
      }),
    );
    out.push(formView(ctx));
    return;
  }
  const prompt = pairPrompt(pairing);
  if (prompt.error) {
    out.push(el("p", { className: "error", text: prompt.lines[0], attrs: { role: "alert" } }));
    out.push(el("div", { className: "actions" }, el("button", { className: "primary", text: "Try again", on: { click: () => ctx.pairCancel() } })));
    return;
  }
  if (pairing.stage === "connecting") {
    out.push(el("p", { text: prompt.lines[0], attrs: { role: "status" } }));
    return;
  }
  for (const line of prompt.lines) out.push(el("p", { text: line, attrs: prompt.digits === null ? {} : { role: "status" } }));
  if (prompt.digits !== null) {
    out.push(el("p", { className: "check-code", text: prompt.digits, attrs: { "aria-label": `Check code ${prompt.digits.split("").join(" ")}` } }));
  }
  if (prompt.action === "send") {
    out.push(
      el(
        "div",
        { className: "actions" },
        el("button", { className: "primary", text: "Send to the PC", on: { click: () => void ctx.pairConfirm() } }),
        el("button", { text: "Cancel", on: { click: () => ctx.pairCancel() } }),
      ),
    );
  }
}

/**
 * The paste-and-connect form.
 * @param {any} ctx
 * @returns {HTMLElement}
 */
function formView(ctx) {
  if (!draft && ctx.state.pendingOffer) draft = ctx.state.pendingOffer;
  if (!draftName) draftName = defaultName();
  const area = /** @type {HTMLTextAreaElement} */ (
    el("textarea", {
      attrs: { id: "offer", rows: "4", "aria-label": "The link or code from the PC", spellcheck: "false", autocomplete: "off" },
      on: { input: (e) => (draft = /** @type {HTMLTextAreaElement} */ (e.target).value) },
    })
  );
  area.value = draft;
  const name = field("Name for this phone", { id: "phone-name", value: draftName, maxLength: 64, required: true });
  name.addEventListener("input", () => (draftName = name.value));
  const connect = () => {
    ctx.state.pendingOffer = "";
    void ctx.pairStart(draft, draftName);
    draft = "";
  };
  const g = /** @type {any} */ (globalThis);
  const canScan = typeof g.BarcodeDetector === "function" && !!g.navigator?.mediaDevices?.getUserMedia;
  const video = el("video", { className: "scan", attrs: { playsinline: true, muted: true, hidden: !scanning, "aria-label": "Camera for the PC's code" } });
  if (scanning) {
    /** @type {HTMLVideoElement} */ (video).srcObject = scanning.stream;
    void /** @type {HTMLVideoElement} */ (video).play().catch(() => undefined);
  }
  return el(
    "section",
    { className: "stack" },
    el("label", { attrs: { for: "offer" }, text: "Link or code from the PC" }),
    area,
    name,
    el(
      "div",
      { className: "actions" },
      el("button", { className: "primary", text: "Connect", attrs: { disabled: draft.trim().length === 0 }, on: { click: connect } }),
      canScan ? el("button", { text: scanning ? "Stop scanning" : "Scan the code", on: { click: () => (scanning ? stopScan(ctx) : void startScan(ctx)) } }) : null,
    ),
    canScan ? video : null,
    el("p", { className: "muted", text: "The code is good for ten minutes and works once. Nothing but the secure handshake is sent until you press Send to the PC." }),
  );
}

/**
 * Reads QR codes from the camera until one is found, then uses it as the offer.
 * @param {any} ctx
 */
async function startScan(ctx) {
  try {
    const stream = await globalThis.navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" }, audio: false });
    const Detector = /** @type {any} */ (globalThis).BarcodeDetector;
    const detector = new Detector({ formats: ["qr_code"] });
    const timer = setInterval(() => {
      const video = document.querySelector("video.scan");
      if (!(video instanceof HTMLVideoElement) || video.readyState < 2) return;
      detector
        .detect(video)
        .then((/** @type {Array<{ rawValue?: string }>} */ codes) => {
          const raw = codes[0]?.rawValue;
          if (raw && scanning) {
            stopScan(ctx);
            draft = raw;
            ctx.state.pendingOffer = "";
            void ctx.pairStart(raw, draftName || defaultName());
          }
        })
        .catch(() => undefined);
    }, 300);
    scanning = { stream, timer };
    ctx.notice("");
  } catch {
    ctx.notice("The camera is not available. Paste the link instead.");
  }
}

/**
 * @param {any} ctx
 */
function stopScan(ctx) {
  if (!scanning) return;
  clearInterval(scanning.timer);
  for (const t of scanning.stream.getTracks()) t.stop();
  scanning = null;
  ctx.notice("");
}
