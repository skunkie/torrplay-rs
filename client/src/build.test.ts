// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { build, type Rollup } from "vite";
import { expect, it } from "vitest";

it("builds no font into a data URL, which the content security policy refuses", async () => {
  const output = (await build({
    logLevel: "silent",
    build: { write: false },
  })) as Rollup.RollupOutput;

  const styles = output.output.filter((file) => file.fileName.endsWith(".css"));
  expect(styles).not.toHaveLength(0);
  for (const style of styles) {
    expect(style.type === "asset" ? String(style.source) : "").not.toContain("url(data:font");
  }
}, 60_000);
