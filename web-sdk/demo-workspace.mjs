import { DemoSession } from "./demo-session.mjs";
import { mountShell } from "./demo-shell.mjs";
import { SharedState } from "./demo-state.mjs";

export function workspace(app, options, defaults, validate, render) {
  const session = new DemoSession(app);
  const shell = mountShell(session, options);
  const state = new SharedState(session, defaults, validate, () =>
    render(state, session),
  );
  session.on("connected", () => render(state, session));
  session.on("disconnected", () => render(state, session));
  const timer = setInterval(() => state.tick(), 400);
  window.addEventListener("pagehide", () => clearInterval(timer), {
    once: true,
  });
  render(state, session);
  session.connect().catch((error) => {
    console.error("[demo] session connect failed:", error);
    shell.setStatus(
      "Open in DoubleSlash through a connected supernode",
      "error",
    );
  });
  return { state, session, shell };
}
