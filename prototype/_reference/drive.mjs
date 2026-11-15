// Drive the prototype in headless Chrome over CDP: click through states, capture screenshots,
// and report any runtime error. Read-only: the page only touches its own local files.
import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, 'shots');
mkdirSync(outDir, { recursive: true });
// Chrome path comes from CHROME_BIN (or the usual per-OS defaults); never hardcoded here.
const chrome =
  process.env.CHROME_BIN ||
  (process.platform === 'win32'
    ? join(process.env['PROGRAMFILES'] || 'C:\\Program Files', 'Google\\Chrome\\Application\\chrome.exe')
    : process.platform === 'darwin'
      ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
      : '/usr/bin/google-chrome');
const port = 9333;
// Page URL defaults to ../index.html next to this script; override with argv[2].
const pageUrl =
  process.argv[2] || new URL('../index.html', import.meta.url).href;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const proc = spawn(
  chrome,
  [
    '--headless=new',
    '--disable-gpu',
    '--no-sandbox',
    '--hide-scrollbars',
    '--no-first-run',
    '--disable-extensions',
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${join(tmpdir(), 'dsh-proto-chrome')}`,
    '--window-size=1400,1150',
    'about:blank',
  ],
  { stdio: 'ignore' },
);

let ws;
let seq = 0;
const pending = new Map();
const errors = [];

function send(method, params = {}) {
  const id = ++seq;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });
}

async function connect() {
  for (let i = 0; i < 40; i += 1) {
    try {
      const res = await fetch(`http://127.0.0.1:${port}/json/new?${encodeURIComponent('about:blank')}`, { method: 'PUT' });
      if (res.ok) {
        const target = await res.json();
        await open(target.webSocketDebuggerUrl);
        return;
      }
    } catch {
      /* not up yet */
    }
    await sleep(250);
  }
  throw new Error('chrome devtools endpoint never came up');
}

function open(url) {
  return new Promise((resolve, reject) => {
    ws = new WebSocket(url);
    ws.addEventListener('open', resolve);
    ws.addEventListener('error', reject);
    ws.addEventListener('message', (e) => {
      const msg = JSON.parse(e.data);
      if (msg.id && pending.has(msg.id)) {
        const { resolve: ok, reject: bad } = pending.get(msg.id);
        pending.delete(msg.id);
        if (msg.error) bad(new Error(msg.error.message));
        else ok(msg.result);
      } else if (msg.method === 'Runtime.exceptionThrown') {
        errors.push(msg.params.exceptionDetails?.exception?.description || msg.params.exceptionDetails?.text || 'unknown');
      } else if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') {
        errors.push(msg.params.args.map((a) => a.value ?? a.description).join(' '));
      }
    });
  });
}

async function evaluate(expression) {
  const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) {
    throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
  }
  return r.result.value;
}

async function go(hash) {
  await send('Page.navigate', { url: pageUrl + hash });
  await sleep(1000);
}

async function shot(name) {
  const { data } = await send('Page.captureScreenshot', { format: 'png' });
  writeFileSync(join(outDir, `state-${name}.png`), Buffer.from(data, 'base64'));
  console.log(`shot state-${name}.png`);
}

const click = (sel) => evaluate(`document.querySelector(${JSON.stringify(sel)}).click(); true`);
const setInput = (sel, value) =>
  evaluate(`(() => { const el = document.querySelector(${JSON.stringify(sel)}); el.value = ${JSON.stringify(value)}; el.dispatchEvent(new Event('input', { bubbles: true })); return el.value; })()`);

try {
  await connect();
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Log.enable');

  await go('#node');
  await shot('running');

  // 维护模式开关在设置页（节点页只有提示条），先切过去再拍
  await go('#settings');
  await click('[data-toggle="maintain"]');
  await sleep(500);
  await shot('maintain');
  await click('[data-toggle="maintain"]');
  await sleep(300);

  await go('#node');
  await click('[data-act="pod"]');
  await sleep(400);
  await shot('sheet-pod');
  await click('[data-act="close-sheet"]');
  await sleep(200);

  await go('#images');
  await click('[data-act="image"]');
  await sleep(400);
  await shot('sheet-image');
  await click('[data-act="close-sheet"]');
  await sleep(200);

  await click('[data-act="ask-pull"]');
  await sleep(300);
  await shot('sheet-pull');
  await setInput('[data-model="pullRef"]', 'nginx:1.27-alpine');
  await click('[data-act="do-pull"]');
  await sleep(1600);
  await shot('pull-progress');
  await sleep(3200);

  await go('#logs');
  await click('[data-act="toggle-pause"]');
  await sleep(3000);
  await shot('logs-paused');
  await click('[data-act="toggle-pause"]');

  await go('#settings');
  await setInput('[data-model="port"]', '80');
  await sleep(300);
  await shot('settings-invalid');

  await click('[data-act="go-join"]');
  await sleep(400);
  await shot('join-token');

  await click('[data-act="join-submit"]');
  await sleep(300);
  await shot('join-error');

  await click('[data-act="join-mode"][data-mode="file"]');
  await sleep(250);
  await click('[data-act="join-pick-file"]');
  await sleep(300);
  await shot('join-file');

  await click('[data-act="back"]');
  await sleep(300);

  // 配色对比：同一屏在 夜班 / 观测台 / 沙纸 下各来一张
  await click('[data-act="theme"][data-theme="paper"]');
  await sleep(250);
  await click('[data-act="go-join"]');
  await sleep(300);
  await shot('join-paper');
  await click('[data-act="back"]');
  await sleep(300);
  await shot('settings-paper');

  await evaluate("document.querySelector('#view').scrollTop = 760; true");
  await sleep(250);
  await shot('settings-paper-mid');

  await evaluate("document.querySelector('#view').scrollTop = 99999; true");
  await sleep(300);
  await shot('settings-paper-bottom');

  await go('#node');
  await sleep(300);
  await shot('node-paper');
  await go('#settings');
  await sleep(250);

  await click('[data-act="theme"][data-theme="night"]');
  await sleep(250);

  await go('#node');
  await click('[data-act="shot"]');
  await sleep(700);
  await shot('lightbox');
  await click('[data-act="close-lightbox"]');

  await go('#pods');
  await shot('pods');

  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 2, mobile: true });
  await go('#node');
  await sleep(600);
  await shot('mobile');

  const counts = await evaluate(`({ pods: document.querySelectorAll('[data-act="pod"]').length, tabs: document.querySelectorAll('.tab').length })`);
  console.log('dom check', JSON.stringify(counts));
  console.log('runtime errors:', errors.length ? errors.join('\n') : 'none');
} catch (e) {
  console.error('DRIVER ERROR', e.message);
  console.error('runtime errors:', errors.join('\n'));
  process.exitCode = 1;
} finally {
  try {
    ws?.close();
  } catch {
    /* ignore */
  }
  proc.kill();
}
