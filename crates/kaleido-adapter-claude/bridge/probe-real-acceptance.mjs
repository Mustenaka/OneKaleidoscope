import { existsSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawn } from "node:child_process";
import readline from "node:readline";

const PROTOCOL = "onekaleidoscope.claude.sidecar";
const VERSION = 1;
const FRAME_TIMEOUT_MS = 120_000;
const bridgeDir = resolve(process.cwd());
const bridgeScript = resolve(bridgeDir, "index.ts");
const probeRoot = resolve(tmpdir(), `onekaleidoscope-claude-acceptance-${process.pid}`);
const permissionAllowProbe = resolve(probeRoot, "permission-allow-probe.txt");
const permissionProbe = resolve(probeRoot, "permission-probe.txt");
mkdirSync(probeRoot, { recursive: true });

class Bridge {
  constructor() {
    this.child = spawn(process.execPath, ["--experimental-strip-types", bridgeScript], {
      cwd: bridgeDir,
      stdio: ["pipe", "pipe", "ignore"],
    });
    this.pending = [];
    this.waiter = undefined;
    this.lines = readline.createInterface({ input: this.child.stdout });
    this.lines.on("line", (line) => {
      if (this.waiter) {
        const waiter = this.waiter;
        this.waiter = undefined;
        waiter(line);
      } else {
        this.pending.push(line);
      }
    });
  }

  send(kind, payload) {
    this.child.stdin.write(`${JSON.stringify({ v: VERSION, protocol: PROTOCOL, kind, payload })}\n`);
  }

  nextLine(timeoutMs = FRAME_TIMEOUT_MS) {
    const queued = this.pending.shift();
    if (queued !== undefined) return Promise.resolve(queued);
    return new Promise((resolveLine) => {
      this.waiter = resolveLine;
      setTimeout(() => {
        if (this.waiter === resolveLine) {
          this.waiter = undefined;
          resolveLine(undefined);
        }
      }, timeoutMs).unref();
    });
  }

  async nextFrame(timeoutMs = FRAME_TIMEOUT_MS) {
    const line = await this.nextLine(timeoutMs);
    if (line === undefined) throw new Error("frame_timeout");
    let frame;
    try {
      frame = JSON.parse(line);
    } catch {
      throw new Error("malformed_sidecar_output");
    }
    if (frame?.v !== VERSION || frame?.protocol !== PROTOCOL) {
      throw new Error("unexpected_sidecar_envelope");
    }
    if (frame.kind === "error") throw new Error("sidecar_error");
    return frame;
  }

  async waitFor(predicate, onFrame = undefined) {
    const deadline = Date.now() + FRAME_TIMEOUT_MS;
    while (Date.now() < deadline) {
      const frame = await this.nextFrame(Math.max(1, deadline - Date.now()));
      if (onFrame) await onFrame(frame);
      if (predicate(frame)) return frame;
    }
    throw new Error("acceptance_timeout");
  }

  async close() {
    if (this.child.exitCode === null) {
      const exited = new Promise((resolveExit) => this.child.once("exit", resolveExit));
      this.send("close", {});
      try {
        await this.waitFor((frame) => frame.kind === "closed");
      } catch {
        // The process is still terminated below; callers already hold the
        // relevant acceptance result and never treat close failure as proof.
      }
      this.child.stdin.end();
      if (this.child.exitCode === null) this.child.kill();
      await Promise.race([
        exited,
        new Promise((resolveTimeout) => setTimeout(resolveTimeout, 5_000)),
      ]);
    }
    this.lines.close();
    this.child.stdout.destroy();
  }
}

async function removeProbeRoot() {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    try {
      rmSync(probeRoot, { recursive: true, force: true, maxRetries: 2, retryDelay: 100 });
      return;
    } catch (error) {
      if (attempt === 19 || error?.code !== "EBUSY") throw error;
      await new Promise((resolveDelay) => setTimeout(resolveDelay, 250));
    }
  }
}

function isSuccessfulResult(frame, turnId) {
  return (
    frame.kind === "sdk_event" &&
    frame.payload?.turn_id === turnId &&
    frame.payload?.event?.event === "result" &&
    frame.payload.event.subtype === "success" &&
    frame.payload.event.is_error === false
  );
}

const evidence = {
  successful_turn: false,
  question_answered: false,
  permission_allowed: false,
  permission_tool_use: false,
  permission_request: false,
  permission_result: false,
  permission_denied: false,
  interrupt_receipt: false,
  resumed_session: false,
  discovered_session: false,
  nonempty_history: false,
};
let first;
let resumed;
let sdkVersion;
let sessionId;

try {
  first = new Bridge();
  first.send("start", { cwd: probeRoot });
  const ready = await first.waitFor((frame) => frame.kind === "ready");
  sdkVersion = ready.payload?.sdk_version;

  const simpleTurn = "turn-acceptance-simple";
  first.send("prompt", {
    turn_id: simpleTurn,
    text: "Reply with exactly KALEIDO ACCEPTANCE READY. Do not use tools.",
  });
  const simpleResult = await first.waitFor((frame) => isSuccessfulResult(frame, simpleTurn), (frame) => {
    if (frame.kind === "session_started") sessionId = frame.payload?.session_id;
  });
  evidence.successful_turn = isSuccessfulResult(simpleResult, simpleTurn) && Boolean(sessionId);

  const questionTurn = "turn-acceptance-question";
  let questionRequestSeen = false;
  let questionResultSeen = false;
  first.send("prompt", {
    turn_id: questionTurn,
    text: "Use AskUserQuestion now. Ask one single-select question with header Color, question Which color?, and exactly two options Red and Blue. Do not answer it yourself and do not use other tools. After the answer, reply exactly QUESTION COMPLETE.",
  });
  await first.waitFor((frame) => isSuccessfulResult(frame, questionTurn), (frame) => {
    if (frame.kind === "question_request" && !questionRequestSeen) {
      const questions = frame.payload?.questions;
      const firstOption = questions?.[0]?.options?.[0]?.label;
      if (questions?.length !== 1 || typeof firstOption !== "string" || firstOption.length === 0) {
        throw new Error("invalid_question_request");
      }
      questionRequestSeen = true;
      first.send("question_result", {
        request_id: frame.payload.request_id,
        answers: [{ question_index: 0, values: [firstOption] }],
      });
    }
    if (frame.kind === "question_result") questionResultSeen = true;
    if (frame.kind === "permission_request") {
      first.send("permission_result", {
        request_id: frame.payload.request_id,
        decision: "deny",
      });
    }
  });
  evidence.question_answered = questionRequestSeen && questionResultSeen;

  const permissionAllowTurn = "turn-acceptance-permission-allow";
  let permissionAllowRequestSeen = false;
  let permissionAllowResultSeen = false;
  first.send("prompt", {
    turn_id: permissionAllowTurn,
    text: "Use the Write tool exactly once to create permission-allow-probe.txt in the current directory with the text KALEIDO_PERMISSION_ALLOW. Do not use any other tool. After it succeeds, reply exactly PERMISSION ALLOWED.",
  });
  await first.waitFor((frame) => isSuccessfulResult(frame, permissionAllowTurn), (frame) => {
    if (frame.kind === "permission_request" && !permissionAllowRequestSeen) {
      permissionAllowRequestSeen = true;
      first.send("permission_result", {
        request_id: frame.payload.request_id,
        decision: "allow",
      });
    }
    if (frame.kind === "permission_result") permissionAllowResultSeen = true;
    if (frame.kind === "question_request") throw new Error("unexpected_question_request");
  });
  if (permissionAllowRequestSeen && !permissionAllowResultSeen) {
    await first.waitFor((frame) => frame.kind === "permission_result", (frame) => {
      if (frame.kind === "permission_result") permissionAllowResultSeen = true;
    });
  }
  evidence.permission_allowed =
    permissionAllowRequestSeen && permissionAllowResultSeen && existsSync(permissionAllowProbe);

  const permissionTurn = "turn-acceptance-permission";
  let permissionRequestSeen = false;
  let permissionResultSeen = false;
  first.send("prompt", {
    turn_id: permissionTurn,
    text: "Use the Write tool exactly once to create permission-probe.txt in the current directory with the text KALEIDO_PERMISSION_PROBE. Do not use any other tool. If permission is denied, reply exactly PERMISSION DENIED.",
  });
  await first.waitFor((frame) => isSuccessfulResult(frame, permissionTurn), (frame) => {
    if (
      frame.kind === "sdk_event" &&
      frame.payload?.turn_id === permissionTurn &&
      frame.payload?.event?.event === "assistant" &&
      frame.payload.event.blocks?.some((block) => block?.kind === "tool_use")
    ) {
      evidence.permission_tool_use = true;
    }
    if (frame.kind === "permission_request" && !permissionRequestSeen) {
      permissionRequestSeen = true;
      first.send("permission_result", {
        request_id: frame.payload.request_id,
        decision: "deny",
      });
    }
    if (frame.kind === "permission_result") permissionResultSeen = true;
    if (frame.kind === "question_request") throw new Error("unexpected_question_request");
  });
  if (permissionRequestSeen && !permissionResultSeen) {
    await first.waitFor((frame) => frame.kind === "permission_result", (frame) => {
      if (frame.kind === "permission_result") permissionResultSeen = true;
    });
  }
  evidence.permission_request = permissionRequestSeen;
  evidence.permission_result = permissionResultSeen;
  evidence.permission_denied =
    permissionRequestSeen && permissionResultSeen && !existsSync(permissionProbe);

  const interruptTurn = "turn-acceptance-interrupt";
  let interruptSent = false;
  first.send("prompt", {
    turn_id: interruptTurn,
    text: "Write a very long numbered explanation with at least 500 entries. Do not use tools.",
  });
  await first.waitFor((frame) => frame.kind === "interrupt_result", (frame) => {
    if (frame.kind === "prompt_accepted" && frame.payload?.turn_id === interruptTurn && !interruptSent) {
      interruptSent = true;
      first.send("interrupt", {});
    }
  });
  evidence.interrupt_receipt = interruptSent;
  await first.close();
  first = undefined;

  if (typeof sessionId !== "string" || sessionId.length === 0) throw new Error("missing_session_id");
  resumed = new Bridge();
  resumed.send("start", { cwd: probeRoot, resume: sessionId });
  await resumed.waitFor((frame) => frame.kind === "ready");
  const resumeTurn = "turn-acceptance-resume";
  let resumedFrameSeen = false;
  resumed.send("prompt", {
    turn_id: resumeTurn,
    text: "Reply with exactly KALEIDO RESUME READY. Do not use tools.",
  });
  await resumed.waitFor((frame) => isSuccessfulResult(frame, resumeTurn), (frame) => {
    if (frame.kind === "session_resumed" && frame.payload?.session_id === sessionId) {
      resumedFrameSeen = true;
    }
  });
  evidence.resumed_session = resumedFrameSeen;

  resumed.send("list_sessions", { cwd: probeRoot });
  const list = await resumed.waitFor((frame) => frame.kind === "session_list");
  evidence.discovered_session =
    Array.isArray(list.payload?.sessions) &&
    list.payload.sessions.some((session) => session?.session_id === sessionId);

  resumed.send("get_session_messages", {
    cwd: probeRoot,
    session_id: sessionId,
    offset: 0,
    limit: 100,
  });
  const history = await resumed.waitFor((frame) => frame.kind === "session_messages");
  evidence.nonempty_history =
    history.payload?.session_id === sessionId &&
    Array.isArray(history.payload?.messages) &&
    history.payload.messages.length > 0;

  const blockers = Object.entries(evidence)
    .filter(([, passed]) => !passed)
    .map(([name]) => name);
  console.log(
    JSON.stringify({
      gate: "Claude-real-provider-acceptance",
      result: blockers.length === 0 ? "pass" : "blocked",
      sdk_version: sdkVersion,
      evidence,
      blockers,
    }),
  );
  if (blockers.length !== 0) process.exitCode = 1;
} catch {
  console.log(
    JSON.stringify({
      gate: "Claude-real-provider-acceptance",
      result: "blocked",
      sdk_version: sdkVersion ?? null,
      evidence,
      blockers: ["provider_probe_failed"],
    }),
  );
  process.exitCode = 1;
} finally {
  if (first) await first.close();
  if (resumed) await resumed.close();
  await removeProbeRoot();
}
