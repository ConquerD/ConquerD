export const defaults = Object.fromEntries(
  Array.from({ length: 8 }, (_, i) => [`task${i}`, { text: "", done: false }]),
);
export function validTask(value) {
  return (
    !!value &&
    typeof value.text === "string" &&
    value.text.length <= 80 &&
    typeof value.done === "boolean"
  );
}
