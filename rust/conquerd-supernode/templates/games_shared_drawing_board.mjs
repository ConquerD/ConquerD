export const LIMIT = 3000;
export function compareOps(a, b) {
  return a.clock - b.clock || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
}
export function validOp(o) {
  return (
    o &&
    /^[a-f0-9]{24}:\d{1,10}$/.test(o.id) &&
    Number.isSafeInteger(o.clock) &&
    o.clock > 0 &&
    o.clock < 1e12 &&
    (o.kind === "clear" ||
      (["ink", "erase"].includes(o.kind) &&
        ["x1", "y1", "x2", "y2"].every(
          (k) => Number.isFinite(o[k]) && o[k] >= 0 && o[k] <= 1,
        ) &&
        [2, 4, 8, 16].includes(o.width) &&
        /^#[a-fA-F0-9]{6}$/.test(o.color)))
  );
}
// A bounded operation set: deterministic order makes duplicate/reordered
// replay harmless. Clear is a tombstone, so late history cannot resurrect ink.
export class Board {
  constructor() {
    this.ops = new Map();
    this.clock = 0;
    this.clear = null;
    this.revision = 0;
  }
  add(op) {
    if (!validOp(op) || this.ops.has(op.id)) return false;
    this.clock = Math.max(this.clock, op.clock);
    if (this.clear && compareOps(op, this.clear) <= 0) return false;
    if (op.kind === "clear") {
      this.clear = op;
      for (const [id, old] of this.ops)
        if (compareOps(old, op) <= 0) this.ops.delete(id);
      if (this.ops.size >= LIMIT) this.ops.delete(this.sorted().at(-1).id);
    } else if (this.ops.size >= LIMIT) {
      // Keep the same earliest operations regardless of arrival order when
      // concurrent writers reach the cap. Clear still frees the whole board.
      const latest = this.sorted().at(-1);
      if (compareOps(op, latest) >= 0) return false;
      this.ops.delete(latest.id);
    }
    this.ops.set(op.id, op);
    this.revision++;
    return true;
  }
  sorted() {
    return [...this.ops.values()].sort(compareOps);
  }
  digest() {
    let hash = 2166136261;
    for (const op of this.sorted())
      for (const c of op.id) hash = Math.imul(hash ^ c.charCodeAt(0), 16777619);
    return `${this.ops.size}:${hash >>> 0}`;
  }
}
