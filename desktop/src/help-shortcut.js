// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

function installHelpShortcut(helpUrl) {
  const help = new URL(helpUrl);
  // WebView2 also runs the initialization script in subframes.
  if (window.top !== window || location.origin !== help.origin) return;

  window.addEventListener(
    "keydown",
    (event) => {
      if (
        !event.isTrusted ||
        event.key !== "F1" ||
        event.altKey ||
        event.ctrlKey ||
        event.metaKey ||
        event.shiftKey
      )
        return;

      event.preventDefault();
      if (!event.repeat) location.assign(help.href);
    },
    true,
  );
}
