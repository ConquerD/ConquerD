export function newGame() {
  return { moves: [] };
}
export function boardFor(moves) {
  const board = Array(42).fill(0);
  for (let turn = 0; turn < moves.length; turn++) {
    const column = moves[turn];
    if (!Number.isInteger(column) || column < 0 || column > 6 || winner(board))
      return null;
    let row = 5;
    while (row >= 0 && board[row * 7 + column]) row--;
    if (row < 0) return null;
    board[row * 7 + column] = (turn % 2) + 1;
  }
  return board;
}
export function winner(board) {
  for (let row = 0; row < 6; row++)
    for (let col = 0; col < 7; col++) {
      const side = board[row * 7 + col];
      if (!side) continue;
      for (const [dx, dy] of [
        [1, 0],
        [0, 1],
        [1, 1],
        [-1, 1],
      ]) {
        if (col + dx * 3 < 0 || col + dx * 3 > 6 || row + dy * 3 > 5) continue;
        if (
          [1, 2, 3].every(
            (i) => board[(row + dy * i) * 7 + col + dx * i] === side,
          )
        )
          return side;
      }
    }
  return 0;
}
export function validGame(value) {
  return (
    !!value &&
    Array.isArray(value.moves) &&
    value.moves.length <= 42 &&
    !!boardFor(value.moves)
  );
}
export function drop(value, column) {
  const next = { moves: [...value.moves, column] };
  return validGame(next) ? next : null;
}
