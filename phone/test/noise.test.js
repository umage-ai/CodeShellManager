// Noise over WebCrypto against csm-remote-proto's vectors/noise.json: every handshake byte, the
// transport, the handshake hash, the check code, and the rekey schedule (10 000 messages).

import assert from "node:assert/strict";
import { test } from "node:test";
import { concat, hex, unhex } from "../js/bytes.js";
import { checkCode } from "../js/pairing.js";
import { keypairFromSeed, publicOf, dh } from "../js/crypto.js";
import { Handshake, PARAMS_PAIRING, PARAMS_SESSION, Transport, REKEY_INTERVAL } from "../js/noise.js";
import { sha256 } from "../js/crypto.js";
import { vector } from "./helpers.js";

const noise = vector("noise");

/**
 * Builds both sides of one vector with its fixed keys and ephemerals.
 * @param {any} v
 */
async function sides(v) {
  const is = await keypairFromSeed(unhex(v.initiator_static_private_hex));
  const rs = await keypairFromSeed(unhex(v.responder_static_private_hex));
  const ei = await keypairFromSeed(unhex(v.initiator_ephemeral_private_hex));
  const er = await keypairFromSeed(unhex(v.responder_ephemeral_private_hex));
  const prologue = unhex(v.prologue_hex);
  const psk = v.psk_hex ? unhex(v.psk_hex) : null;
  const init = new Handshake({ role: "initiator", params: v.params, prologue, s: is, rs: rs.publicKey, psk, e: ei });
  const resp = new Handshake({ role: "responder", params: v.params, prologue, s: rs, psk, e: er });
  return { is, rs, init, resp };
}

for (const name of ["pairing", "session"]) {
  const v = noise[name];

  test(`keys derive their public halves (${name})`, async () => {
    const is = await keypairFromSeed(unhex(v.initiator_static_private_hex));
    assert.equal(hex(is.publicKey), v.initiator_static_public_hex);
    const rs = await keypairFromSeed(unhex(v.responder_static_private_hex));
    assert.equal(hex(rs.publicKey), v.responder_static_public_hex);
    assert.equal(hex(await publicOf(is.privateKey)), v.initiator_static_public_hex);
  });

  test(`handshake bytes match the vector (${name})`, async () => {
    const { init, resp } = await sides(v);
    const msg1 = await init.write(unhex(v.message1_payload_hex));
    assert.equal(hex(msg1), v.message1_hex);
    assert.equal(hex(await resp.read(msg1)), v.message1_payload_hex);
    const msg2 = await resp.write(unhex(v.message2_payload_hex));
    assert.equal(hex(msg2), v.message2_hex);
    assert.equal(hex(await init.read(msg2)), v.message2_payload_hex);
    assert.equal(init.done && resp.done, true);
  });

  test(`handshake hash and check code match (${name})`, async () => {
    const { init, resp } = await sides(v);
    await resp.read(await init.write(unhex(v.message1_payload_hex)));
    await init.read(await resp.write(unhex(v.message2_payload_hex)));
    const a = await init.finish();
    const b = await resp.finish();
    assert.equal(hex(a.hash), v.handshake_hash_hex);
    assert.equal(hex(b.hash), v.handshake_hash_hex);
    assert.equal(await checkCode(a.hash), v.check_code);
    assert.equal(await checkCode(b.hash), v.check_code);
  });

  test(`transport bytes match the vector (${name})`, async () => {
    const { init, resp } = await sides(v);
    await resp.read(await init.write(unhex(v.message1_payload_hex)));
    await init.read(await resp.write(unhex(v.message2_payload_hex)));
    const i = await init.finish();
    const r = await resp.finish();
    for (const t of v.transport) {
      const pt = unhex(t.plaintext_hex);
      if (t.from === "initiator") {
        assert.equal(hex(await i.send.seal(pt)), t.ciphertext_hex);
        assert.equal(hex(await r.recv.open(unhex(t.ciphertext_hex))), t.plaintext_hex);
      } else {
        assert.equal(hex(await r.send.seal(pt)), t.ciphertext_hex);
        assert.equal(hex(await i.recv.open(unhex(t.ciphertext_hex))), t.plaintext_hex);
      }
    }
  });
}

test("a handshake message with a flipped byte is refused", async () => {
  const v = noise.session;
  const { init, resp } = await sides(v);
  const msg1 = await init.write(unhex(v.message1_payload_hex));
  const bad = msg1.slice();
  bad[bad.length - 1] ^= 1;
  await assert.rejects(resp.read(bad));
});

test("a truncated handshake message is refused, never read past its end", async () => {
  const v = noise.session;
  const { init, resp } = await sides(v);
  const msg1 = await init.write(unhex(v.message1_payload_hex));
  await assert.rejects(resp.read(msg1.subarray(0, 40)));
});

test("an all-zero DH output is refused (low-order peer key)", async () => {
  const k = await keypairFromSeed(unhex(noise.session.initiator_static_private_hex));
  await assert.rejects(dh(k.privateKey, new Uint8Array(32)));
});

test("the rekey schedule matches the vector: every ciphertext, both ends rekey after message N", async () => {
  // The pairing vector's own keys and payloads: the handshake hash, and so every key, is the vector's.
  const v = noise.pairing;
  const { init, resp } = await sides(v);
  await resp.read(await init.write(unhex(v.message1_payload_hex)));
  await init.read(await resp.write(unhex(v.message2_payload_hex)));
  const fi = await init.finish();
  const fr = await resp.finish();
  // The vector's script runs first (five messages, both ways), then the rekey run. The generator
  // rekeys by hand after message N of the run, so the Transports here do not rekey on their own.
  const ti = new Transport(fi.send.cipher, Number.MAX_SAFE_INTEGER);
  const tr = new Transport(fr.recv.cipher, Number.MAX_SAFE_INTEGER);
  const back = new Transport(fr.send.cipher, Number.MAX_SAFE_INTEGER);
  const forth = new Transport(fi.recv.cipher, Number.MAX_SAFE_INTEGER);
  for (const t of noise.pairing.transport) {
    if (t.from === "initiator") {
      assert.equal(hex(await ti.seal(unhex(t.plaintext_hex))), t.ciphertext_hex);
      await tr.open(unhex(t.ciphertext_hex));
    } else {
      assert.equal(hex(await back.seal(unhex(t.plaintext_hex))), t.ciphertext_hex);
      await forth.open(unhex(t.ciphertext_hex));
    }
  }
  const total = noise.pairing.rekey.messages_sent;
  assert.equal(REKEY_INTERVAL, noise.pairing.rekey.interval);
  const around = new Map(noise.pairing.rekey.around_boundary.map(/** @param {any} x */ (x) => [x.index, x]));
  /** @type {Uint8Array[]} */
  const all = [];
  for (let i = 1; i <= total; i++) {
    const plain = new TextEncoder().encode(`m${i}`);
    const ct = await ti.cipher.encrypt(new Uint8Array(0), plain);
    assert.equal(new TextDecoder().decode(await tr.cipher.decrypt(new Uint8Array(0), ct)), `m${i}`);
    const expect = around.get(i);
    if (expect) assert.equal(hex(ct), expect.ciphertext_hex, `ciphertext ${i}`);
    all.push(ct);
    if (i === REKEY_INTERVAL) {
      await ti.cipher.rekey();
      await tr.cipher.rekey();
    }
  }
  assert.equal(hex(await sha256(concat(...all))), noise.pairing.rekey.all_ciphertexts_sha256_hex);
});

test("a Transport built with a small interval rekeys on both ends at the same message", async () => {
  // Same keys on both sides: a message sealed after a rekey opens only after the same rekey.
  const v = noise.session;
  const { init, resp } = await sides(v);
  await resp.read(await init.write(new Uint8Array(0)));
  await init.read(await resp.write(new Uint8Array(0)));
  const a = await init.finish();
  const b = await resp.finish();
  const send = new Transport(a.send.cipher, 3);
  const recv = new Transport(b.recv.cipher, 3);
  for (let i = 0; i < 10; i++) {
    const pt = Uint8Array.of(i);
    assert.deepEqual(await recv.open(await send.seal(pt)), pt);
  }
});

test("the IK pattern is used for a session and IKpsk2 for pairing", () => {
  assert.equal(PARAMS_PAIRING, noise.pairing.params);
  assert.equal(PARAMS_SESSION, noise.session.params);
});

test("the schedule rekeys after message N of the direction, counted from the first transport message", async () => {
  // Product rule (host.rs `send_msg`, csm-remote-proto `noise.rs`): rekey after every N-th message.
  // Checked by hand: after three messages, the fourth is sealed under the rekeyed key.
  const v = noise.session;
  const { init, resp } = await sides(v);
  await resp.read(await init.write(new Uint8Array(0)));
  await init.read(await resp.write(new Uint8Array(0)));
  const a = await init.finish();
  const b = await resp.finish();
  const auto = new Transport(a.send.cipher, 3);
  const pt = Uint8Array.of(7);
  const first = [];
  for (let i = 0; i < 3; i++) first.push(await auto.seal(pt));
  const fourth = await auto.seal(pt);
  // A manual reference cipher: the same keys, the same nonces, rekeyed by hand after message three.
  const ref = await keyedClone(a, v);
  for (let i = 0; i < 3; i++) assert.deepEqual(await ref.encrypt(new Uint8Array(0), pt), first[i]);
  await ref.rekey();
  assert.deepEqual(await ref.encrypt(new Uint8Array(0), pt), fourth);
  assert.ok(b.recv);
});

/**
 * A second, independent send cipher with the same starting key as `fin.send` (a fresh handshake).
 * @param {any} _fin
 * @param {any} v
 */
async function keyedClone(_fin, v) {
  const { init, resp } = await sides(v);
  await resp.read(await init.write(new Uint8Array(0)));
  await init.read(await resp.write(new Uint8Array(0)));
  return (await init.finish()).send.cipher;
}
