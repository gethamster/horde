// Embedded by the Horde binary. The app provides Playwright as a pinned dev dependency.
import readline from 'node:readline';
import { createRequire } from 'node:module';
import path from 'node:path';

const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
const iterator = input[Symbol.asyncIterator]();
const send = value => process.stdout.write(`${JSON.stringify(value)}\n`);
const next = async () => {
  const row = await iterator.next();
  if (row.done) throw new Error('browser controller closed');
  if (row.value.length > 64 * 1024) throw new Error('browser command too large');
  return JSON.parse(row.value);
};
let browser, context, page, origin, screenshot;
let version = 0;
let current = new Map();
let external = [];
const clip = (value, size = 256) => String(value ?? '').replace(/\s+/g, ' ').trim().slice(0, size);
async function screenshotIfPossible() {
  if (page && screenshot) {
    try { await page.screenshot({ path: screenshot, fullPage: false, timeout: 3000 }); } catch { /* evidence is best effort */ }
  }
}
async function observe(ack = null) {
  if (new URL(page.url()).origin !== origin) throw new Error('browser left the app origin');
  const locator = page.locator('button, a[href], input, select, textarea, [role="button"], [role="link"]');
  const count = await locator.count();
  if (count > 128) throw new Error('page has more than 128 controls');
  const controls = [];
  const map = new Map();
  for (let n = 0; n < count; n += 1) {
    const handle = await locator.nth(n).elementHandle();
    if (!handle || !(await handle.isVisible()) || !(await handle.isEnabled())) continue;
    const info = await handle.evaluate(element => {
      const tag = element.tagName.toLowerCase();
      const type = (element.getAttribute('type') ?? '').toLowerCase();
      const role = (element.getAttribute('role') ?? '').toLowerCase();
      const label = element.getAttribute('aria-label') || element.labels?.[0]?.innerText || element.getAttribute('placeholder') || element.innerText || element.getAttribute('value') || '';
      const operations = tag === 'input' && type === 'file' ? [] :
        (tag === 'select' ? ['select'] : ((tag === 'textarea' || (tag === 'input' && !['button', 'submit', 'checkbox', 'radio', 'hidden', 'file'].includes(type))) ? ['fill'] : ['click']));
      return { label, operations, fingerprint: [tag, type, role, element.getAttribute('href'), element.getAttribute('name'), label].join('|') };
    });
    if (info.operations.length === 0) { external.push('file upload control unsupported'); continue; }
    const id = `c${n}`;
    controls.push({ id, label: clip(info.label), operations: info.operations });
    map.set(id, { handle, fingerprint: info.fingerprint, operations: info.operations });
  }
  version += 1;
  current = map;
  send({ kind: 'observation', ack, version, url: page.url(), controls, unsupported: external.splice(0) });
}
async function act(command) {
  const target = current.get(command.id);
  if (command.version !== version || !target || !target.operations.includes(command.operation)) throw new Error('stale or unsupported browser target');
  const fresh = await target.handle.evaluate(element => {
    if (!element.isConnected) return null;
    const tag = element.tagName.toLowerCase();
    const type = (element.getAttribute('type') ?? '').toLowerCase();
    const role = (element.getAttribute('role') ?? '').toLowerCase();
    const label = element.getAttribute('aria-label') || element.labels?.[0]?.innerText || element.getAttribute('placeholder') || element.innerText || element.getAttribute('value') || '';
    return [tag, type, role, element.getAttribute('href'), element.getAttribute('name'), label].join('|');
  });
  if (fresh !== target.fingerprint || !(await target.handle.isVisible()) || !(await target.handle.isEnabled())) throw new Error('browser target changed since observation');
  if (command.operation === 'click') await target.handle.click({ timeout: 5000 });
  else if (command.operation === 'fill' && typeof command.value === 'string') await target.handle.fill(command.value, { timeout: 5000 });
  else if (command.operation === 'select' && typeof command.value === 'string') await target.handle.selectOption(command.value, { timeout: 5000 });
  else throw new Error('browser input requires a declared value');
  await page.waitForTimeout(150);
  await observe(command.decision);
}
async function assertIndependent(assertions, decision) {
  const reports = [];
  for (const item of assertions) {
    let passed = false;
    if (new URL(page.url()).origin !== origin) passed = false;
    else if (item.kind === 'url_path') passed = new URL(page.url()).pathname === item.path;
    else if (item.kind === 'text_visible') passed = await page.getByText(item.text, { exact: true }).first().isVisible().catch(() => false);
    reports.push({ kind: item.kind, passed });
  }
  await screenshotIfPossible();
  send({ kind: 'result', ack: decision, passed: reports.length > 0 && reports.every(row => row.passed) && external.length === 0, assertions: reports });
}
try {
  const start = await next();
  if (start.cmd !== 'start') throw new Error('expected browser start command');
  const requireFromWorkspace = createRequire(path.join(process.cwd(), 'package.json'));
  let chromium;
  try { ({ chromium } = requireFromWorkspace('playwright')); }
  catch { throw new Error('install pinned Playwright in the app workspace and run npx playwright install chromium'); }
  origin = new URL(start.url).origin;
  screenshot = start.screenshot;
  browser = await chromium.launch({ headless: true, env: { PATH: process.env.PATH ?? '', HOME: process.env.HOME ?? '', PLAYWRIGHT_BROWSERS_PATH: process.env.PLAYWRIGHT_BROWSERS_PATH ?? '' } });
  context = await browser.newContext({ serviceWorkers: 'block', permissions: [], acceptDownloads: false });
  await context.route('**/*', route => {
    try { if (new URL(route.request().url()).origin === origin) return route.continue(); }
    catch { /* reject malformed resource URL */ }
    external.push('cross-origin resource or navigation blocked');
    return route.abort();
  });
  if (typeof context.routeWebSocket !== 'function') {
    throw new Error('Playwright 1.48 or newer is required to block cross-origin WebSockets');
  }
  await context.routeWebSocket('**/*', webSocket => {
    try {
      const destination = new URL(webSocket.url());
      if (destination.protocol === 'ws:') destination.protocol = 'http:';
      else if (destination.protocol === 'wss:') destination.protocol = 'https:';
      else throw new Error('unsupported WebSocket scheme');
      if (destination.origin === origin) return webSocket.connectToServer();
    } catch { /* reject malformed WebSocket URL */ }
    external.push('cross-origin WebSocket blocked');
    return webSocket.close();
  });
  page = await context.newPage();
  page.on('popup', popup => { external.push('popup unsupported'); void popup.close(); });
  await page.goto(start.url, { waitUntil: 'domcontentloaded', timeout: 15000 });
  await observe();
  for (;;) {
    const command = await next();
    if (command.cmd === 'act') await act(command);
    else if (command.cmd === 'assert') { await assertIndependent(start.assertions, command.decision); break; }
    else throw new Error('unsupported browser command');
  }
} catch (error) {
  await screenshotIfPossible();
  send({ kind: 'unsupported', reason: clip(error.message, 512) });
} finally {
  if (context) await context.close().catch(() => {});
  if (browser) await browser.close().catch(() => {});
}
