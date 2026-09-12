import { workspace } from "../../web-sdk/demo-workspace.mjs";
import { newGame, validGame, flip } from "./rules.mjs";

const symbols = ["☀", "☾", "★", "♠", "♣", "♥", "♦", "♫"];
const names = [
  "Sun",
  "Moon",
  "Star",
  "Spade",
  "Club",
  "Heart",
  "Diamond",
  "Music",
];
const grid = document.querySelector("#board");
for (let i = 0; i < 16; i++) {
  const button = document.createElement("button");
  button.className = "memory-card";
  button.onclick = () => {
    const next = flip(state.get("game"), i);
    if (next) state.set("game", next);
  };
  grid.append(button);
}
// The initial deal must be identical before any peer has published a change.
const { state } = workspace(
  "memory-match",
  {
    title: "Memory Match",
    category: "Puzzle · Cooperative",
    description:
      "Find eight pairs together. Everyone sees the same flips and can help remember the board.",
    controls: "Reveal two cards · Hide a missed pair to continue",
    detail:
      "Shared, repeated board snapshots let new guests catch up. Cards live in page memory, so this friendly demo is not an anti-cheat game. Concurrent flips resolve to one board.",
  },
  { game: newGame(() => 0.37) },
  (_, value) => validGame(value),
  (state, session) => {
    const game = state.get("game");
    [...grid.children].forEach((button, i) => {
      const matched = game.matched.includes(i),
        visible = matched || game.open.includes(i);
      button.className = `memory-card${matched ? " matched" : visible ? " revealed" : ""}`;
      button.textContent = visible ? symbols[game.cards[i]] : "?";
      button.setAttribute(
        "aria-label",
        `Card ${i + 1}: ${visible ? names[game.cards[i]] : "face down"}${matched ? ", matched" : ""}`,
      );
      button.disabled = !session.connected || visible || game.open.length === 2;
    });
    document.querySelector("#result").textContent =
      `${game.matched.length / 2} / 8 pairs · ${game.turns} attempts${game.matched.length === 16 ? " · All found!" : ""}`;
    document.querySelector("#hide").disabled =
      !session.connected || game.open.length !== 2;
    document.querySelector("#reset").disabled = !session.connected;
  },
);
document.querySelector("#hide").onclick = () =>
  state.set("game", { ...state.get("game"), open: [] });
document.querySelector("#reset").onclick = () => state.set("game", newGame());
