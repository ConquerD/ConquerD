import { workspace } from "../../web-sdk/demo-workspace.mjs";
import { newTimer, validTimer, remaining, toggle } from "./timer.mjs";

function render(state, session) {
  const timer = state.get("timer"),
    seconds = remaining(timer);
  document.querySelector("#time").textContent =
    `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
  document.querySelector("#phase").textContent =
    seconds === 0
      ? "Session complete. Take a breath."
      : timer.end
        ? "Focus time. One thing at a time."
        : "Ready when you are.";
  document.querySelector("#progress").value =
    (timer.duration - seconds) / timer.duration;
  document.querySelector("#toggle").textContent =
    timer.end && seconds ? "Pause together" : "Start together";
  document.querySelectorAll(".workspace button").forEach((button) => {
    button.disabled = !session.connected;
  });
}
const { state, session } = workspace(
  "focus-timer",
  {
    title: "Focus Timer",
    category: "Productivity · Shared rhythm",
    description:
      "Start a focus session or a short break together. Anyone can pause, reset or choose a duration.",
    controls: "Choose a duration · Start or pause for everyone",
    detail:
      "A shared deadline avoids sending a packet every second. Keep device clocks in sync for matching countdowns. The timer is silent and only runs while this page is open.",
  },
  { timer: newTimer() },
  (_, value) => validTimer(value),
  render,
);
document.querySelectorAll("[data-minutes]").forEach((button) => {
  button.onclick = () =>
    state.set("timer", newTimer(Number(button.dataset.minutes) * 60));
});
document.querySelector("#toggle").onclick = () => {
  const timer = state.get("timer");
  state.set(
    "timer",
    toggle(timer.end && !remaining(timer) ? newTimer(timer.duration) : timer),
  );
};
document.querySelector("#reset").onclick = () =>
  state.set("timer", newTimer(state.get("timer").duration));
const tick = setInterval(() => render(state, session), 250);
window.addEventListener("pagehide", () => clearInterval(tick), { once: true });
