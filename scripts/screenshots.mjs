// Screenshots of the page for the README: builds pr-roundup, starts its
// hidden `demo` subcommand (src/demo.rs), drives headless Chrome over the
// DevTools protocol, and writes the screenshots into docs/images/.
// `mise run screenshots` runs it.
//
// Chrome is `$CHROME`, else `google-chrome` on PATH. PNGs go through
// oxipng when it's installed. The page's emoji are drawn with a pinned
// Twemoji release (https://github.com/mozilla/twemoji-colr, CC-BY 4.0),
// injected only into the shots, so they come out the same whatever emoji
// font the machine has, or without one.

import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";

const OUT = "docs/images";

// `until`, if it finds an element, is where the shot stops, else it's the
// whole page. With `tall`, the window is as tall as the page. `fit`,
// instead, is the element the shot is cropped to. `scale` is device pixels
// per CSS pixel, 1 unless given.
const SHOTS = [
  { file: "board.png", path: "/", width: 1200, tall: true },
  { file: "board-dark.png", path: "/", width: 1200, tall: true, dark: true },
];

const TWEMOJI = {
  url: "https://github.com/mozilla/twemoji-colr/releases/download/v0.7.0/Twemoji.Mozilla.ttf",
  sha256: "6d90152ee0d29e82fe2a87793af5aa4b7ad13e6538360889e141e81ed299ee8e",
};

// Run last first, each whether or not the one before it failed. Whatever
// starts a process or makes a dir adds its cleanup as soon as it has.
const cleanups = [];

async function main() {
  const emoji = await emojiFont();
  const demo = await startDemo();
  const port = await startChrome();
  mkdirSync(OUT, { recursive: true });
  for (const shot of SHOTS) {
    const png = await capture(port, new URL(shot.path, demo).href, { ...shot, emoji });
    const file = join(OUT, shot.file);
    writeFileSync(file, png);
    console.log(`wrote ${file}`);
  }
  if (spawnSync("oxipng", ["--version"]).status === 0) {
    run("oxipng", ["-o", "4", "--strip", "safe", ...SHOTS.map((s) => join(OUT, s.file))]);
  }
}

// The pinned Twemoji font, checked against its hash, as base64.
async function emojiFont() {
  const response = await fetch(TWEMOJI.url);
  if (!response.ok) throw new Error(`fetching ${TWEMOJI.url}: ${response.status}`);
  const font = Buffer.from(await response.arrayBuffer());
  const sha256 = createHash("sha256").update(font).digest("hex");
  if (sha256 !== TWEMOJI.sha256) throw new Error(`${TWEMOJI.url} has sha256 ${sha256}, not ${TWEMOJI.sha256}`);
  return font.toString("base64");
}

// Builds the demo and starts it; resolves with its URL.
async function startDemo() {
  const build = run("cargo", [
    "build", "--locked", "--bin", "pr-roundup", "--message-format=json-render-diagnostics",
  ]);
  const exe = build
    .split("\n")
    .filter((line) => line.startsWith("{"))
    .map((line) => JSON.parse(line))
    .find((msg) => msg.reason === "compiler-artifact" && msg.target?.name === "pr-roundup" && msg.executable)?.executable;
  if (!exe) throw new Error("cargo built no pr-roundup executable");
  const child = spawn(exe, ["demo"], { stdio: ["ignore", "pipe", "inherit"] });
  cleanups.push(() => stop(child));
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => reject(new Error(`the demo exited with ${code}`)));
    createInterface({ input: child.stdout }).once("line", resolve);
  });
}

// Starts headless Chrome with a fresh profile; resolves with its DevTools
// port.
async function startChrome() {
  const tmp = mkdtempSync(join(tmpdir(), "pr-roundup-screenshots-"));
  // Added first, so it runs once Chrome has stopped writing to it.
  cleanups.push(() => rmSync(tmp, { recursive: true, force: true }));
  const profile = join(tmp, "profile");
  const child = spawn(
    process.env.CHROME || "google-chrome",
    [
      "--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`,
      "--no-first-run", "--no-default-browser-check", "--hide-scrollbars", "--disable-gpu",
      "about:blank",
    ],
    { stdio: "ignore" },
  );
  cleanups.push(() => stop(child));
  // Not found, or gone before it's listening.
  let gone = null;
  child.once("error", (err) => (gone ??= err));
  child.once("exit", (code) => (gone ??= new Error(`Chrome exited with ${code}`)));
  const portFile = join(profile, "DevToolsActivePort");
  for (let i = 0; i < 100; i++) {
    if (gone) throw gone;
    // Read until it has a port, since Chrome may not have written it yet.
    const port = existsSync(portFile) && readFileSync(portFile, "utf8").split("\n")[0];
    if (port) return port;
    await sleep(100);
  }
  throw new Error("Chrome didn't start");
}

// A PNG of `url` at `width`; see SHOTS.
async function capture(port, url, { width, tall, dark, prepare, until, fit, scale = 1, emoji }) {
  const target = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: "PUT" })).json();
  const page = await connect(target.webSocketDebuggerUrl);
  try {
    await page.send("Page.enable");
    // A tall shot starts short, so the page's height is its content's
    // rather than the window's.
    await page.send("Emulation.setDeviceMetricsOverride", {
      width, height: tall ? 100 : 900, deviceScaleFactor: scale, mobile: false,
    });
    await page.send("Emulation.setEmulatedMedia", {
      features: [{ name: "prefers-color-scheme", value: dark ? "dark" : "light" }],
    });
    // The page writes some times in the browser's zone and locale.
    await page.send("Emulation.setTimezoneOverride", { timezoneId: "UTC" });
    await page.send("Emulation.setLocaleOverride", { locale: "en-US" });
    const loaded = page.once("Page.loadEventFired");
    const { errorText } = await page.send("Page.navigate", { url });
    if (errorText) throw new Error(`opening ${url}: ${errorText}`);
    await loaded;
    // Fonts and deferred scripts.
    await sleep(500);
    // Twemoji ahead of the page's own fonts, for emoji only: the ranges
    // cover the emoji blocks, so text keeps the page's fonts.
    const injected = await page.send("Runtime.evaluate", {
      awaitPromise: true,
      expression: `(async () => {
        const style = document.createElement("style");
        style.textContent = \`@font-face {
          font-family: "Screenshot Emoji";
          src: url(data:font/ttf;base64,${emoji}) format("truetype");
          unicode-range: U+2300-23FF, U+2600-27BF, U+2B00-2BFF, U+1F000-1FAFF;
        }\`;
        document.head.append(style);
        document.body.style.fontFamily = '"Screenshot Emoji", ' + getComputedStyle(document.body).fontFamily;
        await document.fonts.load('16px "Screenshot Emoji"', "\\u2705");
        await document.fonts.ready;
      })()`,
    });
    if (injected.exceptionDetails) {
      throw new Error(`injecting the emoji font: ${injected.exceptionDetails.exception?.description ?? injected.exceptionDetails.text}`);
    }
    // Only a page the demo served whole is worth a shot, not an error page.
    const { result, exceptionDetails } = await page.send("Runtime.evaluate", {
      returnByValue: true,
      expression: `(() => {
        const status = performance.getEntriesByType("navigation")[0].responseStatus;
        if (status !== 200) return { status };
        ${prepare ?? ""};
        const box = ${fit ?? "null"}?.getBoundingClientRect();
        if (box) return { status, width: Math.ceil(box.width), height: Math.ceil(box.height) };
        const stop = ${until ?? "null"};
        const height = stop ? stop.getBoundingClientRect().top + window.scrollY : document.documentElement.scrollHeight;
        return { status, width: ${width}, height: Math.ceil(height) };
      })()`,
    });
    if (exceptionDetails) {
      throw new Error(`preparing ${url}: ${exceptionDetails.exception?.description ?? exceptionDetails.text}`);
    }
    const { status, width: shown, height } = result.value;
    if (status !== 200) throw new Error(`${url} answered ${status}`);
    if (tall) {
      await page.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: scale, mobile: false });
      await sleep(200);
    }
    const { data } = await page.send("Page.captureScreenshot", {
      format: "png",
      captureBeyondViewport: true,
      clip: { x: 0, y: 0, width: shown, height, scale: 1 },
    });
    return Buffer.from(data, "base64");
  } finally {
    page.close();
    // Best effort: if Chrome is gone, so is the tab.
    await fetch(`http://127.0.0.1:${port}/json/close/${target.id}`).catch(() => {});
  }
}

// A DevTools session over `ws`: `send` a method, or wait `once` for an event.
async function connect(ws) {
  const socket = new WebSocket(ws);
  await new Promise((resolve, reject) => {
    socket.onopen = resolve;
    socket.onerror = reject;
  });
  let next = 0;
  const pending = new Map();
  const waiting = new Map();
  socket.onmessage = ({ data }) => {
    const msg = JSON.parse(data);
    if (msg.id !== undefined) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
    } else if (waiting.has(msg.method)) {
      waiting.get(msg.method).resolve(msg.params);
      waiting.delete(msg.method);
    }
  };
  // Chrome gone: fail whatever still waits on it, rather than hang.
  const closed = new Error("the DevTools connection closed");
  socket.onclose = () => {
    for (const { reject } of [...pending.values(), ...waiting.values()]) reject(closed);
    pending.clear();
    waiting.clear();
  };
  return {
    send: (method, params = {}) =>
      new Promise((resolve, reject) => {
        if (socket.readyState !== WebSocket.OPEN) return reject(closed);
        const id = next++;
        pending.set(id, { resolve, reject });
        socket.send(JSON.stringify({ id, method, params }));
      }),
    once: (method) => {
      const event = new Promise((resolve, reject) => waiting.set(method, { resolve, reject }));
      // Closing rejects it even when nothing awaits it any more.
      event.catch(() => {});
      return event;
    },
    close: () => socket.close(),
  };
}

// Runs `cmd` to completion, in `env` if given, failing if it does;
// returns its stdout.
function run(cmd, args, env) {
  const result = spawnSync(cmd, args, {
    encoding: "utf8", stdio: ["ignore", "pipe", "inherit"], maxBuffer: 1 << 26, env,
  });
  if (result.status !== 0) throw new Error(`${cmd} failed: ${result.error ?? `exit ${result.status}`}`);
  return result.stdout;
}

// Asks `child` to stop, and waits for it; kills it if it takes too long.
async function stop(child) {
  if (child.pid === undefined || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise((resolve) => child.once("exit", resolve));
  child.kill("SIGTERM");
  // Unref'd, so it doesn't hold the script open once the child is gone.
  sleep(5000, undefined, { ref: false }).then(() => child.kill("SIGKILL"));
  await exited;
}

try {
  await main();
} catch (err) {
  console.error(err);
  process.exitCode = 1;
} finally {
  for (const cleanup of cleanups.reverse()) {
    try {
      await cleanup();
    } catch (err) {
      console.error(err);
      process.exitCode = 1;
    }
  }
}
