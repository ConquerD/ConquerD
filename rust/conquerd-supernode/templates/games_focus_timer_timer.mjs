export function newTimer(seconds = 1500) {
  return { duration: seconds, remaining: seconds, end: 0 };
}
export function validTimer(v) {
  return (
    !!v &&
    Number.isInteger(v.duration) &&
    v.duration >= 60 &&
    v.duration <= 3600 &&
    Number.isInteger(v.remaining) &&
    v.remaining >= 0 &&
    v.remaining <= v.duration &&
    Number.isSafeInteger(v.end) &&
    v.end >= 0 &&
    v.end <= 8640000000000000
  );
}
export function remaining(v, now = Date.now()) {
  return v.end
    ? Math.max(0, Math.min(v.duration, Math.ceil((v.end - now) / 1000)))
    : v.remaining;
}
export function toggle(v, now = Date.now()) {
  const seconds = remaining(v, now);
  return v.end
    ? { ...v, remaining: seconds, end: 0 }
    : {
        ...v,
        remaining: seconds || v.duration,
        end: now + (seconds || v.duration) * 1000,
      };
}
