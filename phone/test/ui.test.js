// The pairing screen's copy, by stage: the phone must not ask the person to compare digits the PC is
// not yet showing. The PC shows its code only after the phone's Confirm and Hello (pairing.rs), so the
// phone's first action is the send, and its own digits appear only while it waits for the PC's Allow.

import assert from "node:assert/strict";
import { test } from "node:test";
import { pairPrompt } from "../js/ui/pair.js";

const DIGITS = /\b\d{4}\b/;

test("the check stage asks for no digits and offers the send as its first action", () => {
  const p = pairPrompt({ stage: "check", check: "8216" });
  assert.equal(p.digits, null, "the phone's digits are not shown before the PC can show its own");
  assert.equal(p.action, "send");
  const text = p.lines.join(" ");
  assert.ok(!DIGITS.test(text), `no digits in the check stage text: ${text}`);
  assert.ok(!/digits? (match|do match)|the digits match/i.test(text), "no 'digits match' press before the send");
  assert.ok(!/read (these|the same) (four )?digits on the (PC|phone)/i.test(text), "the phone does not ask to read digits yet");
  assert.match(text, /Send to the PC/);
});

test("the waiting stage shows the phone's digits and says the PC shows the same ones", () => {
  const p = pairPrompt({ stage: "waiting", check: "8216" });
  assert.equal(p.digits, "8216");
  assert.equal(p.action, null, "no second press at the waiting stage");
  assert.match(p.lines.join(" "), /PC shows these same four digits, then press Allow on the PC/);
});

test("no stage before the waiting one shows digits", () => {
  for (const stage of ["connecting", "check"]) {
    assert.equal(pairPrompt({ stage, check: "1234" }).digits, null, stage);
  }
  assert.equal(pairPrompt(null).digits, null);
});

test("errors are shown as errors with their message", () => {
  const p = pairPrompt({ stage: "error", error: "The PC refused." });
  assert.equal(p.error, true);
  assert.deepEqual(p.lines, ["The PC refused."]);
});
