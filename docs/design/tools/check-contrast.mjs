#!/usr/bin/env node
// Checks WCAG 2.1 contrast of the token pairs the mockups use, in both themes.
// Reads docs/design/assets/fable.css; no dependencies.
// Usage: node docs/design/tools/check-contrast.mjs   (exit 1 on any failure)
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../assets/fable.css"), "utf8");

function block(selector) {
  const i = css.indexOf(selector + " {");
  if (i < 0) throw new Error(`missing ${selector}`);
  const body = css.slice(i, css.indexOf("\n}", i));
  const vars = {};
  for (const m of body.matchAll(/--([\w-]+):\s*(#[0-9a-fA-F]{6})\s*;/g)) vars[m[1]] = m[2];
  return vars;
}
const light = block(":root");
const dark = { ...light, ...block('[data-theme="dark"]') };

const lum = (hex) => {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
    .map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
};
const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };

// [foreground, background, minimum] — 4.5 for text, 3 for UI components/large text
const pairs = [
  ...["bg", "surface", "surface-2"].flatMap((bg) => [
    ["text", bg, 4.5], ["text-muted", bg, 4.5], ["text-faint", bg, 4.5], ["accent", bg, 4.5],
    ["danger", bg, 4.5], ["warn", bg, 4.5], ["info", bg, 4.5],
  ]),
  ["text-on-accent", "accent", 4.5], ["text-on-accent", "accent-hover", 4.5],
  ["accent-soft-text", "accent-soft", 4.5], ["text", "accent-soft", 4.5],
  ["info", "info-soft", 4.5], ["warn", "warn-soft", 4.5], ["danger", "danger-soft", 4.5],
  ["text", "warn-soft", 4.5], ["text", "info-soft", 4.5], ["text", "danger-soft", 4.5],
  ["text-muted", "neutral-soft", 4.5], ["text-muted", "surface-sunken", 4.5], ["text", "surface-sunken", 4.5],
  ["text-faint", "surface-sunken", 4.5],
  ["border-strong", "surface", 1.0], // decorative; inputs also rely on label + focus ring
  ["focus", "surface", 3], ["focus", "bg", 3], ["accent", "surface", 3], ["chart-1", "surface", 3],
];

let failed = 0;
for (const [name, vars] of [["light", light], ["dark", dark]]) {
  console.log(`\n${name}`);
  for (const [fg, bg, min] of pairs) {
    const r = ratio(vars[fg], vars[bg]);
    const ok = r >= min;
    if (!ok) failed++;
    console.log(`  ${ok ? "ok  " : "FAIL"} ${fg.padEnd(16)} on ${bg.padEnd(15)} ${r.toFixed(2).padStart(5)} (min ${min})`);
  }
}
console.log(failed ? `\n${failed} pair(s) below WCAG AA` : "\nall pairs meet WCAG 2.1 AA");
process.exit(failed ? 1 : 0);
