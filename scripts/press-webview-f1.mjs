// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

// Send a trusted F1 key event to the installed client's WebView2.
import { setTimeout as delay } from "node:timers/promises";

const [port, origin] = process.argv.slice(2);
const endpoint = new URL(`http://127.0.0.1:${port}/json/list`);
let stage = "discovering the WebView debugger";
const deadline = setTimeout(() => {
  console.error(`Timed out ${stage}.`);
  process.exit(1);
}, 60_000);

let socket;
try {
  let page;
  while (!page) {
    const response = await fetch(endpoint, { signal: AbortSignal.timeout(1500) }).catch(() => null);
    if (response?.ok) {
      const pages = await response.json();
      page = pages.find(
        ({ type, url }) => type === "page" && URL.canParse(url) && new URL(url).origin === origin,
      );
      if (!page) stage = `waiting for ${origin} among ${pages.map(({ url }) => url).join(", ")}`;
    }
    if (!page) await delay(250);
  }

  const address = new URL(page.webSocketDebuggerUrl);
  if (
    !["127.0.0.1", "localhost", "[::1]"].includes(address.hostname) ||
    address.port !== endpoint.port ||
    address.protocol !== "ws:"
  ) {
    throw new Error("Expected a loopback WebView debugger at the test port.");
  }
  address.hostname = endpoint.hostname;
  stage = "connecting to the WebView debugger";
  socket = new WebSocket(address);
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", () => reject(new Error("Could not connect to the WebView.")), {
      once: true,
    });
  });

  let lastId = 0;
  async function call(method, params) {
    const id = ++lastId;
    const reply = new Promise((resolve, reject) => {
      function receive(event) {
        const message = JSON.parse(event.data);
        if (message.id !== id) return;
        socket.removeEventListener("message", receive);
        if (message.error) reject(new Error(`${method}: ${message.error.message}`));
        else resolve(message.result);
      }
      socket.addEventListener("message", receive);
    });
    socket.send(JSON.stringify({ id, method, params }));
    return reply;
  }

  stage = "waiting for the service page to finish loading";
  while (true) {
    const { result } = await call("Runtime.evaluate", {
      expression: `location.origin === ${JSON.stringify(origin)} && document.readyState === "complete"`,
      returnByValue: true,
    });
    if (result.value === true) break;
    await delay(250);
  }
  stage = "sending the key event to the client WebView";
  const key = { key: "F1", code: "F1", windowsVirtualKeyCode: 112 };
  await call("Input.dispatchKeyEvent", { ...key, type: "rawKeyDown" });
  await call("Input.dispatchKeyEvent", { ...key, type: "keyUp" });
} finally {
  socket?.close();
  clearTimeout(deadline);
}
