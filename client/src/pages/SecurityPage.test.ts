// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { beforeEach, expect, it, vi } from "vitest";

import { ApiError } from "../lib/api/http";
import type { AccessToken, Backend, CreatedAccessToken, Password } from "../lib/backend";
import { backendContext } from "../lib/context";
import { loadProfiles, profiles, resetProfiles } from "../lib/profile.svelte";
import { session, signIn, signOut } from "../lib/session.svelte";
import { fakeBackend, profile } from "../testing";
import SecurityPage from "./SecurityPage.svelte";

const NEW_PASSWORD = "a new and longer passphrase";

const set: Password = {
  updated_at: "2026-10-24T12:00:00Z",
  links: { self: "/api/v1/password" },
};

function token(id: string, name: string, members: Partial<AccessToken> = {}): AccessToken {
  return {
    id,
    name,
    scope: "api",
    created_at: "2026-10-24T12:00:00Z",
    expires_at: null,
    last_used_at: null,
    links: { self: `/api/v1/access-tokens/${id}` },
    ...members,
  };
}

const created: CreatedAccessToken = { ...token("atk_new", "Web browser"), secret: "tpa_new" };

function show(backend: Backend) {
  render(SecurityPage, { context: backendContext(backend) });
}

function withPassword(members: Partial<Backend> = {}): Backend {
  return fakeBackend({
    getPassword: vi.fn(async () => set),
    listAccessTokens: vi.fn(async () => [token("atk_a", "Laptop"), token("atk_b", "Grafana")]),
    ...members,
  });
}

function refused(status: number, name: string, detail: string): ApiError {
  return new ApiError(status, {
    type: `tag:skunkie.github.io,2026:torrplay/problems/${name}`,
    title: detail,
    detail,
    status,
  });
}

async function fill(label: string, value: string): Promise<void> {
  await fireEvent.input(await screen.findByLabelText(label), { target: { value } });
}

beforeEach(() => {
  signOut();
  resetProfiles();
  localStorage.clear();
});

it("sets the first password and signs this device in with it", async () => {
  const getPassword = vi
    .fn<Backend["getPassword"]>()
    .mockResolvedValueOnce(undefined)
    .mockResolvedValue(set);
  const setPassword = vi.fn(async () => {});
  const backend = fakeBackend({
    getPassword,
    setPassword,
    signIn: vi.fn(async () => created),
    listAccessTokens: vi.fn(async () => [token("atk_new", "Web browser")]),
  });
  show(backend);

  expect(await screen.findByText(/anyone on this network/)).toBeTruthy();
  expect(screen.getByText("Access tokens work once a password is set.")).toBeTruthy();
  expect(screen.queryByLabelText("Current password")).toBeNull();
  await fill("New password", NEW_PASSWORD);
  await fill("Repeat the new password", NEW_PASSWORD);
  await fireEvent.click(screen.getByRole("button", { name: "Set password" }));

  await screen.findByText(/signing in is required/);
  expect(setPassword).toHaveBeenCalledWith({ password: NEW_PASSWORD, revoke_access_tokens: false });
  expect(backend.signIn).toHaveBeenCalledWith(NEW_PASSWORD, "Web browser");
  expect(session.token).toBe("tpa_new");
  expect(screen.getByText("This device")).toBeTruthy();
});

it.each([
  ["short", "short", "A password needs at least 8\u00A0characters."],
  [NEW_PASSWORD, "not the same one", "The two passwords are not the same."],
])("refuses %s and %s without sending them", async (first, second, message) => {
  const setPassword = vi.fn(async () => {});
  show(fakeBackend({ setPassword }));
  await fill("New password", first);
  await fill("Repeat the new password", second);
  await fireEvent.click(screen.getByRole("button", { name: "Set password" }));

  expect((await screen.findByRole("alert")).textContent).toBe(message);
  expect(setPassword).not.toHaveBeenCalled();
});

it("changes the password with the current one and keeps the session", async () => {
  signIn({ secret: "tpa_old", id: "atk_a" });
  const backend = withPassword();
  show(backend);
  await fireEvent.click(await screen.findByRole("button", { name: "Change password" }));
  await fill("Current password", "the old passphrase");
  await fill("New password", NEW_PASSWORD);
  await fill("Repeat the new password", NEW_PASSWORD);
  await fireEvent.click(screen.getByRole("button", { name: "Change password" }));

  await waitFor(() => expect(screen.queryByLabelText("New password")).toBeNull());
  expect(backend.setPassword).toHaveBeenCalledWith({
    password: NEW_PASSWORD,
    current_password: "the old passphrase",
    revoke_access_tokens: false,
  });
  expect(backend.signIn).not.toHaveBeenCalled();
  expect(session.token).toBe("tpa_old");
});

it("signs in again after a change that revokes every token", async () => {
  signIn({ secret: "tpa_old", id: "atk_a" });
  const backend = withPassword({ signIn: vi.fn(async () => created) });
  show(backend);
  await fireEvent.click(await screen.findByRole("button", { name: "Change password" }));
  await fill("Current password", "the old passphrase");
  await fill("New password", NEW_PASSWORD);
  await fill("Repeat the new password", NEW_PASSWORD);
  await fireEvent.click(screen.getByLabelText(/Sign out every device/));
  await fireEvent.click(screen.getByRole("button", { name: "Change password" }));

  await waitFor(() => expect(session.token).toBe("tpa_new"));
  expect(backend.setPassword).toHaveBeenCalledWith(
    expect.objectContaining({ revoke_access_tokens: true }),
  );
});

it("asks to sign in when the password is saved but signing in fails", async () => {
  const getPassword = vi
    .fn<Backend["getPassword"]>()
    .mockResolvedValueOnce(undefined)
    .mockRejectedValue(refused(401, "unauthenticated", "A token is needed."));
  const backend = fakeBackend({
    getPassword,
    signIn: vi.fn(async () => {
      throw refused(429, "too-many-attempts", "Wait.");
    }),
  });
  show(backend);
  await fill("New password", NEW_PASSWORD);
  await fill("Repeat the new password", NEW_PASSWORD);
  await fireEvent.click(screen.getByRole("button", { name: "Set password" }));

  // The page reads itself again, whose refusal is what goes to sign-in.
  await waitFor(() => expect(getPassword).toHaveBeenCalledTimes(2));
  expect(backend.setPassword).toHaveBeenCalled();
  expect(screen.queryByText("Too many attempts. Wait a moment and try again.")).toBeNull();
});

it("says when the current password is wrong, keeping what was typed", async () => {
  show(
    withPassword({
      setPassword: vi.fn(async () => {
        throw refused(403, "invalid-credentials", "The password does not match.");
      }),
    }),
  );
  await fireEvent.click(await screen.findByRole("button", { name: "Change password" }));
  await fill("Current password", "a wrong passphrase");
  await fill("New password", NEW_PASSWORD);
  await fill("Repeat the new password", NEW_PASSWORD);
  await fireEvent.click(screen.getByRole("button", { name: "Change password" }));

  expect((await screen.findByRole("alert")).textContent).toBe("The password is not correct.");
  expect((screen.getByLabelText("New password") as HTMLInputElement).value).toBe(NEW_PASSWORD);
});

it("removes the password after asking, forgetting the token and its grant", async () => {
  signIn({ secret: "tpa_old", id: "atk_a" });
  await loadProfiles(fakeBackend({ listProfiles: vi.fn(async () => [profile(1, "Anna")]) }));
  const getPassword = vi
    .fn<Backend["getPassword"]>()
    .mockResolvedValueOnce(set)
    .mockResolvedValue(undefined);
  const backend = withPassword({ getPassword });
  show(backend);

  await fireEvent.click(await screen.findByRole("button", { name: "Remove password" }));
  expect(backend.deletePassword).not.toHaveBeenCalled();
  const dialog = screen.getByRole("dialog");
  await fireEvent.click(within(dialog).getByRole("button", { name: "Remove password" }));

  await screen.findByText(/anyone on this network/);
  expect(backend.deletePassword).toHaveBeenCalled();
  expect(session.token).toBeUndefined();
  expect(session.tokenId).toBeUndefined();
  expect(localStorage.getItem("torrplay.access-token")).toBeNull();
  expect(profiles.people).toBeUndefined();
});

it("lists tokens by scope and times, marking this device's", async () => {
  signIn({ secret: "tpa_old", id: "atk_a" });
  show(
    withPassword({
      listAccessTokens: vi.fn(async () => [
        token("atk_a", "Laptop"),
        token("atk_b", "Scraper", {
          scope: "metrics",
          last_used_at: new Date(Date.now() - 2 * 24 * 60 * 60 * 1000).toISOString(),
          expires_at: new Date(Date.now() + 3 * 24 * 60 * 60 * 1000).toISOString(),
        }),
      ]),
    }),
  );

  const own = (await screen.findByText("Laptop")).closest("tr") as HTMLElement;
  expect(within(own).getByText("This device")).toBeTruthy();
  expect(within(own).getAllByText("Full access").length).toBeGreaterThan(0);
  const other = screen.getByText("Scraper").closest("tr") as HTMLElement;
  expect(within(other).queryByText("This device")).toBeNull();
  expect(within(other).getAllByText("Metrics only").length).toBeGreaterThan(0);
  expect(other.textContent).toContain("2 days ago");
  expect(other.textContent).toContain("in 3 days");
});

it("creates a token with the password and shows its secret once", async () => {
  const createAccessToken = vi.fn(async () => created);
  show(withPassword({ createAccessToken }));
  await fireEvent.click(await screen.findByRole("button", { name: "New token" }));
  await fill("Name", " Grafana ");
  await fill("Password", "the passphrase");
  await fireEvent.change(screen.getByLabelText("Scope"), { target: { value: "metrics" } });
  await fireEvent.change(screen.getByLabelText("Lasts"), { target: { value: "year" } });
  await fireEvent.click(screen.getByRole("button", { name: "Create" }));

  const secret = (await screen.findByLabelText("The new token")) as HTMLInputElement;
  expect(secret.value).toBe("tpa_new");
  expect(createAccessToken).toHaveBeenCalledWith({
    password: "the passphrase",
    name: "Grafana",
    scope: "metrics",
    expires_in_seconds: 31_536_000,
  });
  await waitFor(() => expect(screen.queryByLabelText("Name")).toBeNull());

  await fireEvent.click(screen.getByRole("button", { name: "Done" }));
  expect(screen.queryByLabelText("The new token")).toBeNull();
});

it("needs a name for a token", async () => {
  const createAccessToken = vi.fn(async () => created);
  show(withPassword({ createAccessToken }));
  await fireEvent.click(await screen.findByRole("button", { name: "New token" }));
  await fill("Name", "   ");
  await fill("Password", "the passphrase");
  await fireEvent.click(screen.getByRole("button", { name: "Create" }));

  await screen.findByRole("alert");
  expect(createAccessToken).not.toHaveBeenCalled();
});

it("copies the secret where the browser allows it", async () => {
  const writeText = vi.fn(async () => {});
  vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
  try {
    show(withPassword({ createAccessToken: vi.fn(async () => created) }));
    await fireEvent.click(await screen.findByRole("button", { name: "New token" }));
    await fill("Name", "Grafana");
    await fill("Password", "the passphrase");
    await fireEvent.click(screen.getByRole("button", { name: "Create" }));
    await fireEvent.click(await screen.findByRole("button", { name: "Copy" }));

    expect(writeText).toHaveBeenCalledWith("tpa_new");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeTruthy();
  } finally {
    vi.unstubAllGlobals();
  }
});

it("selects the secret when the browser refuses to copy it, and does not say copied", async () => {
  const writeText = vi.fn(async () => {
    throw new DOMException("Refused.", "NotAllowedError");
  });
  vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
  try {
    show(withPassword({ createAccessToken: vi.fn(async () => created) }));
    await fireEvent.click(await screen.findByRole("button", { name: "New token" }));
    await fill("Name", "Grafana");
    await fill("Password", "the passphrase");
    await fireEvent.click(screen.getByRole("button", { name: "Create" }));
    const field = (await screen.findByLabelText("The new token")) as HTMLInputElement;
    const select = vi.spyOn(field, "select");
    await fireEvent.click(screen.getByRole("button", { name: "Copy" }));

    await waitFor(() => expect(select).toHaveBeenCalled());
    expect(screen.queryByRole("button", { name: "Copied" })).toBeNull();
  } finally {
    vi.unstubAllGlobals();
  }
});

it("does not call a newer secret copied", async () => {
  const writeText = vi.fn(async () => {});
  vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
  const createAccessToken = vi
    .fn<Backend["createAccessToken"]>()
    .mockResolvedValueOnce(created)
    .mockResolvedValue({ ...created, secret: "tpa_newer" });
  try {
    show(withPassword({ createAccessToken }));
    for (const copied of [false, true]) {
      await fireEvent.click(await screen.findByRole("button", { name: "New token" }));
      await fill("Name", "Grafana");
      await fill("Password", "the passphrase");
      await fireEvent.click(screen.getByRole("button", { name: "Create" }));
      await fireEvent.click(await screen.findByRole("button", { name: "Copy" }));
      await screen.findByRole("button", { name: "Copied" });
      expect(copied).toBe(writeText.mock.calls.length > 1);
    }
    expect(writeText).toHaveBeenLastCalledWith("tpa_newer");
  } finally {
    vi.unstubAllGlobals();
  }
});

it("moves focus into each form, and back to what opened it", async () => {
  show(withPassword({ createAccessToken: vi.fn(async () => created) }));

  const change = await screen.findByRole("button", { name: "Change password" });
  await fireEvent.click(change);
  await waitFor(() => expect(screen.getByLabelText("Current password")).toHaveFocus());
  await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Change password" })).toHaveFocus(),
  );

  await fireEvent.click(screen.getByRole("button", { name: "New token" }));
  await waitFor(() => expect(screen.getByLabelText("Name")).toHaveFocus());
  await fill("Name", "Grafana");
  await fill("Password", "the passphrase");
  await fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(screen.getByLabelText("The new token")).toHaveFocus());
  await fireEvent.click(screen.getByRole("button", { name: "Done" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "New token" })).toHaveFocus());
});

it("leaves copying to the visitor where the browser does not allow it", async () => {
  show(withPassword({ createAccessToken: vi.fn(async () => created) }));
  await fireEvent.click(await screen.findByRole("button", { name: "New token" }));
  await fill("Name", "Grafana");
  await fill("Password", "the passphrase");
  await fireEvent.click(screen.getByRole("button", { name: "Create" }));

  await screen.findByLabelText("The new token");
  expect(screen.queryByRole("button", { name: "Copy" })).toBeNull();
});

it("revokes a token after asking, warning when it is this device's", async () => {
  signIn({ secret: "tpa_old", id: "atk_a" });
  const backend = withPassword();
  show(backend);

  await fireEvent.click(await screen.findByRole("button", { name: "Revoke Grafana" }));
  expect(screen.getByRole("dialog").textContent).not.toContain("This device is signed out.");
  await fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Revoke" }));
  await waitFor(() => expect(backend.deleteAccessToken).toHaveBeenCalledWith("atk_b"));

  await fireEvent.click(await screen.findByRole("button", { name: "Revoke Laptop" }));
  expect(screen.getByRole("dialog").textContent).toContain("This device is signed out.");
});

it("says why a revocation failed", async () => {
  show(
    withPassword({
      deleteAccessToken: vi.fn(async () => {
        throw refused(503, "engine-unavailable", "Not now.");
      }),
    }),
  );
  await fireEvent.click(await screen.findByRole("button", { name: "Revoke Grafana" }));
  await fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Revoke" }));

  expect((await screen.findByRole("alert")).textContent).toContain("starting or stopping");
});

it("offers trying again when the page cannot be read", async () => {
  const getPassword = vi
    .fn<Backend["getPassword"]>()
    .mockRejectedValueOnce(new TypeError("offline"))
    .mockResolvedValue(set);
  show(withPassword({ getPassword }));

  await fireEvent.click(await screen.findByRole("button", { name: "Try again" }));
  expect(await screen.findByText("Laptop")).toBeTruthy();
});
