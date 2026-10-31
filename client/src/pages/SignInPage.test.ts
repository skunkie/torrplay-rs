// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { beforeEach, expect, it, vi } from "vitest";

import { ApiError } from "../lib/api/http";
import type { Backend } from "../lib/backend";
import { backendContext } from "../lib/context";
import { location, navigate } from "../lib/navigation.svelte";
import { session, signOut } from "../lib/session.svelte";
import { fakeBackend } from "../testing";
import SignInPage from "./SignInPage.svelte";

function backend(signIn: Backend["signIn"]): Backend {
  return fakeBackend({ signIn });
}

async function submit(password: string): Promise<void> {
  await fireEvent.input(screen.getByLabelText("Password"), { target: { value: password } });
  await fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
}

beforeEach(() => {
  signOut();
  navigate("/sign-in?next=%2Flibrary%3Fkind%3Dseries");
});

it("keeps the token and returns to the page that asked", async () => {
  const signIn = vi.fn(async () => "tpa_secret");
  render(SignInPage, { context: backendContext(backend(signIn)) });
  await submit("correct horse");
  await waitFor(() => expect(location.path).toBe("/library"));
  expect(location.search).toBe("?kind=series");
  expect(signIn).toHaveBeenCalledWith("correct horse", "Web browser");
  expect(session.token).toBe("tpa_secret");
  expect(localStorage.getItem("torrplay.access-token")).toBe("tpa_secret");
});

it("says when the password is wrong", async () => {
  const refused = new ApiError(403, {
    type: "tag:torrplay,2026:problem/invalid-credentials",
    title: "Invalid credentials",
    status: 403,
  });
  render(SignInPage, {
    context: backendContext(backend(vi.fn(async () => Promise.reject(refused)))),
  });
  await submit("wrong horse");
  expect(await screen.findByRole("alert")).toHaveTextContent("The password is not correct.");
  expect(session.token).toBeUndefined();
  expect(location.path).toBe("/sign-in");
});
