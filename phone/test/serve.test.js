// The dev server over a real loopback socket: the page at /v/<version>/, the dev policy on every
// answer, a 404 for a test or tool path, and nothing listening anywhere but 127.0.0.1.

import assert from "node:assert/strict";
import { test } from "node:test";
import { DEV_CSP, start } from "../tools/serve.mjs";

/**
 * @param {string} path
 * @param {number} port
 */
async function get(path, port) {
  const res = await fetch(`http://127.0.0.1:${port}${path}`, { redirect: "manual" });
  return { status: res.status, csp: res.headers.get("content-security-policy"), type: res.headers.get("content-type"), body: await res.text() };
}

test("the dev server serves the page, the app's files, and the dev policy, on loopback only", async () => {
  const server = start(0);
  await new Promise((r) => server.once("listening", r));
  const { port } = /** @type {import("node:net").AddressInfo} */ (server.address());
  try {
    const page = await get("/v/0.1.0/", port);
    assert.equal(page.status, 200);
    assert.match(page.type ?? "", /text\/html/);
    assert.match(page.body, /<title>CodeShellManager Phone<\/title>/);
    assert.equal(page.csp, DEV_CSP);

    const js = await get("/js/app.js", port);
    assert.equal(js.status, 200);
    assert.match(js.type ?? "", /javascript/);

    const missing = await get("/js/nope.js", port);
    assert.equal(missing.status, 404);
    assert.equal(missing.csp, DEV_CSP, "every answer carries the policy");

    const testFile = await get("/test/noise.test.js", port);
    assert.equal(testFile.status, 404);
  } finally {
    server.close();
  }
});

test("the dev server listens on 127.0.0.1 only", async () => {
  const server = start(0);
  await new Promise((r) => server.once("listening", r));
  try {
    assert.equal(/** @type {import("node:net").AddressInfo} */ (server.address()).address, "127.0.0.1");
  } finally {
    server.close();
  }
});
