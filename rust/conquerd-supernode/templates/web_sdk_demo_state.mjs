// Small, ephemeral last-writer-wins registers. Fixed keys bound memory; repeated
// snapshots repair dropped datagrams and populate late arrivals without a server.
export class SharedState {
  constructor(session, defaults, validate, changed = () => {}) {
    this.session = session;
    this.validate = validate;
    this.changed = changed;
    this.records = new Map(
      Object.entries(defaults).map(([key, value]) => [
        key,
        { key, clock: 0, by: "", value },
      ]),
    );
    session.on("data", (message) => this.merge(message));
    this.cursor = 0;
  }
  get(key) {
    return this.records.get(key)?.value;
  }
  merge(record) {
    if (
      !record ||
      !this.records.has(record.key) ||
      !Number.isSafeInteger(record.clock) ||
      record.clock < 1 ||
      record.clock > 1e12 ||
      !/^[a-f0-9]{24}$/.test(record.by) ||
      !this.validate(record.key, record.value)
    )
      return false;
    const old = this.records.get(record.key);
    if (
      record.clock < old.clock ||
      (record.clock === old.clock && record.by <= old.by)
    )
      return false;
    const clean = {
      key: record.key,
      clock: record.clock,
      by: record.by,
      value: record.value,
    };
    if (new TextEncoder().encode(JSON.stringify(clean)).length > 850)
      return false;
    this.records.set(record.key, JSON.parse(JSON.stringify(clean)));
    this.changed(record.key);
    return true;
  }
  set(key, value) {
    if (!this.session.connected || !this.records.has(key)) return false;
    const record = {
      key,
      clock: this.records.get(key).clock + 1,
      by: this.session.id,
      value,
    };
    if (!this.merge(record)) return false;
    this.session.send(record);
    return true;
  }
  tick() {
    const records = [...this.records.values()];
    const record = records[this.cursor++ % records.length];
    if (record?.clock) this.session.send(record);
  }
}
