import { workspace } from "../../web-sdk/demo-workspace.mjs";
import { defaults, validTask } from "./tasks.mjs";

const list = document.querySelector("#tasks");
for (const key of Object.keys(defaults)) {
  const row = document.createElement("li");
  row.className = "task-row";
  const check = document.createElement("input");
  check.type = "checkbox";
  check.setAttribute("aria-label", `Complete task ${list.children.length + 1}`);
  const input = document.createElement("input");
  input.type = "text";
  input.maxLength = 80;
  input.placeholder = "Add a task…";
  input.setAttribute("aria-label", `Task ${list.children.length + 1}`);
  const clear = document.createElement("button");
  clear.textContent = "×";
  clear.setAttribute("aria-label", `Clear task ${list.children.length + 1}`);
  input.onchange = () =>
    state.set(key, { text: input.value.trim(), done: state.get(key).done });
  input.onkeydown = (event) => {
    if (event.key === "Enter") input.blur();
  };
  input.onblur = () => {
    input.value = state.get(key).text;
  };
  check.onchange = () =>
    state.set(key, { ...state.get(key), done: check.checked });
  clear.onclick = () => state.set(key, { text: "", done: false });
  row.append(check, input, clear);
  list.append(row);
}
const { state } = workspace(
  "task-board",
  {
    title: "Task Board",
    category: "Productivity · Shared checklist",
    description:
      "Plan eight small next steps together. Press Enter or leave a field to share your edit.",
    controls: "Edit a task · Enter to share · Check it off together",
    detail:
      "Each row syncs independently and repeats for late arrivals. Simultaneous edits to one row resolve deterministically. Tasks live only in open pages; closing the last page loses the board.",
  },
  defaults,
  (_, value) => validTask(value),
  (state, session) => {
    let count = 0,
      done = 0;
    [...list.children].forEach((row, i) => {
      const value = state.get(`task${i}`),
        [check, input, clear] = row.children;
      if (document.activeElement !== input) input.value = value.text;
      check.checked = value.done;
      check.disabled = !session.connected || !value.text;
      input.disabled = clear.disabled = !session.connected;
      if (value.text) {
        count++;
        if (value.done) done++;
      }
    });
    document.querySelector("#result").textContent = count
      ? `${done} of ${count} complete`
      : "A little space for your next steps";
  },
);
