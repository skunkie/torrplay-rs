// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { svelteTesting } from "@testing-library/svelte/vite";
import { defineConfig } from "vitest/config";

// `pnpm dev` serves the client and passes the application's own paths to a
// running torrplay, as the embedded client reaches them on one origin.
const application = process.env.TORRPLAY_URL ?? "http://127.0.0.1:8642";

export default defineConfig({
  // svelteTesting resolves Svelte for the browser under Vitest only.
  plugins: [tailwindcss(), svelte(), svelteTesting()],
  build: {
    // Vite names these files by content, so the server may cache them.
    assetsDir: "assets",
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    proxy: Object.fromEntries(["/api", "/livez", "/readyz"].map((path) => [path, application])),
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
    setupFiles: ["src/test-setup.ts"],
  },
});
