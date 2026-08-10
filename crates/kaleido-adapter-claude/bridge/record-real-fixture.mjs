import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { spawn } from "node:child_process";
import { homedir } from "node:os";
import readline from "node:readline";

// This recorder invokes the pinned SDK bridge.  It deliberately has no fake
// provider path: an authentication or network failure is recorded as the
// real SDK error frame and remains a useful fail-closed fixture.
const bridgeDir = resolve(process.cwd());
const toyRoot = resolve(bridgeDir, "../tests/fixtures/sandbox/toy-project");
const fixturePath = resolve(bridgeDir, "../tests/fixtures/sandbox/real-sdk-simple-turn.jsonl");
mkdirSync(toyRoot, { recursive: true });

const child = spawn(process.execPath, ["--experimental-strip-types", "index.ts"], {
  cwd: bridgeDir,
  stdio: ["pipe", "pipe", "ignore"],
});
const lines = readline.createInterface({ input: child.stdout });
const pending = [];
let wake;
lines.on("line", (line) => {
  if (wake) {
    const resolveLine = wake;
    wake = undefined;
    resolveLine(line);
  } else {
    pending.push(line);
  }
});

function nextLine(timeoutMs) {
  const queued = pending.shift();
  if (queued !== undefined) return Promise.resolve(queued);
  return new Promise((resolveLine) => {
    wake = resolveLine;
    setTimeout(() => {
      if (wake === resolveLine) {
        wake = undefined;
        resolveLine(undefined);
      }
    }, timeoutMs).unref();
  });
}

function send(kind, payload) {
  child.stdin.write(`${JSON.stringify({
    v: 1,
    protocol: "onekaleidoscope.claude.sidecar",
    kind,
    payload,
  })}\n`);
}

const frames = [];
send("start", { cwd: toyRoot });
const ready = await nextLine(30_000);
if (ready) frames.push(ready);
send("prompt", {
  turn_id: "turn-real-1",
  text: "Reply with exactly KALEIDO SDK SIMPLE TURN",
});
for (let index = 0; index < 100; index += 1) {
  const line = await nextLine(40_000);
  if (line === undefined) break;
  frames.push(line);
  try {
    const parsed = JSON.parse(line);
    const message = parsed?.payload?.message;
    if (
      parsed?.kind === "error" ||
      (parsed?.kind === "sdk_message" && message?.type === "result")
    ) {
      break;
    }
  } catch {
    break;
  }
}
send("close", {});
child.stdin.end();
child.kill();
lines.close();
child.stdout.destroy();

const redactedRoot = "<sandbox/toy-project>";
const userHome = homedir();
function redact(value) {
  if (typeof value === "string") {
    if (
      value === toyRoot ||
      value === userHome ||
      value.startsWith(`${userHome}/`) ||
      value.startsWith(`${userHome}\\`) ||
      /^[A-Za-z]:[\\/]/.test(value) ||
      value.includes("\\Users\\") ||
      value.includes("/Users/") ||
      value.includes("/home/") ||
      value.includes("\\home\\")
    ) {
      return "<redacted-path>";
    }
    return value;
  }
  if (Array.isArray(value)) return value.map(redact);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, redact(entry)]));
  }
  return value;
}
const sanitized = frames.map((line) => {
  try {
    return JSON.stringify(redact(JSON.parse(line)));
  } catch {
    return line;
  }
});
let recording = `${sanitized.join("\n")}\n`;
recording = recording.replaceAll(toyRoot, redactedRoot);
mkdirSync(dirname(fixturePath), { recursive: true });
writeFileSync(fixturePath, recording, "utf8");
console.log(`recorded ${frames.length} real SDK sidecar frames to ${relative(bridgeDir, fixturePath)}`);
