// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { expect, it, vi } from "vitest";

import { ApiError } from "../lib/api/http";
import type { AccessToken, Backend, Password } from "../lib/backend";
import { backendContext } from "../lib/context";
import { fakeBackend, system } from "../testing";
import StremioPage from "./StremioPage.svelte";

const LAN = "http://192.168.1.10:8642";
const SET: Password = { updated_at: "2026-10-24T12:00:00Z", links: { self: "/api/v1/password" } };

function link(id: string, members: Partial<AccessToken> = {}): AccessToken {
  return {
    id,
    name: "Stremio",
    scope: "playback",
    created_at: "2026-10-24T12:00:00Z",
    expires_at: null,
    last_used_at: null,
    links: { self: `/api/v1/access-tokens/${id}` },
    ...members,
  };
}

function show(backend: Backend) {
  render(StremioPage, { context: backendContext(backend) });
}

function withPassword(tokens: AccessToken[], members: Partial<Backend> = {}): Backend {
  return fakeBackend({
    getPassword: vi.fn(async () => SET),
    listAccessTokens: vi.fn(async () => tokens),
    ...members,
  });
}

async function createLink(): Promise<void> {
  await fireEvent.input(await screen.findByLabelText("Current password"), {
    target: { value: "a passphrase" },
  });
  const button = screen.getByRole("button", { name: "Create link" });
  await fireEvent.submit(button.closest("form") as HTMLFormElement);
}

async function field(): Promise<HTMLTextAreaElement> {
  return screen.findByLabelText<HTMLTextAreaElement>("Link for Stremio");
}

it("shows the link at once without a password, and asks for no tokens", async () => {
  const list = vi.fn(async () => []);
  // Tests run at localhost, which other devices cannot use.
  show(
    fakeBackend({ getSystem: vi.fn(async () => system({ urls: [LAN] })), listAccessTokens: list }),
  );

  expect((await field()).value).toBe(`${LAN}/stremio/manifest.json`);
  expect(screen.getByText(/You opened TorrPlay at localhost/)).toBeTruthy();
  expect(screen.getByText(/No password is set/)).toBeTruthy();
  expect(screen.getByRole("link", { name: "Open in Stremio" }).getAttribute("href")).toBe(
    "stremio://192.168.1.10:8642/stremio/manifest.json",
  );
  expect(list).not.toHaveBeenCalled();
});

it("changes the link with the address chosen", async () => {
  const other = "http://100.84.7.2:8642";
  show(fakeBackend({ getSystem: vi.fn(async () => system({ urls: [LAN, other] })) }));
  await field();

  await fireEvent.change(screen.getByLabelText("Address other devices use"), {
    target: { value: other },
  });

  expect((await field()).value).toBe(`${other}/stremio/manifest.json`);
});

it("offers no choice of a single address", async () => {
  show(fakeBackend({ getSystem: vi.fn(async () => system({ urls: [LAN] })) }));
  await field();
  expect(screen.queryByLabelText("Address other devices use")).toBeNull();
});

it("says when the link works on this computer only", async () => {
  show(fakeBackend({ getSystem: vi.fn(async () => system({ urls: [] })) }));

  expect((await field()).value).toBe(`${window.location.origin}/stremio/manifest.json`);
  expect(screen.getByText(/works on this computer only/)).toBeTruthy();
  expect(screen.getByText(/--listen 127.0.0.1/)).toBeTruthy();
  expect(screen.getByText(/open Add-ons and paste this link/)).toBeTruthy();
});

it("creates a playback token for a link, and shows the link once", async () => {
  const created = {
    ...link("a"),
    secret: "tpp_secret",
  };
  const create = vi.fn<Backend["createAccessToken"]>(async () => created);
  let tokens: AccessToken[] = [];
  show(
    withPassword([], {
      getSystem: vi.fn(async () => system({ urls: [LAN] })),
      listAccessTokens: vi.fn(async () => tokens),
      createAccessToken: vi.fn(async (creation) => {
        tokens = [link("a")];
        return create(creation);
      }),
    }),
  );

  await createLink();

  const url = await field();
  expect(url.value).toBe(`${LAN}/stremio/tpp_secret/manifest.json`);
  expect(create).toHaveBeenCalledWith({
    password: "a passphrase",
    name: "Stremio",
    scope: "playback",
    expires_in_seconds: null,
  });
  expect(screen.getByText("Copy this link now. It is not shown again.")).toBeTruthy();
  await waitFor(() => {
    expect(document.activeElement).toBe(url);
  });
});

it("explains the address of a link created at localhost", async () => {
  show(
    withPassword([], {
      getSystem: vi.fn(async () => system({ urls: [LAN] })),
      createAccessToken: vi.fn(async () => ({ ...link("a"), secret: "tpp_secret" })),
    }),
  );

  await createLink();

  await field();
  expect(screen.getByText(/You opened TorrPlay at localhost/)).toBeTruthy();
});

it("keeps showing a new link when the page cannot be read after it", async () => {
  const read = vi
    .fn<Backend["getPassword"]>()
    .mockResolvedValueOnce(SET)
    .mockRejectedValue(new ApiError(503, undefined));
  show(
    fakeBackend({
      getPassword: read,
      getSystem: vi.fn(async () => system({ urls: [LAN] })),
      createAccessToken: vi.fn(async () => ({ ...link("a"), secret: "tpp_secret" })),
    }),
  );

  await createLink();

  expect((await field()).value).toContain("tpp_secret");
  await waitFor(() => {
    expect(read).toHaveBeenCalledTimes(2);
  });
  expect((await field()).value).toContain("tpp_secret");
});

it("says why a link could not be created", async () => {
  show(
    withPassword([], {
      createAccessToken: vi.fn(async () => {
        throw new ApiError(500, undefined);
      }),
    }),
  );

  await createLink();

  expect(await screen.findByRole("alert")).toBeTruthy();
  // Wrong the first time, it is still there to correct.
  expect(screen.getByLabelText<HTMLInputElement>("Current password").value).toBe("a passphrase");
  expect(screen.queryByLabelText("Link for Stremio")).toBeNull();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Create link" }).disabled).toBe(
    false,
  );
});

it("cannot show an existing link again, and offers a new one", async () => {
  const other: AccessToken = { ...link("x"), name: "TV" };
  show(withPassword([link("a", { last_used_at: "2026-10-25T12:00:00Z" }), other]));

  expect(await screen.findByRole("heading", { name: "Existing links" })).toBeTruthy();
  expect(screen.getAllByRole("listitem")).toHaveLength(1);
  expect(screen.getByText(/Last used/)).toBeTruthy();
  expect(screen.queryByLabelText("Link for Stremio")).toBeNull();
  expect(screen.getByRole("button", { name: "Create a new link" })).toBeTruthy();
});

it("revokes a link once asked, and leaves it on a refusal to", async () => {
  let tokens = [link("a")];
  const remove = vi.fn(async () => {
    tokens = [];
  });
  show(
    withPassword([], {
      listAccessTokens: vi.fn(async () => tokens),
      deleteAccessToken: remove,
    }),
  );

  await fireEvent.click(await screen.findByRole("button", { name: "Revoke" }));
  expect(remove).not.toHaveBeenCalled();
  const dialog = screen.getByRole("dialog", { hidden: true });
  await fireEvent.click(within(dialog).getByRole("button", { name: "Revoke", hidden: true }));

  await waitFor(() => {
    expect(remove).toHaveBeenCalledWith("a");
  });
  expect(await screen.findByRole("button", { name: "Create link" })).toBeTruthy();
});

it("tells apart the links it can revoke", async () => {
  show(withPassword([link("a"), link("b", { last_used_at: "2026-10-25T12:00:00Z" })]));

  const buttons = await screen.findAllByRole("button", { name: "Revoke" });
  const described = buttons.map(
    (button) => document.getElementById(button.getAttribute("aria-describedby") ?? "")?.textContent,
  );
  expect(described[0]).toContain("Not used yet");
  expect(described[1]).toContain("Last used");
});

it("offers to try again when the page cannot be read", async () => {
  const read = vi
    .fn<Backend["getSystem"]>()
    .mockRejectedValueOnce(new ApiError(503, undefined))
    .mockResolvedValue(system({ urls: [LAN] }));
  show(fakeBackend({ getSystem: read }));

  await fireEvent.click(await screen.findByRole("button", { name: "Try again" }));

  expect((await field()).value).toContain(LAN);
});
