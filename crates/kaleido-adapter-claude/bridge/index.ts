import {
  getSessionMessages,
  listSessions,
  query,
} from "@anthropic-ai/claude-agent-sdk";
import type {
  CanUseTool,
  PermissionResult,
  Query,
  SDKAssistantMessage,
  SDKMessage,
  SDKSessionInfo,
  SDKUserMessage,
  SessionMessage,
} from "@anthropic-ai/claude-agent-sdk";
import type { AskUserQuestionInput } from "@anthropic-ai/claude-agent-sdk/sdk-tools";
import { isAbsolute } from "node:path";
import * as readline from "node:readline";

const PROTOCOL = "onekaleidoscope.claude.sidecar" as const;
const VERSION = 1 as const;
const SDK_VERSION = "0.3.226" as const;
const MAX_HISTORY_PAGE = 100;
const MAX_SESSION_LIST = 1_000;
const SESSION_LIST_PAGE = 100;

type JsonObject = Record<string, unknown>;
type PermissionAnswer = {
  decision: "allow" | "allow_always" | "deny" | "cancel";
};
type Question = AskUserQuestionInput["questions"][number];

type OwnedAssistantBlock =
  | { kind: "text"; item_id: string; text: string }
  | { kind: "thinking"; item_id: string; text: string }
  | { kind: "tool_use"; item_id: string; name: string; input_json: string }
  | { kind: "ignored"; item_id: string; label: string };

type OwnedUserBlock =
  | { kind: "text"; text: string }
  | { kind: "tool_result"; tool_use_id: string; content_json: string; is_error: boolean }
  | { kind: "ignored"; label: string };

type OwnedSdkEvent =
  | { event: "init"; cwd: string; capabilities: string[] }
  | { event: "assistant"; message_id: string; error: string | null; blocks: OwnedAssistantBlock[] }
  | { event: "user"; message_id: string; blocks: OwnedUserBlock[] }
  | { event: "stream_text"; block_index: number; text: string }
  | { event: "tool_progress"; tool_use_id: string }
  | { event: "tool_summary"; tool_use_ids: string[]; summary: string }
  | {
      event: "result";
      subtype:
        | "success"
        | "error_during_execution"
        | "error_max_turns"
        | "error_max_budget_usd"
        | "error_max_structured_output_retries";
      is_error: boolean;
      stop_reason: string | null;
      errors: string[];
    }
  | { event: "ignored"; label: string };

type OwnedSessionMessage = {
  role: "user" | "assistant" | "system";
  message_id: string;
  session_id: string;
  parent_tool_use_id: string | null;
  parent_agent_id: string | null;
  message_json: string;
};

type SidecarFrame =
  | {
      kind: "ready";
      payload: {
        sdk_version: typeof SDK_VERSION;
        cwd: string;
        resume_session_id: string | null;
      };
    }
  | {
      kind: "session_started" | "session_resumed";
      payload: { session_id: string; cwd: string };
    }
  | {
      kind: "session_list";
      payload: {
        cwd: string;
        sessions: Array<{ session_id: string; summary: string; last_modified: number }>;
      };
    }
  | {
      kind: "session_messages";
      payload: {
        cwd: string;
        session_id: string;
        offset: number;
        limit: number;
        next_offset: number | null;
        messages: Array<{
          role: "user" | "assistant" | "system";
          message_id: string;
          session_id: string;
          parent_tool_use_id: string | null;
          parent_agent_id: string | null;
          message_json: string;
        }>;
      };
    }
  | { kind: "prompt_accepted"; payload: { turn_id: string } }
  | {
      kind: "permission_request";
      payload: {
        request_id: string;
        tool_name: string;
        input_json: string;
        tool_use_id: string | null;
        title: string | null;
      };
    }
  | {
      kind: "permission_result";
      payload: { request_id: string; decision: PermissionAnswer["decision"] };
    }
  | {
      kind: "question_request";
      payload: {
        request_id: string;
        tool_name: string;
        questions: Array<{
          question: string;
          header: string;
          multi_select: boolean;
          options: Array<{ label: string; description: string }>;
        }>;
      };
    }
  | {
      kind: "question_result";
      payload: {
        request_id: string;
        answers: Array<{ question_index: number; values: string[] }>;
      };
    }
  | {
      kind: "interrupt_result";
      payload: { cancelled: true; still_queued: string[] };
    }
  | {
      kind: "sdk_event";
      payload: { session_id: string; turn_id: string | null; event: OwnedSdkEvent };
    }
  | { kind: "closed"; payload: Record<string, never> }
  | { kind: "error"; payload: { code: string } };

class AsyncQueue<T> implements AsyncIterable<T>, AsyncIterator<T> {
  private readonly pending: Array<(result: IteratorResult<T>) => void> = [];
  private readonly values: Array<{ value: T; consumed: (accepted: boolean) => void }> = [];
  private ended = false;

  push(value: T): Promise<boolean> {
    if (this.ended) return Promise.resolve(false);
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
    while (this.values.length > 0) this.values.shift()?.consumed(false);
    while (this.pending.length > 0) this.pending.shift()?.({ done: true, value: undefined });
  }

  next(): Promise<IteratorResult<T>> {
    const queued = this.values.shift();
    if (queued) {
      queued.consumed(true);
      return Promise.resolve({ done: false, value: queued.value });
    }
    if (this.ended) return Promise.resolve({ done: true, value: undefined });
    return new Promise((resolve) => this.pending.push(resolve));
  }

  [Symbol.asyncIterator](): AsyncIterator<T> {
    return this;
  }
}

let activeCwd: string | undefined;
let observedSessionId: string | undefined;
let requestedResumeId: string | undefined;
let currentTurn: string | undefined;
let inputQueue: AsyncQueue<SDKUserMessage> | undefined;
let activeQuery: Query | undefined;
let started = false;
let suppressQueryFailure = false;
const permissionWaiters = new Map<string, (answer: PermissionAnswer) => void>();
const questionWaiters = new Map<
  string,
  { input: AskUserQuestionInput; resolve: (result: PermissionResult) => void }
>();

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function assertNever(value: never): never {
  throw new Error(`unhandled pinned SDK variant: ${String(value)}`);
}

function jsonString(value: unknown): string {
  return JSON.stringify(value) ?? "null";
}

function emit(frame: SidecarFrame): void {
  process.stdout.write(`${JSON.stringify({ v: VERSION, protocol: PROTOCOL, ...frame })}\n`);
}

function fail(code: string): void {
  emit({ kind: "error", payload: { code } });
}

function isQuestion(value: unknown): value is Question {
  if (!isObject(value)) return false;
  return (
    typeof value.question === "string" &&
    value.question.length > 0 &&
    typeof value.header === "string" &&
    value.header.length > 0 &&
    typeof value.multiSelect === "boolean" &&
    Array.isArray(value.options) &&
    value.options.length >= 2 &&
    value.options.length <= 4 &&
    value.options.every(
      (option) =>
        isObject(option) &&
        typeof option.label === "string" &&
        option.label.length > 0 &&
        typeof option.description === "string",
    )
  );
}

function askUserQuestionInput(input: Record<string, unknown>): AskUserQuestionInput | undefined {
  if (!Array.isArray(input.questions) || input.questions.length < 1 || input.questions.length > 4) {
    return undefined;
  }
  if (!input.questions.every(isQuestion)) return undefined;
  switch (input.questions.length) {
    case 1:
      return { questions: [input.questions[0]] };
    case 2:
      return { questions: [input.questions[0], input.questions[1]] };
    case 3:
      return { questions: [input.questions[0], input.questions[1], input.questions[2]] };
    case 4:
      return {
        questions: [
          input.questions[0],
          input.questions[1],
          input.questions[2],
          input.questions[3],
        ],
      };
    default:
      return undefined;
  }
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

function assistantError(error: SDKAssistantMessage["error"]): string | null {
  if (error === undefined) return null;
  switch (error) {
    case "authentication_failed":
    case "oauth_org_not_allowed":
    case "billing_error":
    case "rate_limit":
    case "overloaded":
    case "invalid_request":
    case "model_not_found":
    case "server_error":
    case "unknown":
    case "max_output_tokens":
      return error;
    default:
      return assertNever(error);
  }
}

function assistantBlocks(message: SDKAssistantMessage): OwnedAssistantBlock[] {
  return message.message.content.map((block, index) => {
    const fallbackId = `${message.uuid}:${index}`;
    switch (block.type) {
      case "text":
        return { kind: "text", item_id: fallbackId, text: block.text };
      case "thinking":
        return { kind: "thinking", item_id: fallbackId, text: block.thinking };
      case "tool_use":
        return {
          kind: "tool_use",
          item_id: block.id,
          name: block.name,
          input_json: jsonString(block.input),
        };
      case "redacted_thinking":
      case "server_tool_use":
      case "web_search_tool_result":
      case "web_fetch_tool_result":
      case "code_execution_tool_result":
      case "bash_code_execution_tool_result":
      case "text_editor_code_execution_tool_result":
      case "tool_search_tool_result":
      case "container_upload":
      case "mcp_tool_use":
      case "mcp_tool_result":
      case "advisor_tool_result":
      case "compaction":
      case "fallback":
        return { kind: "ignored", item_id: fallbackId, label: block.type };
      default:
        return assertNever(block);
    }
  });
}

function userBlocks(message: SDKUserMessage): OwnedUserBlock[] {
  const content = message.message.content;
  if (typeof content === "string") return [{ kind: "text", text: content }];
  return content.map((block) => {
    switch (block.type) {
      case "text":
        return { kind: "text", text: block.text };
      case "tool_result":
        return {
          kind: "tool_result",
          tool_use_id: block.tool_use_id,
          content_json: jsonString(block.content),
          is_error: block.is_error ?? false,
        };
      case "image":
      case "document":
      case "thinking":
      case "redacted_thinking":
      case "mid_conv_system":
      case "search_result":
      case "tool_use":
      case "server_tool_use":
      case "web_search_tool_result":
      case "web_fetch_tool_result":
      case "code_execution_tool_result":
      case "bash_code_execution_tool_result":
      case "text_editor_code_execution_tool_result":
      case "tool_search_tool_result":
      case "container_upload":
        return { kind: "ignored", label: block.type };
      default:
        return assertNever(block);
    }
  });
}

function ignoredTopLevel(
  message: Extract<
    SDKMessage,
    { type: "auth_status" | "rate_limit_event" | "prompt_suggestion" | "conversation_reset" }
  >,
): OwnedSdkEvent {
  switch (message.type) {
    case "auth_status":
    case "rate_limit_event":
    case "prompt_suggestion":
    case "conversation_reset":
      return { event: "ignored", label: message.type };
    default:
      return assertNever(message);
  }
}

function systemEvent(message: Extract<SDKMessage, { type: "system" }>): OwnedSdkEvent {
  switch (message.subtype) {
    case "init":
      return { event: "init", cwd: message.cwd, capabilities: message.capabilities ?? [] };
    case "compact_boundary":
    case "status":
    case "api_retry":
    case "control_request_progress":
    case "model_refusal_fallback":
    case "model_refusal_no_fallback":
    case "local_command_output":
    case "hook_started":
    case "hook_progress":
    case "hook_response":
    case "plugin_install":
    case "task_notification":
    case "task_started":
    case "task_updated":
    case "task_progress":
    case "background_tasks_changed":
    case "thinking_tokens":
    case "session_state_changed":
    case "worker_shutting_down":
    case "commands_changed":
    case "notification":
    case "files_persisted":
    case "memory_recall":
    case "elicitation_complete":
    case "permission_denied":
    case "mirror_error":
    case "informational":
      return { event: "ignored", label: `system:${message.subtype}` };
    default:
      return assertNever(message);
  }
}

function sdkEvent(message: SDKMessage): OwnedSdkEvent {
  switch (message.type) {
    case "assistant":
      return {
        event: "assistant",
        message_id: message.uuid,
        error: assistantError(message.error),
        blocks: assistantBlocks(message),
      };
    case "user":
      return {
        event: "user",
        message_id: message.uuid ?? "sdk-user",
        blocks: userBlocks(message),
      };
    case "stream_event": {
      const event = message.event;
      if (
        event.type === "content_block_delta" &&
        event.delta.type === "text_delta"
      ) {
        return {
          event: "stream_text",
          block_index: event.index,
          text: event.delta.text,
        };
      }
      return { event: "ignored", label: `stream:${event.type}` };
    }
    case "tool_progress":
      return { event: "tool_progress", tool_use_id: message.tool_use_id };
    case "tool_use_summary":
      return {
        event: "tool_summary",
        tool_use_ids: message.preceding_tool_use_ids,
        summary: message.summary,
      };
    case "result":
      return {
        event: "result",
        subtype: message.subtype,
        is_error: message.is_error,
        stop_reason: message.stop_reason,
        errors: message.subtype === "success" ? [] : message.errors,
      };
    case "system":
      return systemEvent(message);
    case "auth_status":
    case "rate_limit_event":
    case "prompt_suggestion":
    case "conversation_reset":
      return ignoredTopLevel(message);
    default:
      return assertNever(message);
  }
}

function messageSessionId(message: SDKMessage): string | undefined {
  return message.session_id ?? observedSessionId;
}

function observeSessionIdentity(message: SDKMessage): string {
  const candidate = messageSessionId(message);
  if (!candidate || candidate.length === 0) throw new Error("missing_session_id");
  if (requestedResumeId && candidate !== requestedResumeId) throw new Error("resume_id_mismatch");
  if (observedSessionId && candidate !== observedSessionId) throw new Error("session_id_changed");
  if (message.type === "system" && message.subtype === "init" && message.cwd !== activeCwd) {
    throw new Error("cwd_mismatch");
  }
  if (!observedSessionId) {
    observedSessionId = candidate;
    if (!activeCwd) throw new Error("missing_active_cwd");
    emit({
      kind: requestedResumeId ? "session_resumed" : "session_started",
      payload: { session_id: candidate, cwd: activeCwd },
    });
  }
  return candidate;
}

function permissionCallback(): CanUseTool {
  return async (toolName, input, options): Promise<PermissionResult> => {
    const requestId = options.requestId;
    if (toolName === "AskUserQuestion" || toolName === "ask_user_question") {
      const questionInput = askUserQuestionInput(input);
      if (!questionInput) {
        return {
          behavior: "deny",
          message: "AskUserQuestion input failed the pinned SDK shape check",
        };
      }
      emit({
        kind: "question_request",
        payload: {
          request_id: requestId,
          tool_name: toolName,
          questions: questionInput.questions.map((question) => ({
            question: question.question,
            header: question.header,
            multi_select: question.multiSelect,
            options: question.options.map((option) => ({
              label: option.label,
              description: option.description,
            })),
          })),
        },
      });
      return new Promise((resolve) => {
        questionWaiters.set(requestId, { input: questionInput, resolve });
      });
    }
    emit({
      kind: "permission_request",
      payload: {
        request_id: requestId,
        tool_name: toolName,
        input_json: jsonString(input),
        tool_use_id: options.toolUseID ?? null,
        title: options.title ?? null,
      },
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

function clearSessionState(): void {
  inputQueue?.close();
  activeQuery?.close();
  for (const waiter of permissionWaiters.values()) waiter({ decision: "cancel" });
  for (const waiter of questionWaiters.values()) {
    waiter.resolve({ behavior: "deny", message: "Claude sidecar session closed" });
  }
  permissionWaiters.clear();
  questionWaiters.clear();
  activeQuery = undefined;
  inputQueue = undefined;
  activeCwd = undefined;
  observedSessionId = undefined;
  requestedResumeId = undefined;
  currentTurn = undefined;
  started = false;
}

async function consume(queryHandle: Query): Promise<void> {
  try {
    for await (const message of queryHandle) {
      const sessionId = observeSessionIdentity(message);
      emit({
        kind: "sdk_event",
        payload: { session_id: sessionId, turn_id: currentTurn ?? null, event: sdkEvent(message) },
      });
    }
  } catch (caught) {
    if (!suppressQueryFailure) {
      const code = caught instanceof Error ? caught.message : "query_failed";
      fail(code);
    }
    clearSessionState();
  }
}

async function start(payload: JsonObject): Promise<void> {
  if (started) {
    fail("already_started");
    return;
  }
  if (typeof payload.cwd !== "string" || !isAbsolute(payload.cwd)) {
    fail("missing_cwd");
    return;
  }
  if (payload.resume !== undefined && payload.resume !== null && typeof payload.resume !== "string") {
    fail("invalid_resume");
    return;
  }
  const resume = typeof payload.resume === "string" ? payload.resume : undefined;
  if (resume !== undefined && resume.length === 0) {
    fail("invalid_resume");
    return;
  }
  activeCwd = payload.cwd;
  suppressQueryFailure = false;
  requestedResumeId = resume;
  inputQueue = new AsyncQueue<SDKUserMessage>();
  try {
    const queryHandle = query({
      prompt: inputQueue,
      options: {
        cwd: activeCwd,
        resume,
        canUseTool: permissionCallback(),
        persistSession: true,
      },
    });
    activeQuery = queryHandle;
    started = true;
    emit({
      kind: "ready",
      payload: { sdk_version: SDK_VERSION, cwd: activeCwd, resume_session_id: resume ?? null },
    });
    void consume(queryHandle);
  } catch {
    clearSessionState();
    fail("query_start_failed");
  }
}

async function list(payload: JsonObject): Promise<void> {
  if (typeof payload.cwd !== "string" || !isAbsolute(payload.cwd)) {
    fail("missing_cwd");
    return;
  }
  try {
    const sessions: SDKSessionInfo[] = [];
    for (let offset = 0; offset < MAX_SESSION_LIST; offset += SESSION_LIST_PAGE) {
      const page = await listSessions({
        dir: payload.cwd,
        includeWorktrees: false,
        limit: SESSION_LIST_PAGE,
        offset,
      });
      sessions.push(...page);
      if (page.length < SESSION_LIST_PAGE) break;
      if (offset + SESSION_LIST_PAGE >= MAX_SESSION_LIST) {
        fail("session_list_limit_exceeded");
        return;
      }
    }
    emit({
      kind: "session_list",
      payload: {
        cwd: payload.cwd,
        sessions: sessions.map((entry) => ({
          session_id: entry.sessionId,
          summary: entry.summary,
          last_modified: entry.lastModified,
        })),
      },
    });
  } catch {
    fail("session_list_failed");
  }
}

function historyMessage(message: SessionMessage): OwnedSessionMessage {
  switch (message.type) {
    case "user":
    case "assistant":
    case "system":
      return {
        role: message.type,
        message_id: message.uuid,
        session_id: message.session_id,
        parent_tool_use_id: message.parent_tool_use_id,
        parent_agent_id: message.parent_agent_id,
        message_json: jsonString(message.message),
      };
    default:
      return assertNever(message.type);
  }
}

async function getMessages(payload: JsonObject): Promise<void> {
  if (
    typeof payload.cwd !== "string" ||
    !isAbsolute(payload.cwd) ||
    typeof payload.session_id !== "string" ||
    payload.session_id.length === 0 ||
    !Number.isInteger(payload.offset) ||
    typeof payload.offset !== "number" ||
    payload.offset < 0 ||
    !Number.isInteger(payload.limit) ||
    typeof payload.limit !== "number" ||
    payload.limit < 1 ||
    payload.limit > MAX_HISTORY_PAGE
  ) {
    fail("invalid_history_request");
    return;
  }
  try {
    const messages = await getSessionMessages(payload.session_id, {
      dir: payload.cwd,
      offset: payload.offset,
      limit: payload.limit,
      includeSystemMessages: true,
    });
    emit({
      kind: "session_messages",
      payload: {
        cwd: payload.cwd,
        session_id: payload.session_id,
        offset: payload.offset,
        limit: payload.limit,
        next_offset: messages.length === payload.limit ? payload.offset + messages.length : null,
        messages: messages.map(historyMessage),
      },
    });
  } catch {
    fail("session_messages_failed");
  }
}

async function handle(frame: unknown): Promise<void> {
  if (!isObject(frame) || frame.v !== VERSION || frame.protocol !== PROTOCOL) {
    fail("invalid_frame");
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
    case "get_session_messages":
      await getMessages(payload);
      break;
    case "prompt": {
      if (!started || !inputQueue || typeof payload.text !== "string") {
        fail("prompt_unavailable");
        break;
      }
      if (typeof payload.turn_id !== "string" || payload.turn_id.length === 0) {
        fail("missing_turn_id");
        break;
      }
      currentTurn = payload.turn_id;
      const accepted = await inputQueue.push(makeUserMessage(payload.text));
      if (accepted) emit({ kind: "prompt_accepted", payload: { turn_id: payload.turn_id } });
      else fail("prompt_not_consumed");
      break;
    }
    case "permission_result": {
      if (typeof payload.request_id !== "string") {
        fail("missing_request_id");
        break;
      }
      const decision = payload.decision;
      if (
        decision !== "allow" &&
        decision !== "allow_always" &&
        decision !== "deny" &&
        decision !== "cancel"
      ) {
        fail("invalid_permission_decision");
        break;
      }
      const waiter = permissionWaiters.get(payload.request_id);
      if (!waiter) {
        fail("unknown_permission_request");
        break;
      }
      permissionWaiters.delete(payload.request_id);
      waiter({ decision });
      emit({
        kind: "permission_result",
        payload: { request_id: payload.request_id, decision },
      });
      break;
    }
    case "question_result": {
      if (typeof payload.request_id !== "string" || !Array.isArray(payload.answers)) {
        fail("invalid_question_result");
        break;
      }
      const waiter = questionWaiters.get(payload.request_id);
      if (!waiter || payload.answers.length !== waiter.input.questions.length) {
        fail("unknown_question_request");
        break;
      }
      const answers: Record<string, string> = {};
      const closedAnswers: Array<{ question_index: number; values: string[] }> = [];
      for (let index = 0; index < waiter.input.questions.length; index += 1) {
        const answer = payload.answers[index];
        if (
          !isObject(answer) ||
          answer.question_index !== index ||
          !Array.isArray(answer.values) ||
          answer.values.length === 0 ||
          !answer.values.every((value) => typeof value === "string" && value.length > 0)
        ) {
          fail("invalid_question_answers");
          return;
        }
        const values = answer.values as string[];
        answers[waiter.input.questions[index].question] = values.join(", ");
        closedAnswers.push({ question_index: index, values });
      }
      questionWaiters.delete(payload.request_id);
      waiter.resolve({ behavior: "allow", updatedInput: { ...waiter.input, answers } });
      emit({
        kind: "question_result",
        payload: { request_id: payload.request_id, answers: closedAnswers },
      });
      break;
    }
    case "interrupt": {
      if (!activeQuery) {
        fail("interrupt_unavailable");
        break;
      }
      try {
        const receipt = await activeQuery.interrupt();
        emit({
          kind: "interrupt_result",
          payload: { cancelled: true, still_queued: receipt?.still_queued ?? [] },
        });
      } catch {
        fail("interrupt_failed");
      }
      break;
    }
    case "close":
      suppressQueryFailure = true;
      clearSessionState();
      emit({ kind: "closed", payload: {} });
      break;
    default:
      fail("unknown_command");
      break;
  }
}

process.stdin.setEncoding("utf8");
const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
lines.on("line", (line) => {
  if (line.trim().length === 0) return;
  try {
    void handle(JSON.parse(String(line)) as unknown);
  } catch {
    fail("malformed_input");
  }
});

process.on("uncaughtException", () => {
  fail("bridge_crash");
  clearSessionState();
  process.exitCode = 1;
});
