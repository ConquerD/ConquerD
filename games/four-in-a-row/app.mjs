import { workspace } from "../../web-sdk/demo-workspace.mjs";
import { newGame, validGame, boardFor, winner, drop } from "./rules.mjs";

const grid = document.querySelector("#board");
const controls = document.querySelector("#columns");
for (let i = 0; i < 42; i++) {
  const cell = document.createElement("span");
  cell.className = "disc";
  grid.append(cell);
}
for (let i = 0; i < 7; i++) {
  const button = document.createElement("button");
  button.textContent = "↓";
  button.setAttribute("aria-label", `Drop in column ${i + 1}`);
  button.onclick = () => {
    const next = drop(state.get("game"), i);
    if (next) state.set("game", next);
  };
  controls.append(button);
}
const { state } = workspace(
  "four-in-a-row",
  {
    title: "Four in a Row",
    category: "Tabletop · Shared board",
    description:
      "Take turns dropping discs. Connect four horizontally, vertically or diagonally.",
    controls: "Choose a column · Mint and lilac alternate",
    detail:
      "An open tabletop: anyone in the room can play either side. Concurrent moves resolve to one shared board; play again if your move is replaced.",
  },
  { game: newGame() },
  (_, value) => validGame(value),
  (state, session) => {
    const game = state.get("game"),
      board = boardFor(game.moves),
      won = winner(board);
    [...grid.children].forEach((cell, i) => {
      cell.dataset.side = board[i];
      cell.textContent = board[i] || "";
      cell.setAttribute(
        "aria-label",
        `Row ${Math.floor(i / 7) + 1}, column ${(i % 7) + 1}: ${["empty", "mint", "lilac"][board[i]]}`,
      );
    });
    [...controls.children].forEach((button, i) => {
      button.disabled = !session.connected || !!won || board[i] !== 0;
    });
    document.querySelector("#result").textContent = won
      ? `${won === 1 ? "Mint" : "Lilac"} wins!`
      : game.moves.length === 42
        ? "A draw. Try another round!"
        : `${game.moves.length % 2 ? "Lilac (2)" : "Mint (1)"} to play · ${game.moves.length} moves`;
    document.querySelector("#reset").disabled = !session.connected;
  },
);
document.querySelector("#reset").onclick = () => state.set("game", newGame());
