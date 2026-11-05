// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it, vi } from "vitest";

import { ApiError, createHttp } from "./api/http";
import { httpBackend } from "./backend";

/** A backend whose requests all answer `response`. */
function answering(response: () => Response) {
  const fetch = vi.fn<(request: Request) => Promise<Response>>(async () => response());
  const backend = httpBackend(
    createHttp({
      token: () => undefined,
      unauthorized: () => {},
      baseUrl: "http://torrplay.test",
      fetch: fetch as unknown as typeof globalThis.fetch,
    }),
  );
  return { backend, fetch };
}

const image = new File(["x"], "poster.png", { type: "image/png" });

it.each([201, 204])("takes a poster stored with an empty %i", async (status) => {
  const { backend, fetch } = answering(() => new Response(null, { status }));

  await expect(backend.setPoster("trr_a", image)).resolves.toBeUndefined();

  const request = fetch.mock.calls[0]?.[0] as Request;
  expect(request.method).toBe("PUT");
  expect(request.headers.get("Content-Type")).toBe("image/png");
});

it("reports a poster the server refuses", async () => {
  const { backend } = answering(
    () =>
      new Response(JSON.stringify({ type: "x/validation-failed", title: "No", status: 422 }), {
        status: 422,
        headers: { "Content-Type": "application/problem+json" },
      }),
  );

  await expect(backend.setPoster("trr_a", image)).rejects.toBeInstanceOf(ApiError);
});
