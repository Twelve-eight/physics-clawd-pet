// Clawd 桌宠 × oh-my-pi（omp / omp-zh）。
//
// omp 的扩展就是一个导出默认函数的 TS 文件：桌宠把它放进 omp 的 agent 目录
// （<agent>/extensions/clawd-pet/index.ts，见 src-tauri/src/omp.rs），omp 启动时加载。
// 它在 omp 进程里监听会话事件，翻译成桌宠认识的 hook 事件名，用和 Claude Code hook 一样的
// 本机 TCP 一行 JSON 协议发给宠物。宠物没开就连不上，静默跳过，不影响 omp。
//
// 端口和令牌由桌宠安装时写进同目录的 config.json，这个文件本身不含机器相关内容。
// 只依赖 Node 内置模块；omp 传进来的事件和上下文一律当 unknown，用到哪个字段就检查哪个字段。

import net from 'node:net';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const PROVIDER = 'omp';
const MAX_CALLS = 64;

type Handler = (event: unknown, ctx: unknown) => unknown;
interface OmpApi {
  on(name: string, handler: Handler): void;
}
interface ToolCall {
  tool_name: string;
  tool: string;
}

const truncate = (s: string, max: number) => (s.length <= max ? s : `${s.slice(0, max - 1)}…`);

/** unknown 对象上的一个字段；不是对象或没有这个字段就是 undefined */
function field(value: unknown, key: string): unknown {
  return value && typeof value === 'object' ? Reflect.get(value, key) : undefined;
}

const text = (value: unknown) => (typeof value === 'string' ? value.trim() : '');

function loadConfig(): { port: number; token: string } | null {
  try {
    const dir = path.dirname(fileURLToPath(import.meta.url));
    const raw: unknown = JSON.parse(fs.readFileSync(path.join(dir, 'config.json'), 'utf8'));
    const port = field(raw, 'port');
    const token = field(raw, 'token');
    if (typeof port === 'number' && Number.isInteger(port) && typeof token === 'string') return { port, token };
  } catch {
    // 没装好配置：整个扩展安静地什么也不做
  }
  return null;
}

/** 工具参数里挑出气泡上显示的一行：命令、文件、网址…… */
function toolSummary(input: unknown): string {
  if (!input || typeof input !== 'object') return '';
  for (const key of ['command', 'cmd', 'path', 'file_path', 'filePath', 'url', 'query', 'pattern', 'description', 'prompt']) {
    const v = text(Reflect.get(input, key));
    if (v) return truncate(v, 400);
  }
  const first = Object.values(input).map(text).find(Boolean);
  return truncate(first ?? '', 400);
}

/** 失败的工具：isError，或者 shell 退出码不是 0（只留一句简短说明） */
function failure(event: unknown): string | undefined {
  if (field(event, 'isError') === true) return 'Tool failed';
  const content = field(event, 'content');
  const output = Array.isArray(content)
    ? content.map((b: unknown) => (field(b, 'type') === 'text' ? text(field(b, 'text')) : '')).join('\n')
    : '';
  const m = /\[exit code: (-?\d+)\]\s*$/.exec(output);
  return m && m[1] !== '0' ? `Exit code: ${m[1]}` : undefined;
}

function sessionId(ctx: unknown): string {
  const manager = field(ctx, 'sessionManager');
  for (const name of ['getSessionId', 'getSessionFile']) {
    const fn = field(manager, name);
    if (typeof fn !== 'function') continue;
    try {
      const id = text(Reflect.apply(fn, manager, []));
      if (id) return id;
    } catch {
      // 换下一个来源
    }
  }
  return 'default';
}

export default function clawdPetExtension(omp: OmpApi): void {
  const cfg = loadConfig();
  if (!cfg) return;
  // 宠物靠进程 id 找“该跑过去敲的窗口”：omp 自己和它的父进程（终端）
  const ancestors = [process.pid, process.ppid];
  const calls = new Map<string, ToolCall>();
  /** session_stop 标出的“这一轮可能结束了”的会话，等 agent_end 确认 omp 不再续跑 */
  const stopCandidates = new Set<string>();

  function send(msg: Record<string, unknown>): void {
    const sock = net.connect({ host: '127.0.0.1', port: cfg!.port });
    const done = () => sock.destroy();
    sock.setTimeout(3000, done);
    sock.on('error', done);
    sock.on('connect', () => {
      sock.write(`${JSON.stringify({ ...msg, provider: PROVIDER, t: Date.now(), token: cfg!.token, ancestors })}\n`);
      sock.end();
    });
  }

  // 没有界面的后台 worker 不是用户能回去的对话，不报
  const reports = (ctx: unknown) => field(ctx, 'hasUI') !== false;
  const base = (ctx: unknown) => ({ session_id: `omp:${sessionId(ctx)}`, cwd: text(field(ctx, 'cwd')) });

  // session_switch / session_branch：终端换到另一个会话，被离开的那个没有 shutdown
  const lifecycle: [string, string][] = [
    ['session_start', 'SessionStart'],
    ['session_switch', 'SessionStart'],
    ['session_branch', 'SessionStart'],
    ['before_agent_start', 'UserPromptSubmit'],
    ['session_before_compact', 'PreCompact'],
    ['session_compact', 'PostCompact'],
    ['session_shutdown', 'SessionEnd'],
  ];
  for (const [name, event] of lifecycle) {
    omp.on(name, (_ev, ctx) => {
      if (reports(ctx)) send({ ...base(ctx), event });
    });
  }

  omp.on('tool_call', (ev, ctx) => {
    if (!reports(ctx)) return;
    const call: ToolCall = { tool_name: text(field(ev, 'toolName')), tool: toolSummary(field(ev, 'input')) };
    const id = text(field(ev, 'toolCallId'));
    if (id) {
      calls.set(id, call);
      if (calls.size > MAX_CALLS) calls.delete(calls.keys().next().value ?? '');
    }
    send({ ...base(ctx), event: 'PreToolUse', ...call });
  });

  omp.on('tool_result', (ev, ctx) => {
    if (!reports(ctx)) return;
    const id = text(field(ev, 'toolCallId'));
    const call = calls.get(id) ?? { tool_name: text(field(ev, 'toolName')), tool: '' };
    calls.delete(id);
    const error = failure(ev);
    send({ ...base(ctx), event: error ? 'PostToolUseFailure' : 'PostToolUse', ...call, ...(error ? { error } : {}) });
  });

  // 一轮结束分两步：session_stop 只标候选；agent_end 看 omp 是不是还要自动续跑，真停了才报 Stop
  omp.on('session_stop', (_ev, ctx) => {
    if (reports(ctx)) stopCandidates.add(sessionId(ctx));
    return undefined;
  });

  omp.on('agent_end', (ev, ctx) => {
    const key = sessionId(ctx);
    if (!stopCandidates.delete(key) || field(ev, 'willContinue') === true) return undefined;
    send({ ...base(ctx), event: 'Stop' });
    return undefined;
  });
}
