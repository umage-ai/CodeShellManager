// Rules the code must keep, checked on the source. Each one is a real risk for this app: untrusted
// text written as HTML, keys or offers in web storage, a page the production CSP would not allow,
// colour literals in screen code (the tokens rule), and the dev server's policy and path rules.

import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { DEV_CSP, fileFor } from "../tools/serve.mjs";
import { b64urlDecode, b64urlEncode, concat, ctEqual, hex, unhex } from "../js/bytes.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

/**
 * @param {string} dir
 * @returns {string[]}
 */
function filesUnder(dir) {
  /** @type {string[]} */
  const out = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) out.push(...filesUnder(p));
    else out.push(p);
  }
  return out;
}

/**
 * The lines of a source file that run: comment lines are left out, so a rule is about the code.
 * @param {string} file
 * @returns {string}
 */
function code(file) {
  return readFileSync(file, "utf8")
    .split("\n")
    .filter((line) => !/^\s*(\/\/|\/\*|\*)/.test(line))
    .join("\n");
}

const jsFiles = filesUnder(join(root, "js")).filter((f) => f.endsWith(".js"));
const uiFiles = filesUnder(join(root, "js", "ui")).filter((f) => f.endsWith(".js"));

test("no page code writes HTML from data: innerHTML, outerHTML, insertAdjacentHTML, document.write", () => {
  for (const f of [...jsFiles, join(root, "sw.js")]) {
    assert.doesNotMatch(code(f), /innerHTML|outerHTML|insertAdjacentHTML|document\.write|\beval\(|new Function\(/, relative(root, f));
  }
});

test("keys and offers never go to localStorage or sessionStorage", () => {
  for (const f of jsFiles) {
    assert.doesNotMatch(code(f), /localStorage|sessionStorage/, relative(root, f));
  }
});

test("screen code has no colour literals and no inline style: colours come from tokens", () => {
  for (const f of uiFiles) {
    const src = code(f);
    assert.doesNotMatch(src, /#[0-9a-fA-F]{3,8}\b|\brgba?\(|\bhsla?\(/, relative(root, f));
    assert.doesNotMatch(src, /\bstyle\s*[:=]/, `${relative(root, f)} sets an inline style`);
  }
});

test("the HTML has no inline script, style or event handler, so the production CSP allows it", () => {
  const html = readFileSync(join(root, "index.html"), "utf8");
  assert.doesNotMatch(html, /<script(?![^>]*\bsrc=)/i, "every script has a src");
  assert.doesNotMatch(html, /<style/i);
  assert.doesNotMatch(html, /\sstyle=/i);
  assert.doesNotMatch(html, /\son[a-z]+=/i);
});

test("the production policy (_headers) keeps the rules of design 10.1", () => {
  const h = readFileSync(join(root, "_headers"), "utf8");
  const csp = /Content-Security-Policy: (.*)/.exec(h)?.[1] ?? "";
  assert.match(csp, /default-src 'self'/);
  assert.match(csp, /connect-src 'self' wss: https:/);
  assert.match(csp, /script-src 'self'(;|$)/);
  assert.match(csp, /style-src 'self'(;|$)/);
  assert.doesNotMatch(csp, /unsafe|\*|ws:\/\/|http:\/\//);
});

test("the dev policy allows the loopback relay and nothing else added", () => {
  assert.match(DEV_CSP, /connect-src 'self' ws:\/\/127\.0\.0\.1:\*(;|$)/);
  assert.doesNotMatch(DEV_CSP, /wss:|https:/);
  assert.doesNotMatch(DEV_CSP, /unsafe/);
});

test("the manifest is valid JSON with an installable name and standalone display", () => {
  const m = JSON.parse(readFileSync(join(root, "manifest.webmanifest"), "utf8"));
  assert.equal(m.display, "standalone");
  assert.equal(m.name, "CodeShellManager Phone");
  assert.ok(Array.isArray(m.icons) && m.icons.length > 0);
});

test("the service worker is a classic script that imports only the notification text", () => {
  const sw = readFileSync(join(root, "sw.js"), "utf8");
  assert.doesNotMatch(sw, /^\s*import\s/m);
  assert.doesNotMatch(sw, /^\s*export\s/m);
  assert.deepEqual([...sw.matchAll(/importScripts\("([^"]+)"\)/g)].map((m) => m[1]), ["/js/push-text.js"]);
});

test("every shell file the worker caches exists", () => {
  const sw = readFileSync(join(root, "sw.js"), "utf8");
  const shell = /const SHELL = \[([\s\S]*?)\];/.exec(sw)?.[1] ?? "";
  for (const m of shell.matchAll(/"([^"]+)"/g)) {
    assert.ok(statSync(join(root, m[1])).isFile(), `${m[1]} is missing`);
  }
});

test("the page's module graph is only local files", () => {
  for (const f of jsFiles) {
    for (const m of readFileSync(f, "utf8").matchAll(/from\s+"([^"]+)"/g)) {
      assert.ok(m[1].startsWith("./") || m[1].startsWith("../"), `${relative(root, f)} imports ${m[1]}`);
    }
  }
});

test("no test or tool folder is served", () => {
  assert.equal(fileFor("/test/noise.test.js"), null);
  assert.equal(fileFor("/tools/serve.mjs"), null);
  assert.equal(fileFor("/node_modules/typescript/package.json"), null);
});

test("the dev server never serves a path outside its folder", () => {
  assert.equal(fileFor("/../package.json"), null);
  assert.equal(fileFor("/%2e%2e/package.json"), null);
  assert.equal(fileFor("/js/%00.js"), null);
  assert.equal(fileFor("/v/0.1.0/"), join(root, "index.html"));
  assert.equal(fileFor("/"), join(root, "index.html"));
  assert.equal(fileFor("/js/app.js"), join(root, "js", "app.js"));
});

test("the byte helpers: canonical base64url only, hex and constant-time equality", () => {
  const bytes = Uint8Array.from([0, 1, 2, 250, 251, 252, 253, 254, 255]);
  assert.deepEqual(b64urlDecode(b64urlEncode(bytes)), bytes);
  assert.equal(b64urlDecode("AAE="), null, "padding is not part of the spelling");
  assert.equal(b64urlDecode("AA+/"), null, "standard base64 is not accepted");
  assert.equal(b64urlDecode("A"), null);
  assert.equal(hex(unhex("00ff10")), "00ff10");
  assert.throws(() => unhex("abc"));
  assert.throws(() => unhex("zz"));
  assert.deepEqual(concat(Uint8Array.of(1), Uint8Array.of(2, 3)), Uint8Array.of(1, 2, 3));
  assert.equal(ctEqual(Uint8Array.of(1, 2), Uint8Array.of(1, 2)), true);
  assert.equal(ctEqual(Uint8Array.of(1, 2), Uint8Array.of(1, 3)), false);
  assert.equal(ctEqual(Uint8Array.of(1), Uint8Array.of(1, 0)), false);
});
