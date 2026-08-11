import { existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import * as readline from "node:readline";

const protocol = "onekaleidoscope.claude.sidecar";
function emit(kind, payload) {
  process.stdout.write(`${JSON.stringify({ v: 1, protocol, kind, payload })}\n`);
}

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
lines.on("line", (line) => {
  const command = JSON.parse(line);
  if (command.kind === "start") {
    const { cwd, resume } = command.payload;
    const marker = join(cwd, ".claude-sidecar-retry");
    if (resume === "retry-once" && !existsSync(marker)) {
      writeFileSync(marker, "failed once", "utf8");
      emit("error", { code: "query_start_failed" });
      return;
    }
    emit("ready", {
      sdk_version: "0.3.226",
      cwd,
      resume_session_id: resume ?? null,
    });
    return;
  }
  if (command.kind === "close") {
    emit("closed", {});
    return;
  }
  if (command.kind === "list_sessions") {
    emit("session_list", {
      cwd: command.payload.cwd,
      sessions: [{
        session_id: "fake-history-session",
        summary: "Fake history session",
        last_modified: 1_785_378_000_000,
      }],
    });
    return;
  }
  if (command.kind === "get_session_messages") {
    emit("session_messages", {
      cwd: command.payload.cwd,
      session_id: command.payload.session_id,
      offset: command.payload.offset,
      limit: command.payload.limit,
      next_offset: null,
      messages: [{
        role: "assistant",
        message_id: "fake-message-1",
        session_id: command.payload.session_id,
        parent_tool_use_id: null,
        parent_agent_id: null,
        message_json: "{\"role\":\"assistant\",\"content\":\"fixture-only\"}",
      }],
    });
    return;
  }
  emit("error", { code: "unexpected_test_command" });
});
