#!/usr/bin/env node
import readline from 'node:readline';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// `MOCK_PI_IGNORE_EOF=1` switches to the #703 teardown shape: the mock stays
// deaf to stdin EOF (the CLI hang the EOF watchdog exists for) and handles
// SIGTERM itself, exiting 143 rather than dying from the signal — mirrors
// The mock intentionally ignores EOF and exits with the same signal status as the agent stub.
// and `mock-codex-app-server.mjs`'s `MOCK_CODEX_IGNORE_EOF` toggle.
const ignoreEof = process.env.MOCK_PI_IGNORE_EOF === '1';
if (ignoreEof) {
  process.on('SIGTERM', () => process.exit(143));
  // Keep the event loop alive so EOF alone can never end the process.
  setInterval(() => {}, 60_000);
}

const sessionId = '00000000-0000-4000-8000-0000000000pi';
const send = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);

for await (const line of readline.createInterface({ input: process.stdin })) {
  const command = JSON.parse(line);
  if (command.type === 'get_state') {
    send({
      id: command.id,
      type: 'response',
      command: 'get_state',
      success: true,
      data: {
        sessionId,
        thinkingLevel: 'medium',
        isStreaming: false,
        isCompacting: false,
        steeringMode: 'all',
        followUpMode: 'one-at-a-time',
        autoCompactionEnabled: true,
        messageCount: 0,
        pendingMessageCount: 0,
      },
    });
  } else if (command.type === 'prompt') {
    const message = typeof command.message === 'string' ? command.message : '';
    // `mock:slow` → hold the turn for ~25s so a wall-clock timeout is observable
    // (mirrors mock-claude.mjs's own `mock:slow`).
    if (message.includes('mock:slow')) await sleep(25_000);
    // `mock:done` → the reply ends with the DUCK:DONE completion marker (#347),
    // so the auto-close path is testable dry (mirrors mock-claude.mjs).
    const doneMarker = message.includes('mock:done') ? '\n\nDUCK:DONE' : '';
    const replyText = `Investigating: ${message}${doneMarker}`;

    send({ type: 'response', command: 'prompt', success: true });
    send({ type: 'agent_start' });
    send({ type: 'turn_start' });
    send({
      type: 'message_update',
      message: {},
      assistantMessageEvent: { type: 'text_start', contentIndex: 0, partial: {} },
    });
    send({
      type: 'message_update',
      message: {},
      assistantMessageEvent: {
        type: 'text_delta',
        contentIndex: 0,
        delta: replyText,
        partial: {},
      },
    });
    send({
      type: 'message_update',
      message: {},
      assistantMessageEvent: {
        type: 'text_end',
        contentIndex: 0,
        content: replyText,
        partial: {},
      },
    });
    send({ type: 'tool_execution_start', toolCallId: 'tool-1', toolName: 'read', args: { path: 'README.md' } });
    send({
      type: 'tool_execution_end',
      toolCallId: 'tool-1',
      toolName: 'read',
      result: { content: [{ type: 'text', text: 'mock file' }] },
      isError: false,
    });
    send({
      type: 'message_end',
      message: {
        role: 'assistant',
        usage: {
          input: 10,
          output: 5,
          cacheRead: 0,
          cacheWrite: 0,
          cost: { total: 0.001 },
        },
      },
    });
    send({ type: 'turn_end', message: {}, toolResults: [] });
    send({ type: 'agent_end', messages: [], willRetry: false });
    send({ type: 'agent_settled' });
  } else if (command.type === 'abort') {
    send({ type: 'response', command: 'abort', success: true });
  }
}
