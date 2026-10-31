// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import createClient, { type Middleware } from "openapi-fetch";

import type { components, paths } from "./schema";

export type Schemas = components["schemas"];

/** An RFC 9457 Problem Details body, as the API answers every error. */
export type Problem = Schemas["Problem"];

/** A response the API refused: its status and its Problem Details body,
 * when it sent one. */
export class ApiError extends Error {
  readonly status: number;
  readonly problem: Problem | undefined;

  constructor(status: number, problem: Problem | undefined) {
    super(problem?.detail ?? `The request failed with status ${status}.`);
    this.name = "ApiError";
    this.status = status;
    this.problem = problem;
  }

  /** The problem type's name, such as `not-found`. */
  get type(): string | undefined {
    return this.problem?.type.split("/").pop();
  }
}

/** A conditional read: the representation and its ETag, or `unchanged`
 * when the one the client holds is current. */
export type Conditional<T> =
  { unchanged: true } | { unchanged: false; data: T; etag: string | undefined };

export interface HttpOptions {
  /** The access token's secret, or `undefined` without one. */
  token: () => string | undefined;
  /** Told when the API refuses the request's credential. */
  unauthorized: () => void;
  /** Where requests go, the page's own origin by default. */
  baseUrl?: string;
  fetch?: typeof globalThis.fetch;
}

/** The typed client of the API's contract, sending the access token and
 * reporting a refused credential. */
export function createHttp(options: HttpOptions) {
  const client = createClient<paths>({
    baseUrl: options.baseUrl ?? "",
    ...(options.fetch ? { fetch: options.fetch } : {}),
  });
  const credentials: Middleware = {
    onRequest({ request }) {
      const token = options.token();
      if (token !== undefined) {
        request.headers.set("Authorization", `Bearer ${token}`);
      }
      return request;
    },
    onResponse({ response }) {
      if (response.status === 401) {
        options.unauthorized();
      }
      return response;
    },
  };
  client.use(credentials);
  return client;
}

export type Http = ReturnType<typeof createHttp>;

/** The data of a successful response, or an `ApiError` for any other. */
export function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
  if (result.data !== undefined && result.response.ok) {
    return result.data;
  }
  throw new ApiError(result.response.status, problem(result.error));
}

/** A conditional read's result: `unchanged` for `304`, the data and its
 * ETag for a success, and an `ApiError` otherwise. */
export function conditional<T>(result: {
  data?: T;
  error?: unknown;
  response: Response;
}): Conditional<T> {
  if (result.response.status === 304) {
    return { unchanged: true };
  }
  const data = unwrap(result);
  return { unchanged: false, data, etag: result.response.headers.get("ETag") ?? undefined };
}

function problem(body: unknown): Problem | undefined {
  return typeof body === "object" && body !== null && "type" in body && "status" in body
    ? (body as Problem)
    : undefined;
}
