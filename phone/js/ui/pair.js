// @ts-check
// The pairing screen (design 10.5, 1). Paste the link, or scan the PC's code where the browser can
// read QR codes. Then the check code: the person reads the same four digits on the PC, and only
// then confirms. The PC shows the request; this screen waits for its answer.

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
  if (pairing.stage === "connecting") {
    out.push(el("p", { text: "Connecting to the PC…", attrs: { role: "status" } }));
    return;
  }
  if (pairing.stage === "check") {
    out.push(el("p", { text: "Read these four digits on the PC. If they differ, do not continue." }));
    out.push(el("p", { className: "check-code", text: pairing.check ?? "", attrs: { "aria-label": `Check code ${(pairing.check ?? "").split("").join(" ")}` } }));
    out.push(
      el(
        "div",
        { className: "actions" },
        el("button", { className: "primary", text: "The digits match", on: { click: () => void ctx.pairConfirm() } }),
        el("button", { text: "Cancel", on: { click: () => ctx.pairCancel() } }),
      ),
    );
    return;
  }
  if (pairing.stage === "waiting") {
    out.push(el("p", { text: "Waiting for the PC to allow this phone. Check the PC.", attrs: { role: "status" } }));
    if (pairing.check) out.push(el("p", { className: "muted", text: `Check code ${pairing.check}` }));
    return;
  }
  out.push(el("p", { className: "error", text: pairing.error ?? "Pairing failed.", attrs: { role: "alert" } }));
  out.push(el("div", { className: "actions" }, el("button", { className: "primary", text: "Try again", on: { click: () => ctx.pairCancel() } })));
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
    el("p", { className: "muted", text: "The code is good for ten minutes and works once. Nothing is sent to the PC until you confirm the digits." }),
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
