// @ts-check
// The phone's copy of the session list (design section 7.2, msg.rs `Event`). The rules:
//
// - A snapshot (one or more parts, all with one seq) replaces everything once its last part arrives.
//   Parts must come in order; anything else asks for a snapshot again.
// - A delta applies only when its seq is exactly the one after the last applied (`seq + 1`). A split
//   change arrives as several deltas with consecutive seqs, and each applies on its own.
// - The inbox event shares the seq counter, so a missed inbox event is a gap like any other.
// - A gap, or a delta before any snapshot, means: send `snapshot`. Nothing is guessed.

/** @typedef {import("./protocol.js").SessionInfo} SessionInfo */
/** @typedef {import("./protocol.js").ServerMsg} ServerMsg */

/**
 * @typedef {object} ApplyResult
 * @property {boolean} changed the list or the unread count changed
 * @property {boolean} resync the phone must ask for a snapshot
 */

export class SessionList {
  constructor() {
    /** @type {number | null} */
    this.seq = null;
    /** @type {Map<string, SessionInfo>} */
    this.sessions = new Map();
    /** @type {number} */
    this.unread = 0;
    /** @type {{ seq: number, parts: number, next: number, sessions: SessionInfo[], unread: number | undefined } | null} */
    this.pending = null;
  }

  /** Forgets everything: used when the channel is replaced. */
  reset() {
    this.seq = null;
    this.sessions = new Map();
    this.unread = 0;
    this.pending = null;
  }

  /**
   * @returns {SessionInfo[]} in list order (the order the host sent)
   */
  list() {
    return [...this.sessions.values()];
  }

  /**
   * @param {ServerMsg} ev an event (responses are not handled here)
   * @returns {ApplyResult}
   */
  apply(ev) {
    if (ev.kind !== "event") return { changed: false, resync: false };
    switch (ev.ev) {
      case "snapshot":
        return this.snapshot(ev);
      case "delta":
        return this.delta(ev);
      case "inbox":
        return this.inbox(ev);
      default:
        return { changed: false, resync: false };
    }
  }

  /**
   * @param {Extract<ServerMsg, { ev: "snapshot" }>} ev
   * @returns {ApplyResult}
   */
  snapshot(ev) {
    if (ev.part === 1) {
      // A new snapshot starts here; anything half-read before is dropped.
      this.pending = { seq: ev.seq, parts: ev.parts, next: 2, sessions: [...ev.sessions], unread: ev.unread };
    } else if (this.pending && this.pending.seq === ev.seq && this.pending.parts === ev.parts && ev.part === this.pending.next) {
      this.pending.sessions.push(...ev.sessions);
      this.pending.next += 1;
      if (ev.unread !== undefined) this.pending.unread = ev.unread;
    } else {
      this.pending = null;
      return { changed: false, resync: true };
    }
    if (!this.pending || this.pending.next <= this.pending.parts) return { changed: false, resync: false };
    const done = this.pending;
    this.pending = null;
    this.seq = done.seq;
    this.sessions = new Map(done.sessions.map((s) => [s.id, s]));
    this.unread = done.unread ?? this.unread;
    return { changed: true, resync: false };
  }

  /**
   * @param {Extract<ServerMsg, { ev: "delta" }>} ev
   * @returns {ApplyResult}
   */
  delta(ev) {
    if (!this.accepts(ev.seq)) return { changed: false, resync: true };
    this.seq = ev.seq;
    for (const id of ev.removed) this.sessions.delete(id);
    for (const s of ev.upserts) this.sessions.set(s.id, s);
    return { changed: true, resync: false };
  }

  /**
   * @param {Extract<ServerMsg, { ev: "inbox" }>} ev
   * @returns {ApplyResult}
   */
  inbox(ev) {
    if (!this.accepts(ev.seq)) return { changed: false, resync: true };
    this.seq = ev.seq;
    const changed = this.unread !== ev.unread;
    this.unread = ev.unread;
    return { changed, resync: false };
  }

  /**
   * Whether an incremental event with this seq applies now.
   * @param {number} seq
   * @returns {boolean}
   */
  accepts(seq) {
    if (this.pending) return false;
    return this.seq !== null && seq === this.seq + 1;
  }
}
