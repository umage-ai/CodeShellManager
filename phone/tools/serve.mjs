// Static server for the phone app on 127.0.0.1 (design section 11). No dependencies.
//
//   node tools/serve.mjs [port]          (default 8080; the app is then at http://127.0.0.1:8080/)
//
// It sends the DEV content security policy (design 10.1): the only place `ws://127.0.0.1` is allowed,
// so the page can reach a relay on the loopback address. Production hosts send the production policy
// (`_headers`). Paths: `/v/<anything>/` is the app page, as an offer link has it. Only files under
// this folder are served; anything else is a 404. It listens on 127.0.0.1 only.

import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(fileURLToPath(new URL("..", import.meta.url)));
const HOST = "127.0.0.1";

export const DEV_CSP = [
  "default-src 'self'",
  "connect-src 'self' ws://127.0.0.1:*",
  "img-src 'self' data:",
  "style-src 'self'",
  "script-src 'self'",
  "worker-src 'self'",
  "manifest-src 'self'",
  "base-uri 'self'",
  "form-action 'none'",
  "frame-ancestors 'none'",
].join("; ");

/** @type {Record<string, string>} */
const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".webmanifest": "application/manifest+json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

/**
 * Maps a request path to a file under ROOT, or null when it is not allowed.
 * @param {string} urlPath
 * @returns {string | null}
 */
export function fileFor(urlPath) {
  let p;
  try {
    p = decodeURIComponent(urlPath.split("?")[0].split("#")[0]);
  } catch {
    return null;
  }
  if (p.includes("\0")) return null;
  // The app page: /v/<version>/ and anything under it that is not a file of the app.
  if (/^\/v\/[^/]+\/?$/.test(p)) p = "/index.html";
  if (p === "/") p = "/index.html";
  // Test and tool folders are not served.
  if (/^\/(test|tools|node_modules)(\/|$)/.test(p)) return null;
  const full = normalize(join(ROOT, p));
  if (full !== ROOT && !full.startsWith(ROOT + sep)) return null;
  return full;
}

/**
 * @param {import("node:http").IncomingMessage} req
 * @param {import("node:http").ServerResponse} res
 */
async function handle(req, res) {
  if (req.method !== "GET" && req.method !== "HEAD") {
    res.writeHead(405, { Allow: "GET, HEAD" }).end();
    return;
  }
  const file = fileFor(req.url ?? "/");
  if (!file) {
    res.writeHead(404, headers("text/plain; charset=utf-8")).end("not found");
    return;
  }
  try {
    const info = await stat(file);
    if (!info.isFile()) throw new Error("not a file");
    const body = await readFile(file);
    res.writeHead(200, headers(TYPES[extname(file)] ?? "application/octet-stream"));
    res.end(req.method === "HEAD" ? undefined : body);
  } catch {
    res.writeHead(404, headers("text/plain; charset=utf-8")).end("not found");
  }
}

/**
 * @param {string} type
 * @returns {Record<string, string>}
 */
function headers(type) {
  return {
    "Content-Type": type,
    "Content-Security-Policy": DEV_CSP,
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "no-referrer",
    "Cache-Control": "no-cache",
  };
}

/**
 * @param {number} port
 */
export function start(port) {
  const server = createServer((req, res) => void handle(req, res));
  server.listen(port, HOST, () => {
    process.stdout.write(`CodeShellManager phone app: http://${HOST}:${port}/ (dev policy, loopback only)\n`);
  });
  return server;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const port = Number(process.argv[2] ?? 8080);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    process.stderr.write("port must be 1 to 65535\n");
    process.exit(2);
  }
  start(port);
}
