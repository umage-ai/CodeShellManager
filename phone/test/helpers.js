// Test helpers: the shared vectors (copies of csm-remote-proto's vectors/, checked by the Rust test
// `tests/vectors.rs`), and small utilities. Node only.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

/**
 * @param {string} name e.g. "noise"
 * @returns {any}
 */
export function vector(name) {
  return JSON.parse(readFileSync(join(here, "fixtures", `${name}.json`), "utf8"));
}

/** @param {string} path */
export function readText(path) {
  return readFileSync(join(here, "..", path), "utf8");
}
