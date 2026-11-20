import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { test } from 'node:test';

const source = `
import { serve } from './crates/pi-rpc/src/voice/bridge.mjs';
await serve(onState => ({
  async start() { onState('recording'); },
  async stop() { onState('transcribing'); return { text: 'hola 🌎', truncated: false }; },
  async cancel() {},
}));`;

async function check(input, code, expected) {
  const child = spawn(process.execPath, ['--input-type=module', '-e', source], {
    env: process.env.SystemRoot ? { SystemRoot: process.env.SystemRoot } : {},
    stdio: 'pipe',
  });
  const exited = new Promise(resolve => child.once('close', resolve));
  let stderr = '';
  child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  const actual = [];
  child.stdin.write('start\n');
  for await (const line of createInterface({ input: child.stdout })) {
    actual.push(line);
    if (line === '{"type":"recording"}') child.stdin.write(input);
  }
  assert.equal(await exited, code);
  assert.deepEqual(actual, expected);
  assert.equal(stderr, '');
}

const recording = '{"type":"recording"}';
test('start -> recording -> stop -> result', { timeout: 3000 }, () => check('stop\n', 0, [
  recording, '{"type":"transcribing"}', '{"type":"result","text":"hola 🌎","truncated":false}',
]));
test('cancel yields no transcript', { timeout: 3000 }, () => check('cancel\n', 0, [
  recording, '{"type":"cancelled"}',
]));
test('overlong input cancels without a transcript', { timeout: 3000 }, () => check('x'.repeat(33), 1, [
  recording, '{"type":"error","code":"protocol"}',
]));
