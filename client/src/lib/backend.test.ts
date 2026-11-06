// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it, vi } from "vitest";

import { ApiError, createHttp } from "./api/http";
import { dispositionName, httpBackend } from "./backend";

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

it.each([
  [200, true],
  [503, false],
])("reads readiness from a %i", async (status, ready) => {
  const { backend } = answering(() => new Response("{}", { status }));

  await expect(backend.getReadiness()).resolves.toBe(ready);
});

it("does not take a proxy's 502 for the application's own answer", async () => {
  const { backend } = answering(() => new Response(null, { status: 502 }));

  await expect(backend.getReadiness()).rejects.toBeInstanceOf(ApiError);
});

function page(avatar: object | null): Response {
  return Response.json({
    items: [
      {
        id: "prf_a",
        name: "Anna",
        kind: "person",
        created_at: "2026-10-31T00:00:00Z",
        avatar,
        links: {
          self: "",
          avatar: "/api/v1/profiles/prf_a/avatar",
          up_next: "",
          title_progress: "",
        },
      },
    ],
    links: { self: "" },
  });
}

it("makes a new avatar a new URL, since an element keeps what a URL gave", async () => {
  const uploaded = (updated_at: string) => ({ media_type: "image/png", size_bytes: 1, updated_at });
  const read = async (avatar: object) =>
    (await answering(() => page(avatar)).backend.listProfiles())[0]?.links.avatar;

  const first = await read(uploaded("2026-11-06T00:00:00Z"));
  const second = await read(uploaded("2026-11-07T00:00:00Z"));

  expect(first).toContain("/api/v1/profiles/prf_a/avatar?v=");
  expect(second).not.toBe(first);
});

it("leaves the link of a profile without an avatar as it is", async () => {
  const [profile] = await answering(() => page(null)).backend.listProfiles();

  expect(profile?.links.avatar).toBe("/api/v1/profiles/prf_a/avatar");
});

it("tells an open server from a password read that failed", async () => {
  const open = answering(() => new Response(null, { status: 404 }));
  await expect(open.backend.getPassword()).resolves.toBeUndefined();

  const failing = answering(() => new Response(null, { status: 500 }));
  await expect(failing.backend.getPassword()).rejects.toBeInstanceOf(ApiError);
});

it("reads every page of access tokens", async () => {
  const token = (id: string) => ({ id, name: id, scope: "api" });
  const pages = [
    { items: [token("atk_a")], links: { self: "/", next: "/api/v1/access-tokens?cursor=abc" } },
    { items: [token("atk_b")], links: { self: "/" } },
  ];
  const { backend, fetch } = answering(() => Response.json(pages.shift()));

  const tokens = await backend.listAccessTokens();

  expect(tokens.map((listed) => listed.id)).toEqual(["atk_a", "atk_b"]);
  expect(new URL(fetch.mock.lastCall?.[0].url ?? "").searchParams.get("cursor")).toBe("abc");
});

it("takes a bodyless revocation and removal", async () => {
  const { backend } = answering(() => new Response(null, { status: 204 }));

  await expect(backend.deleteAccessToken("atk_a")).resolves.toBeUndefined();
  await expect(backend.deletePassword()).resolves.toBeUndefined();
  await expect(backend.restoreBackup(new File(["PK"], "a.zip"))).resolves.toBeUndefined();
});

it("sends a backup as a zip archive whatever the file calls itself", async () => {
  const { backend, fetch } = answering(() => new Response(null, { status: 204 }));
  const archive = new File(["PK"], "a.zip", { type: "application/x-zip-compressed" });

  await backend.restoreBackup(archive);

  const request = fetch.mock.lastCall?.[0];
  expect(request?.headers.get("Content-Type")).toBe("application/zip");
  expect(await request?.text()).toBe("PK");
});

it("names a downloaded backup as the server does", async () => {
  const { backend } = answering(
    () =>
      new Response("PK", {
        headers: { "Content-Disposition": 'attachment; filename="torrplay-backup-1.zip"' },
      }),
  );

  const { archive, name } = await backend.getBackup();

  expect(name).toBe("torrplay-backup-1.zip");
  expect(await archive.text()).toBe("PK");
});

it("gives a backup a name when the server gives none", () => {
  expect(dispositionName(null)).toBe("torrplay-backup.zip");
});

it("reports a refused backup download as a problem", async () => {
  const { backend } = answering(() =>
    Response.json(
      { type: "x/insufficient-storage", title: "Full", detail: "Full", status: 507 },
      { status: 507, headers: { "Content-Type": "application/problem+json" } },
    ),
  );

  await expect(backend.getBackup()).rejects.toMatchObject({ type: "insufficient-storage" });
});
