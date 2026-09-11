export const COLORS = [
  "#87b7ff",
  "#6be4bc",
  "#ffa878",
  "#c7a0ff",
  "#f7ce69",
  "#ff91ba",
];
export function peerColor(id) {
  return COLORS[parseInt(id.slice(0, 6), 16) % COLORS.length];
}
export function shortName(id) {
  return `Guest ${id.slice(0, 4).toUpperCase()}`;
}

export function mountShell(
  session,
  { title, category, description, controls, detail },
) {
  const root = document.querySelector(".app");
  const panel = document.querySelector(".session-panel");
  document.querySelector("h1").textContent = title;
  document.querySelector(".eyebrow").textContent = category;
  document.querySelector("#room-label").textContent = session.room;
  document.querySelector("#controls-hint").textContent = controls;
  panel.innerHTML = `<h2>Session</h2><p class="description"></p>
    <form id="room-form"><label for="room-input">Room name</label><input id="room-input" maxlength="48" required pattern="[A-Za-z0-9 _.-]+" autocomplete="off"><button type="submit">Join room</button><button id="new-room" type="button">New room</button></form>
    <p>Use the same app, room name and supernode on another device.</p>
    <label id="copy-fallback" hidden>Session link<input id="link-copy" readonly aria-label="Session link to copy"></label>
    <button id="ready" type="button" aria-pressed="false">Mark ready</button>
    <h3>Here now <span id="member-count"></span></h3><ul class="members"></ul>
    <h3>Live connection</h3><div class="metrics">
    <div class="metric"><b id="rtt">—</b><span>Peer round trip</span></div>
    <div class="metric"><b id="packet-rate">0</b><span>Packets / sec · in + out</span></div>
    <div class="metric"><b id="bandwidth">0</b><span>App bytes / sec · in + out</span></div>
    <div class="metric"><b id="send-errors">0</b><span>Send errors / skipped</span></div></div>
    <svg class="sparkline" viewBox="0 0 240 40" role="img" aria-label="Recent application traffic"><path fill="none" stroke="#6be4bc" stroke-width="2"/></svg>
    <p>Measured here. Round trip includes the native bridge and peer response; it is not a raw network ping.</p>
    <details><summary>What this demonstrates</summary><p class="demo-detail"></p><p>Hosted pages + native identity transport + app-defined messages. Participant labels are temporary app sessions, not verified identity badges. Readiness is a lobby signal, not an access grant.</p></details>`;
  panel.querySelector(".description").textContent = description;
  panel.querySelector(".demo-detail").textContent = detail;
  const input = panel.querySelector("input");
  input.value = session.room;
  const status = document.querySelector("#status");
  const setStatus = (text, cls = "") => {
    status.textContent = text;
    status.className = cls;
  };
  const toggle = document.querySelector("#session-toggle");
  toggle.onclick = () => {
    panel.hidden = !panel.hidden;
    toggle.setAttribute("aria-expanded", String(!panel.hidden));
  };
  const focus = (value) => {
    root.classList.toggle("focus-mode", value);
    document
      .querySelector("#focus")
      .setAttribute("aria-pressed", String(value));
  };
  document.querySelector("#focus").onclick = () => focus(true);
  document.querySelector("#exit-focus").onclick = () => focus(false);
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") focus(false);
  });
  panel.querySelector("#room-form").onsubmit = (e) => {
    e.preventDefault();
    const room = input.value.trim();
    if (!room) return;
    const url = new URL(location.href);
    url.searchParams.set("room", room);
    location.assign(url.href);
  };
  panel.querySelector("#new-room").onclick = () => {
    input.value = `room-${crypto.randomUUID().slice(0, 8)}`;
    input.focus();
  };
  document.querySelector("#share").onclick = async () => {
    const url = new URL(location.href);
    url.searchParams.set("room", session.room);
    // Keep the desktop-safe scheme when copying links from its portal.
    try {
      await navigator.clipboard.writeText(url.href);
      document.querySelector("#notice").textContent = "Session link copied";
    } catch {
      const fallback = panel.querySelector("#copy-fallback");
      const link = panel.querySelector("#link-copy");
      fallback.hidden = false;
      link.value = url.href;
      panel.hidden = false;
      toggle.setAttribute("aria-expanded", "true");
      link.focus();
      link.select();
      document.querySelector("#notice").textContent =
        "Copy the selected session link";
    }
  };
  const ready = panel.querySelector("#ready");
  ready.onclick = () => session.setReady(!session.ready);
  const members = () => {
    const list = panel.querySelector(".members");
    list.replaceChildren();
    const ids = session.connected ? [session.id, ...session.peers.keys()] : [];
    document.querySelector("#peers-count").textContent = `${ids.length} here`;
    panel.querySelector("#member-count").textContent = `· ${ids.length}`;
    for (const id of ids) {
      const li = document.createElement("li");
      const dot = document.createElement("span");
      dot.className = "member-dot";
      dot.style.background = peerColor(id);
      const name = document.createElement("span");
      name.textContent = id === session.id ? "You" : shortName(id);
      const meta = document.createElement("span");
      meta.className = "member-meta";
      meta.textContent = (
        id === session.id ? session.ready : session.peers.get(id)?.ready
      )
        ? "Ready"
        : "In session";
      if (id === session.leader) meta.textContent += " · coordinator";
      li.append(dot, name, meta);
      list.append(li);
    }
    ready.setAttribute("aria-pressed", String(session.ready));
    ready.textContent = session.ready ? "Ready ✓" : "Mark ready";
    ready.disabled = !session.connected;
  };
  session
    .on("members", members)
    .on("connected", () => {
      setStatus("Live · native relay", "connected");
      members();
    })
    .on("disconnected", () => {
      setStatus("Disconnected · reopen to reconnect", "error");
      members();
    })
    .on("error", () =>
      setStatus("Connection interrupted · check native session", "error"),
    );
  let previous = { ...session.stats },
    last = performance.now();
  const history = [];
  const timer = setInterval(() => {
    const now = performance.now(),
      seconds = (now - last) / 1000;
    const s = session.stats;
    const packets = Math.round(
      (s.sent + s.received - previous.sent - previous.received) / seconds,
    );
    const bytes = Math.round(
      (s.bytesIn + s.bytesOut - previous.bytesIn - previous.bytesOut) / seconds,
    );
    const rtts = [...session.peers.values()]
      .map((p) => p.rtt)
      .filter((n) => n !== null);
    panel.querySelector("#rtt").textContent = rtts.length
      ? `${Math.round(rtts.reduce((a, b) => a + b, 0) / rtts.length)} ms`
      : "—";
    panel.querySelector("#packet-rate").textContent = packets;
    panel.querySelector("#bandwidth").textContent =
      bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(1)} KB`;
    panel.querySelector("#send-errors").textContent =
      `${s.errors} / ${s.dropped}`;
    history.push(bytes);
    if (history.length > 40) history.shift();
    const max = Math.max(100, ...history);
    panel
      .querySelector("path")
      .setAttribute(
        "d",
        history
          .map((v, i) => `${i ? "L" : "M"}${i * 6},${38 - (v / max) * 34}`)
          .join(" "),
      );
    previous = { ...s };
    last = now;
  }, 1000);
  window.addEventListener(
    "pagehide",
    () => {
      clearInterval(timer);
      session.disconnect();
    },
    { once: true },
  );
  members();
  return {
    setStatus,
    notice: (text) => {
      document.querySelector("#notice").textContent = text;
    },
  };
}
