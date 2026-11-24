// @ts-check
// A small DOM builder. Text always goes in as textContent: session names, folders and messages from
// the PC are untrusted, so nothing here parses HTML and nothing uses innerHTML. No inline styles
// (the CSP forbids them); layout is in css/app.css.

/**
 * @typedef {string | number | Node | null | undefined | false | Array<unknown>} Child
 */

/**
 * @param {string} tag
 * @param {{ className?: string, text?: string, attrs?: Record<string, string | boolean>, on?: Record<string, (e: Event) => void>, props?: Record<string, unknown> }} [o]
 * @param {...Child} children
 * @returns {HTMLElement}
 */
export function el(tag, o = {}, ...children) {
  const node = document.createElement(tag);
  if (o.className) node.className = o.className;
  if (o.text !== undefined) node.textContent = o.text;
  for (const [k, v] of Object.entries(o.attrs ?? {})) {
    if (v === false) continue;
    node.setAttribute(k, v === true ? "" : v);
  }
  for (const [k, v] of Object.entries(o.on ?? {})) node.addEventListener(k, v);
  for (const [k, v] of Object.entries(o.props ?? {})) /** @type {Record<string, unknown>} */ (/** @type {unknown} */ (node))[k] = v;
  append(node, children);
  return node;
}

/**
 * @param {Node} parent
 * @param {any[]} children
 */
export function append(parent, children) {
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    if (Array.isArray(c)) append(parent, c);
    else if (typeof c === "string" || typeof c === "number") parent.appendChild(document.createTextNode(String(c)));
    else parent.appendChild(c);
  }
}

/**
 * Replaces everything in `node` with `children`.
 * @param {HTMLElement} node
 * @param {Child[]} children
 */
export function replace(node, children) {
  node.replaceChildren();
  append(node, children);
}

/**
 * A labelled text field. Returns the input.
 * @param {string} label
 * @param {{ id: string, type?: string, value?: string, maxLength?: number, required?: boolean, autocomplete?: string }} o
 * @returns {HTMLInputElement}
 */
export function field(label, o) {
  const input = /** @type {HTMLInputElement} */ (
    el("input", { attrs: { id: o.id, type: o.type ?? "text", maxlength: o.maxLength ? String(o.maxLength) : false, required: o.required ?? false, autocomplete: o.autocomplete ?? "off" } })
  );
  input.value = o.value ?? "";
  return input;
}

/**
 * A relative time for a Unix-seconds stamp ("5 min ago").
 * @param {number | undefined} since
 * @param {number} now milliseconds
 * @returns {string}
 */
export function ago(since, now) {
  if (since === undefined) return "";
  const secs = Math.max(0, Math.floor(now / 1000) - since);
  if (secs < 60) return "just now";
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins} min`;
  const hours = Math.floor(mins / 60);
  if (hours < 48) return `${hours} h`;
  return `${Math.floor(hours / 24)} days`;
}
