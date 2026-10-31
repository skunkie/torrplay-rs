// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import js from "@eslint/js";
import tailwind from "eslint-plugin-better-tailwindcss";
import perfectionist from "eslint-plugin-perfectionist";
import svelte from "eslint-plugin-svelte";
import globals from "globals";
import ts from "typescript-eslint";

import svelteConfig from "./svelte.config.js";

export default ts.config(
  { ignores: ["dist/", "node_modules/", "src/lib/api/schema.d.ts"] },
  js.configs.recommended,
  ...ts.configs.strict,
  ...ts.configs.stylistic,
  ...svelte.configs.recommended,
  {
    languageOptions: { globals: { ...globals.browser, ...globals.node } },
    plugins: { perfectionist },
    rules: {
      // Destructuring drops a member into `_` to copy the rest.
      "@typescript-eslint/no-unused-vars": [
        "error",
        { ignoreRestSiblings: true, varsIgnorePattern: "^_" },
      ],
      // An empty arrow function is a deliberate no-op, as in test doubles.
      "@typescript-eslint/no-empty-function": ["error", { allow: ["arrowFunctions"] }],
      "@typescript-eslint/consistent-type-imports": ["error", { fixStyle: "inline-type-imports" }],
      // Only imports and exports are sorted: the order of object members,
      // interfaces, and unions often carries meaning, such as the
      // contract's order or the order a menu shows.
      "perfectionist/sort-imports": [
        "error",
        {
          type: "natural",
          groups: ["side-effect", ["builtin", "external"], ["parent", "sibling", "index"]],
          newlinesBetween: 1,
        },
      ],
      "perfectionist/sort-named-imports": ["error", { type: "natural" }],
      "perfectionist/sort-named-exports": ["error", { type: "natural" }],
      "perfectionist/sort-exports": ["error", { type: "natural" }],
    },
  },
  {
    files: ["**/*.svelte", "**/*.svelte.ts"],
    languageOptions: {
      parserOptions: {
        parser: ts.parser,
        extraFileExtensions: [".svelte"],
        svelteConfig,
      },
    },
  },
  {
    files: ["**/*.svelte"],
    plugins: { "better-tailwindcss": tailwind },
    settings: { "better-tailwindcss": { entryPoint: "src/app.css" } },
    rules: {
      ...tailwind.configs["recommended-error"].rules,
      // Prettier wraps the markup.
      "better-tailwindcss/enforce-consistent-line-wrapping": "off",
      // Components color with the theme's tokens, so both themes change in
      // one place: app.css removes Tailwind's palette, and arbitrary
      // colors are refused.
      "better-tailwindcss/no-restricted-classes": [
        "error",
        {
          restrict: [
            {
              pattern:
                "^(?:.*:)?(?:bg|text|border(?:-[trblxyse])?|outline|ring|fill|stroke|shadow|decoration|accent|caret|divide|from|via|to)-\\[.+\\]$",
              message: "Color with a theme token, not an arbitrary value.",
            },
          ],
        },
      ],
      "svelte/shorthand-attribute": "error",
      "svelte/shorthand-directive": "error",
    },
  },
);
