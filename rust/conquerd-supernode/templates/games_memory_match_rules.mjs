export function newGame(random = Math.random) {
  const cards = Array.from({ length: 16 }, (_, i) => i % 8);
  for (let i = cards.length - 1; i > 0; i--) {
    const j = Math.floor(random() * (i + 1));
    [cards[i], cards[j]] = [cards[j], cards[i]];
  }
  return { cards, matched: [], open: [], turns: 0 };
}
export function validGame(v) {
  const indexes = (a, max) =>
    Array.isArray(a) &&
    a.length <= max &&
    new Set(a).size === a.length &&
    a.every((n) => Number.isInteger(n) && n >= 0 && n < 16);
  return (
    !!v &&
    Array.isArray(v.cards) &&
    v.cards.length === 16 &&
    v.cards.every((n) => Number.isInteger(n) && n >= 0 && n < 8) &&
    Array.from(
      { length: 8 },
      (_, n) => v.cards.filter((c) => c === n).length === 2,
    ).every(Boolean) &&
    indexes(v.matched, 16) &&
    v.matched.length % 2 === 0 &&
    v.matched.every(
      (i) => v.matched.filter((j) => v.cards[j] === v.cards[i]).length === 2,
    ) &&
    indexes(v.open, 2) &&
    v.open.every((i) => !v.matched.includes(i)) &&
    (v.open.length < 2 || v.cards[v.open[0]] !== v.cards[v.open[1]]) &&
    Number.isSafeInteger(v.turns) &&
    v.turns >= v.matched.length / 2 &&
    v.turns <= 10000
  );
}
export function flip(value, index) {
  if (
    !Number.isInteger(index) ||
    index < 0 ||
    index > 15 ||
    value.open.length === 2 ||
    value.open.includes(index) ||
    value.matched.includes(index) ||
    value.turns === 10000
  )
    return null;
  const next = {
    ...value,
    open: [...value.open, index],
    matched: [...value.matched],
  };
  if (next.open.length === 2) {
    next.turns++;
    if (next.cards[next.open[0]] === next.cards[next.open[1]]) {
      next.matched.push(...next.open);
      next.open = [];
    }
  }
  return next;
}
