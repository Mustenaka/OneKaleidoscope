import { listSessions, query } from "@anthropic-ai/claude-agent-sdk";
import type {
  CanUseTool,
  PermissionResult,
  Query,
  SDKMessage,
  SDKSessionInfo,
  SDKUserMessage,
} from "@anthropic-ai/claude-agent-sdk";
import type { AskUserQuestionInput } from "@anthropic-ai/claude-agent-sdk/sdk-tools";
import * as readline from "node:readline";

const PROTOCOL = "onekaleidoscope.claude.sidecar" as const;
const VERSION = 1 as const;

type JsonObject = Record<string, unknown>;
type SidecarFrame = {
  v: typeof VERSION;
  protocol: typeof PROTOCOL;
  kind: string;
  payload: unknown;
};

type PermissionAnswer = {
  decision: "allow" | "allow_always" | "deny" | "cancel";
};

class AsyncQueue<T> implements AsyncIterable<T>, AsyncIterator<T> {
  private readonly pending: Array<(result: IteratorResult<T>) => void> = [];
  private readonly values: Array<{
    value: T;
    consumed: (accepted: boolean) => void;
  }> = [];
  private ended = false;

  push(value: T): Promise<boolean> {
    if (this.ended) {
      return Promise.resolve(false);
    }
    return new Promise((consumed) => {
      const waiter = this.pending.shift();
      if (waiter) {
        waiter({ done: false, value });
        consumed(true);
      } else {
        this.values.push({ value, consumed });
      }
    });
  }

  close(): void {
    this.ended = true;
    while (this.values.length > 0) {
      this.values.shift()?.consumed(false);
    }
    while (this.pending.length > 0) {
      this.pending.shift()?.({ done: true, value: undefined });
    }
  }

  next(): Promise<IteratorResult<T>> {
    const queued = this.values.shift();
    if (queued !== undefined) {
      queued.consumed(true);
      return Promise.resolve({ done: false, value: queued.value });
    }
    if (this.ended) {
      return Promise.resolve({ done: true, value: undefined });
    }
    return new Promise((resolve) => this.pending.push(resolve));
  }

  [Symbol.asyncIterator](): AsyncIterator<T> {
    return this;
  }
}

let cwd: string | undefined;
let sessionId: string | undefined;
let requestedResume = false;
let currentTurn: string | undefined;
let inputQueue: AsyncQueue<SDKUserMessage> | undefined;
let activeQuery: Query | undefined;
let started = false;
const permissionWaiters = new Map<
  string,
  (answer: PermissionAnswer) => void
>();
const questionWaiters = new Map<
  string,
  {
    input: AskUserQuestionInput;
    resolve: (result: PermissionResult) => void;
  }
>();

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isAskUserQuestionInput(
  input: Record<string, unknown>,
): input is Record<string, unknown> & AskUserQuestionInput {
  if (!Array.isArray(input.questions) || input.questions.length < 1 || input.questions.length > 4) {
    return false;
  }
  return input.questions.every(
    (question) =>
      isObject(question) &&
      typeof question.question === "string" &&
      question.question.length > 0 &&
      typeof question.header === "string" &&
      Array.isArray(question.options) &&
      question.options.length >= 2 &&
      question.options.length <= 4 &&
      question.options.every(
        (option) =>
          isObject(option) &&
          typeof option.label === "string" &&
          option.label.length > 0 &&
          typeof option.description === "string",
      ) &&
      typeof question.multiSelect === "boolean",
  );
}

function emit(kind: string, payload: unknown): void {
  const frame: SidecarFrame = {
    v: VERSION,
    protocol: PROTOCOL,
    kind,
    payload,
  };
  process.stdout.write(`${JSON.stringify(frame)}\n`);
}

function error(code: string): void {
  emit("error", { code });
}

function messageSessionId(message: SDKMessage): string | undefined {
  if (isObject(message) && typeof message.session_id === "string") {
    return message.session_id;
  }
  return sessionId;
}

function makeUserMessage(text: string): SDKUserMessage {
  return {
    type: "user",
    message: { role: "user", content: text },
    parent_tool_use_id: null,
    priority: "now",
    shouldQuery: true,
  };
}

function permissionCallback(): CanUseTool {
  return async (toolName, input, options): Promise<PermissionResult> => {
    const requestId = options.requestId;
    if (toolName === "AskUserQuestion" || toolName === "ask_user_question") {
      if (!isAskUserQuestionInput(input)) {
        return {
          behavior: "deny",
          message: "AskUserQuestion input failed the pinned SDK shape check",
        };
      }
      emit("question_request", {
        request_id: requestId,
        tool_name: toolName,
        questions: input.questions,
      });
      return new Promise((resolve) => {
        questionWaiters.set(requestId, { input, resolve });
      });
    }
    emit("permission_request", {
      request_id: requestId,
      tool_name: toolName,
      input,
      tool_use_id: options.toolUseID,
      title: options.title,
      display_name: options.displayName,
      description: options.description,
    });
    return new Promise((resolve) => {
      permissionWaiters.set(requestId, (answer) => {
        if (answer.decision === "allow" || answer.decision === "allow_always") {
          resolve({
            behavior: "allow",
            decisionClassification:
              answer.decision === "allow_always" ? "user_permanent" : "user_temporary",
          });
        } else {
          resolve({
            behavior: "deny",
            message: answer.decision === "cancel" ? "Cancelled by the user" : "Denied by the user",
            interrupt: answer.decision === "cancel",
            decisionClassification: "user_reject",
          });
        }
      });
    });
  };
}

async function consume(queryHandle: Query): Promise<void> {
  try {
    for await (const message of queryHandle) {
      const observedSession = messageSessionId(message);
      if (observedSession && observedSession !== sessionId && cwd) {
        sessionId = observedSession;
        emit(requestedResume ? "session_resumed" : "session_started", {
          session_id: observedSession,
          cwd,
          resumed: requestedResume,
        });
      }
      emit("sdk_message", {
        session_id: observedSession,
        turn_id: currentTurn,
        message,
      });
    }
  } catch {
    error("query_failed");
  }
}

async function start(payload: JsonObject): Promise<void> {
  if (started) {
    error("already_started");
    return;
  }
  if (typeof payload.cwd !== "string" || payload.cwd.length === 0) {
    error("missing_cwd");
    return;
  }
  cwd = payload.cwd;
  const resume = typeof payload.resume === "string" ? payload.resume : undefined;
  requestedResume = resume !== undefined;
  inputQueue = new AsyncQueue<SDKUserMessage>();
  try {
    activeQuery = query({
      prompt: inputQueue,
      options: {
        cwd,
        resume,
        canUseTool: permissionCallback(),
        persistSession: true,
      },
    });
    started = true;
    emit("ready", {
      sdk_version: "0.3.226",
      resume: resume !== undefined,
      cwd,
    });
    void consume(activeQuery);
  } catch {
    error("query_start_failed");
  }
}

async function list(payload: JsonObject): Promise<void> {
  if (typeof payload.cwd !== "string" || payload.cwd.length === 0) {
    error("missing_cwd");
    return;
  }
  try {
    const sessions: SDKSessionInfo[] = await listSessions({ dir: payload.cwd });
    emit("session_list", {
      cwd: payload.cwd,
      sessions: sessions.map((entry) => ({
        session_id: entry.sessionId,
        summary: entry.summary,
        last_modified: entry.lastModified,
      })),
    });
  } catch {
    error("session_list_failed");
  }
}

async function handle(frame: unknown): Promise<void> {
  if (!isObject(frame) || frame.v !== VERSION || frame.protocol !== PROTOCOL) {
    error("invalid_frame");
    return;
  }
  const payload = isObject(frame.payload) ? frame.payload : {};
  switch (frame.kind) {
    case "start":
      await start(payload);
      break;
    case "list_sessions":
      await list(payload);
      break;
    case "prompt": {
      if (!started || !inputQueue || typeof payload.text !== "string") {
        error("prompt_unavailable");
        break;
      }
      if (typeof payload.turn_id !== "string") {
        error("missing_turn_id");
        break;
      }
      currentTurn = payload.turn_id;
      const accepted = await inputQueue.push(makeUserMessage(payload.text));
      if (accepted) {
        emit("prompt_accepted", { turn_id: currentTurn });
      } else {
        error("prompt_not_consumed");
      }
      break;
    }
    case "permission_result": {
      if (typeof payload.request_id !== "string") {
        error("missing_request_id");
        break;
      }
      const waiter = permissionWaiters.get(payload.request_id);
      if (!waiter) {
        error("unknown_permission_request");
        break;
      }
      permissionWaiters.delete(payload.request_id);
      const decision = payload.decision;
      if (
        decision !== "allow" &&
        decision !== "allow_always" &&
        decision !== "deny" &&
        decision !== "cancel"
      ) {
        error("invalid_permission_decision");
        break;
      }
      waiter({ decision });
      emit("permission_result", { request_id: payload.request_id, decision });
      break;
    }
    case "question_result": {
      if (typeof payload.request_id !== "string" || !Array.isArray(payload.answers)) {
        error("invalid_question_result");
        break;
      }
      const waiter = questionWaiters.get(payload.request_id);
      if (!waiter || payload.answers.length !== waiter.input.questions.length) {
        error("unknown_question_request");
        break;
      }
      const answers: Record<string, string> = {};
      let valid = true;
      for (let index = 0; index < waiter.input.questions.length; index += 1) {
        const answer = payload.answers[index];
        if (
          !isObject(answer) ||
          answer.question_index !== index ||
          !Array.isArray(answer.values) ||
          answer.values.length === 0 ||
          !answer.values.every((value) => typeof value === "string" && value.length > 0)
        ) {
          valid = false;
          break;
        }
        answers[waiter.input.questions[index].question] = answer.values.join(", ");
      }
      if (!valid) {
        error("invalid_question_answers");
        break;
      }
      questionWaiters.delete(payload.request_id);
      waiter.resolve({
        behavior: "allow",
        updatedInput: { ...waiter.input, answers },
      });
      emit("question_result", {
        request_id: payload.request_id,
        answers: payload.answers,
      });
      break;
    }
    case "interrupt": {
      if (!activeQuery) {
        error("interrupt_unavailable");
        break;
      }
      try {
        const receipt = await activeQuery.interrupt();
        emit("interrupt_result", { cancelled: true, receipt });
      } catch {
        error("interrupt_failed");
      }
      break;
    }
    case "close":
      activeQuery?.close();
      inputQueue?.close();
      activeQuery = undefined;
      inputQueue = undefined;
      started = false;
      break;
    default:
      error("unknown_command");
      break;
  }
}

process.stdin.setEncoding("utf8");
const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
lines.on("line", (line) => {
  if (line.trim().length === 0) {
    return;
  }
  try {
    void handle(JSON.parse(String(line)) as unknown);
  } catch {
    error("malformed_input");
  }
});

process.on("uncaughtException", () => {
  error("bridge_crash");
  process.exitCode = 1;
});
