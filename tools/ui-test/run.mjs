#!/usr/bin/env node
// ticket-tracker Web 面板 UI 回归测试（开发用）。
//
// 自动用隔离的临时目录和端口启动 tt-server（关闭监控），播种一条测试 watch，
// 然后用系统 Chrome 跑 Playwright 交互断言，结束后清理。
//
// 不会读写 ~/.local/state/ticket-tracker 下的生产库，也不会触发猫眼轮询 / 通知。
//
// 用法：
//   cd tools/ui-test && npm install && npm test
//   TT_SERVER_BIN=/path/to/tt-server TT_CHROME=/path/to/chrome npm test
//
// 前置：先构建服务端 `cd rs && cargo build --bin tt-server`。

import { chromium } from 'playwright-core';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import net from 'node:net';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, '../..');
const WATCH = 'w-ui-test';
const SEED_MOVIE = 999001;
const SEED_CINEMA_A = '880001';
const SEED_CINEMA_B = '880002';

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok: !!ok, detail });
  console.log((ok ? 'PASS  ' : 'FAIL  ') + name + (detail ? '   [' + detail + ']' : ''));
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function which(bin) {
  const r = spawnSync('which', [bin], { encoding: 'utf8' });
  return r.status === 0 ? r.stdout.trim() : null;
}

function findServerBin() {
  const bin = process.env.TT_SERVER_BIN || path.join(REPO, 'rs/target/debug/tt-server');
  if (!existsSync(bin)) {
    throw new Error(
      `找不到 tt-server：${bin}\n先执行：cd rs && cargo build --bin tt-server` +
      `\n或设置 TT_SERVER_BIN 指向已构建的二进制。`
    );
  }
  return bin;
}

function findChrome() {
  const candidates = [
    process.env.TT_CHROME,
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
    which('google-chrome'),
    which('google-chrome-stable'),
    which('chromium'),
    which('chromium-browser'),
  ].filter(Boolean);
  const found = candidates.find((p) => existsSync(p));
  if (!found) {
    throw new Error(
      '找不到 Chrome / Chromium。安装 Chrome，或设置 TT_CHROME 指向浏览器可执行文件。'
    );
  }
  return found;
}

function requireSqlite() {
  if (!which('sqlite3')) {
    throw new Error('找不到 sqlite3 CLI。macOS 自带；Ubuntu 可 apt-get install -y sqlite3。');
  }
}

function freePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.once('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const port = srv.address().port;
      srv.close(() => resolve(port));
    });
  });
}

async function waitForHealth(base, child, logs, timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`tt-server 启动失败（exit ${child.exitCode}）：\n${logs()}`);
    }
    try {
      const res = await fetch(base + '/health', { signal: AbortSignal.timeout(1000) });
      if (res.status === 200) return;
    } catch { /* not up yet */ }
    await sleep(200);
  }
  throw new Error(`等待 tt-server /health 超时：\n${logs()}`);
}

async function stopChild(child) {
  if (!child || child.exitCode !== null) return;
  child.kill('SIGTERM');
  for (let i = 0; i < 30; i++) {
    if (child.exitCode !== null) return;
    await sleep(100);
  }
  child.kill('SIGKILL');
}

async function runTests({ base, db, password, chrome }) {
  const sqlite = (sql) => {
    const r = spawnSync('sqlite3', [db, sql], { encoding: 'utf8' });
    if (r.status !== 0) throw new Error('sqlite3 失败：' + (r.stderr || ''));
    return r.stdout.trim();
  };

  const browser = await chromium.launch({ executablePath: chrome, headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    const pageErrors = [];
    page.on('pageerror', (e) => pageErrors.push(String(e)));
    page.on('console', (m) => { if (m.type() === 'error') pageErrors.push('console: ' + m.text()); });

    const waitInit = () => page.waitForFunction(
      () => document.querySelector('[data-time-picker]')?.dataset.timeInit === '1',
      null, { timeout: 10000 }
    );
    async function gotoDetail() {
      await page.goto(base + '/watches/' + WATCH, { waitUntil: 'domcontentloaded' });
      await waitInit();
    }
    async function blurToBody() {
      await page.locator('.detail-hero h1').click();
      await page.waitForFunction(() => document.activeElement === document.body);
    }
    async function setFormMarker() {
      await page.evaluate(() => {
        document.querySelector('form[data-picker-form]').dataset.testMark = 'b';
      });
    }
    async function formMarkerPresent() {
      return page.evaluate(
        () => document.querySelector('form[data-picker-form]')?.dataset.testMark === 'b'
      );
    }
    async function saveFilter() {
      await Promise.all([
        page.waitForNavigation({ waitUntil: 'domcontentloaded' }),
        page.getByRole('button', { name: '保存过滤条件' }).click(),
      ]);
      await waitInit();
    }

    // ---------- 登录 ----------
    await page.goto(base + '/login', { waitUntil: 'domcontentloaded' });
    await page.fill('#password', password);
    await Promise.all([
      page.waitForNavigation({ waitUntil: 'domcontentloaded' }),
      page.getByRole('button', { name: '登录' }).click(),
    ]);
    check('登录成功', page.url() === base + '/', page.url());

    // ---------- A. 干净页面 5s 轮询仍会刷新 ----------
    await gotoDetail();
    await page.evaluate(() => {
      document.querySelector('form[data-picker-form]').dataset.testMark = 'a';
    });
    await sleep(6500);
    const swappedWhileClean = await page.evaluate(
      () => document.querySelector('form[data-picker-form]').dataset.testMark !== 'a'
    );
    check('A. 干净页面 5s 轮询正常换块', swappedWhileClean);

    // ---------- B. 改 IMAX 后暂停轮询，改动不被冲掉 ----------
    const imax = page.locator('input[name="imax_only"]');
    check('B0. 初始 IMAX 未勾选', !(await imax.isChecked()));
    await imax.check();
    await blurToBody(); // 焦点离开 input：旧代码在这种状态下会被轮询覆盖
    await setFormMarker();
    await sleep(6500);
    check('B1. 勾选 IMAX 后 6.5s 仍是勾选状态', await imax.isChecked());
    check('B2. 有未保存改动时不发生换块', await formMarkerPresent());

    // ---------- C. 保存 IMAX 并落库 ----------
    await saveFilter();
    check('C1. 保存后详情页显示“只看 IMAX”',
      (await page.locator('.detail-meta').innerText()).includes('只看 IMAX'));
    check('C2. 保存后 checkbox 仍为勾选', await page.locator('input[name="imax_only"]').isChecked());
    check('C3. 数据库 imax_only=1',
      sqlite(`SELECT imax_only FROM watches WHERE id='${WATCH}'`) === '1');

    // ---------- D. 先经历一次 HTMX 换块，再验证日期弹窗重新绑定 ----------
    await gotoDetail();
    await page.evaluate(() => {
      document.querySelector('form[data-picker-form]').dataset.testMark = 'a';
    });
    await sleep(6500);
    const swapped = await page.evaluate(
      () => document.querySelector('form[data-picker-form]').dataset.testMark !== 'a'
    );
    check('D1. 换块已发生（后续操作针对新 DOM）', swapped);

    await page.locator('[data-time-mode="dates"]').click();
    await page.locator('[data-date-trigger]').first().click();
    await page.waitForFunction(() => document.getElementById('date-dialog')?.open === true);
    const titleBefore = await page.locator('[data-date-title]').innerText();
    await page.locator('[data-date-next]').click();
    const titleAfter = await page.locator('[data-date-title]').innerText();
    check('D2. 换块后日期弹窗“下一月”可用', titleBefore !== titleAfter,
      titleBefore + ' -> ' + titleAfter);

    const days = await page.locator('#date-dialog .cal-day[data-date]').evaluateAll(
      (els) => els.map((e) => e.dataset.date)
    );
    const d1 = days[2], d2 = days[5];
    await page.locator(`[data-date="${d1}"]`).click();
    await page.locator(`[data-date="${d2}"]`).click();
    await page.locator('[data-date-apply]').click();
    await page.waitForFunction(() => document.getElementById('date-dialog')?.open === false);
    const segLabel = await page.locator('[data-date-trigger]').first().innerText();
    check('D3. 换块后“应用”生效，日期段更新',
      segLabel.includes(d1) && segLabel.includes(d2), segLabel);
    const spec = JSON.parse(await page.locator('[data-time-value]').inputValue());
    check('D4. hidden time_spec 变为 intervals',
      spec.type === 'intervals'
        && spec.intervals[0].start_date === d1
        && spec.intervals[0].end_date === d2,
      JSON.stringify(spec));

    // ---------- E. 保存时间并验证刷新后回显 ----------
    await saveFilter();
    check('E1. 保存后数据库 time_spec 为 intervals',
      sqlite(`SELECT time_spec FROM watches WHERE id='${WATCH}'`).includes('intervals'));
    check('E2. 刷新后仍处于“指定日期”模式',
      (await page.locator('[data-time-mode="dates"]').getAttribute('class')).includes('active'));
    const segAfter = await page.locator('[data-date-trigger]').first().innerText();
    check('E3. 刷新后日期段回显正确', segAfter.includes(d1) && segAfter.includes(d2), segAfter);

    // ---------- F. 影院选择显示与保存 ----------
    const chips = page.locator('[data-cinema-chips] .chip');
    check('F1. 首屏即显示已选 2 家影院', (await chips.count()) === 2, String(await chips.count()));
    await page.locator('[data-cinema-open]').click();
    await page.waitForFunction(() => document.getElementById('cinema-dialog')?.open === true);
    await page.locator(`#cinema-dialog input[type=checkbox][value="${SEED_CINEMA_B}"]`).uncheck();
    await page.locator('[data-cinema-done]').click();
    await page.waitForFunction(() => document.getElementById('cinema-dialog')?.open === false);
    check('F2. 取消勾选后 chips 变为 1 家', (await chips.count()) === 1, String(await chips.count()));
    check('F3. 剩余影院为 UI测试影院A',
      (await chips.first().innerText()).includes('UI测试影院A'));

    await blurToBody();
    await setFormMarker();
    await sleep(6500);
    check('F4. 影院改动 6.5s 后仍保留', (await chips.count()) === 1 && await formMarkerPresent());

    await saveFilter();
    const remaining = sqlite(`SELECT group_concat(cinema_id) FROM watch_cinemas WHERE watch_id='${WATCH}'`);
    check('F5. 保存后数据库只剩 880001', remaining === SEED_CINEMA_A, remaining);
    check('F6. 刷新后仍显示 1 家影院',
      (await page.locator('[data-cinema-chips] .chip').count()) === 1);

    // ---------- G. 日期段内取消小时 / 移除日期段，不被轮询恢复 ----------
    const seg = page.locator('[data-time-segments] .time-segment').first();
    const cell19 = seg.locator('.hour-cell[data-hour="19"]');
    check('G0. 19 点初始为选中', (await cell19.getAttribute('class')).includes('on'));
    await cell19.click();
    check('G1. 点击后 19 点取消选中', !(await cell19.getAttribute('class')).includes('on'));
    await blurToBody();
    await setFormMarker();
    await sleep(6500);
    check('G2. 6.5s 后 19 点仍为未选中', !(await cell19.getAttribute('class')).includes('on'));
    check('G3. 有未保存改动时不发生换块', await formMarkerPresent());
    const specG = JSON.parse(await page.locator('[data-time-value]').inputValue());
    const hoursG = [];
    for (const it of specG.intervals) {
      for (let h = it.start_hour; h < it.end_hour; h++) hoursG.push(h);
    }
    check('G4. time_spec 已排除 19 点', !hoursG.includes(19), JSON.stringify(specG));

    await page.locator('[data-time-add]').click();
    const segs = page.locator('[data-time-segments] .time-segment');
    check('G5. 添加后有两个日期段', (await segs.count()) === 2, String(await segs.count()));
    await segs.nth(1).getByRole('button', { name: '移除' }).click();
    check('G6. 移除后只剩一个日期段', (await segs.count()) === 1, String(await segs.count()));
    await blurToBody();
    await setFormMarker();
    await sleep(6500);
    check('G7. 6.5s 后仍只有一个日期段', (await segs.count()) === 1, String(await segs.count()));
    check('G8. 有未保存改动时不发生换块', await formMarkerPresent());

    // ---------- H. 页面无 JS 错误 ----------
    check('H. 无 pageerror / console.error', pageErrors.length === 0, pageErrors.join(' | '));
  } finally {
    await browser.close().catch(() => {});
  }
}

function seed(db) {
  const sql = `
    INSERT OR REPLACE INTO movies(id, name, show_type)
      VALUES(${SEED_MOVIE}, 'UI测试电影', 1);
    INSERT OR REPLACE INTO cinemas(id, name, favorite)
      VALUES('${SEED_CINEMA_A}', 'UI测试影院A', 1),
            ('${SEED_CINEMA_B}', 'UI测试影院B', 0),
            ('880003', 'UI测试影院C', 0);
    INSERT OR REPLACE INTO watches(id, filter_key, movie_id, movie_name, time_spec,
                                   imax_only, enabled, scope, city_id, city_name)
      VALUES('${WATCH}', 'ui-test-key-1', ${SEED_MOVIE}, 'UI测试电影',
             '{"type":"all"}', 0, 1, 'cinema', 0, '');
    DELETE FROM watch_cinemas WHERE watch_id='${WATCH}';
    INSERT INTO watch_cinemas(watch_id, cinema_id)
      VALUES('${WATCH}', '${SEED_CINEMA_A}'),
            ('${WATCH}', '${SEED_CINEMA_B}');
  `;
  const r = spawnSync('sqlite3', [db, sql], { encoding: 'utf8' });
  if (r.status !== 0) throw new Error('播种测试数据失败：' + (r.stderr || ''));
}

async function main() {
  const serverBin = findServerBin();
  const chrome = findChrome();
  requireSqlite();

  const tmp = mkdtempSync(path.join(tmpdir(), 'tt-ui-test-'));
  const stateHome = path.join(tmp, 'state');
  mkdirSync(stateHome, { recursive: true });
  const db = path.join(stateHome, 'ticket-tracker.db');
  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  const password = crypto.randomBytes(12).toString('hex');

  // 确保用当前内置资产，不受开发者 shell 里的外部目录 / HTTPS cookie 设置影响。
  const env = { ...process.env };
  delete env.TT_WEB_DIR;
  delete env.TT_COOKIE_SECURE;
  Object.assign(env, {
    XDG_STATE_HOME: stateHome,
    TT_DB: db,
    TT_BIND: `127.0.0.1:${port}`,
    TT_NO_MONITOR: 'true',
    TT_ADMIN_PASSWORD: password,
  });

  let buf = '';
  const child = spawn(serverBin, [], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  const collect = (d) => {
    buf += d.toString();
    if (buf.length > 20000) buf = buf.slice(-20000);
  };
  child.stdout.on('data', collect);
  child.stderr.on('data', collect);

  let browserFailed = false;
  try {
    await waitForHealth(base, child, () => buf);
    seed(db);
    await runTests({ base, db, password, chrome });
  } catch (e) {
    browserFailed = true;
    console.error(e);
    if (buf) console.error('\n--- tt-server log ---\n' + buf);
  } finally {
    await stopChild(child);
    rmSync(tmp, { recursive: true, force: true });
  }

  const failed = results.filter((r) => !r.ok);
  console.log(`\n==== ${results.length - failed.length}/${results.length} passed ====`);
  if (browserFailed || failed.length) {
    if (failed.length) console.log('FAILED: ' + failed.map((f) => f.name).join(', '));
    process.exit(1);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(2);
});
