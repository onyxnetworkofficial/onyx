// e2e "U2 concurrent send keeps the active draft bound to its receipt".
// U2: actual node supplied by scripts/level3-book.sh local; no mocked successful receipts.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { createRequire } from 'node:module';
const axeSrc = await readFile(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');
const [api, reviewedId] = process.argv.slice(2);
assert.match(api ?? '', /^http:\/\/127\.0\.0\.1:\d+$/);
assert.match(reviewedId ?? '', /^sub_[0-9a-f]{32}$/);
const root = fileURLToPath(new URL('..', import.meta.url));
const server = spawn('pnpm', ['dev', '--host', '127.0.0.1', '--port', '0'], {
	cwd: root, env: { ...process.env, VITE_ONYX_SUBMIT_API: '1', ONYX_SUBMIT_PROXY: api },
	detached: true, stdio: ['ignore', 'pipe', 'pipe']
});
let browser;
try {
	const base = await new Promise((resolve, reject) => {
		const timeout = setTimeout(() => reject(new Error('connected preview did not start')), 30000);
		let log = '';
		server.stdout.on('data', (d) => {
			log = (log + d.toString()).slice(-8192);
			const url = log.match(/http:\/\/127\.0\.0\.1:\d+\//)?.[0];
			if (url) { clearTimeout(timeout); resolve(url); }
		});
		server.on('exit', (code) => { clearTimeout(timeout); reject(new Error(`preview exited ${code}`)); });
	});
	browser = await chromium.launch({ channel: 'chrome' });
	const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, reducedMotion: 'reduce', acceptDownloads: true });
	const page = await context.newPage();
	const errors = []; page.on('pageerror', (e) => errors.push(String(e)));
	const posts = []; page.on('request', (r) => { if (r.method() === 'POST') posts.push(r); });
	await page.goto(base + '#/submit');
	await page.getByTestId('receipt-id').fill(reviewedId);
	await page.getByTestId('receipt-refresh').click();
	await page.getByTestId('receipt-status').filter({ hasText: 'Approved for checking.' }).waitFor();
	assert.match(await page.getByTestId('receipt-status').innerText(), /verification and publication are separate/);
	console.log('ok U2 reviewed receipt from restarted real node; approval stays separate from verification');
	// Pre-public PP1: the rehearsal fed this receipt to the observers; the node's evidence is shown.
	const fed = page.getByTestId('receipt-check');
	await fed.waitFor();
	const fedText = await fed.innerText();
	assert.match(fedText, /Checked by observers \(level 2/);
	assert.match(fedText, /2026-10-08/);
	// PP11: the value the observers agreed on, grouped for reading, digit for digit the node's.
	assert.match(fedText, /Value the observers agreed on/);
	assert.equal(await page.getByTestId('receipt-value').innerText(), '40,305,316,210,829.72');
	assert.match(fedText, /0x[0-9a-f]{64}[\s\S]*0x[0-9a-f]{64}/);
	assert.match(fedText, /does not prove the source itself is correct/);
	assert.match(fedText, /not posted on a chain/);
	const fedStatus = await page.evaluate(async (receiptId) => (await fetch(`/v1/submit/${receiptId}`)).json(), reviewedId);
	assert.ok(fedText.includes(fedStatus.check.roundId) && fedText.includes(fedStatus.check.result), 'the page shows the node evidence');
	assert.deepEqual(Object.keys(fedStatus).sort(), ['check', 'id', 'status']);
	for (const width of [390, 1280]) {
		await page.setViewportSize({ width, height: 900 });
		assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, `fed receipt fits at ${width}px`);
		await page.addScriptTag({ content: axeSrc });
		const fedA11y = await page.evaluate(async () => (await window.axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa', 'best-practice'] } })).violations.filter((v) => ['serious', 'critical'].includes(v.impact)).map((v) => v.id));
		assert.deepEqual(fedA11y, []);
		await page.screenshot({ path: `/tmp/onyx-w1-fed-receipt-${width}.png`, fullPage: true });
	}
	console.log('ok PP1 fed receipt shows the observers\' round, result and limits from the real node; fits 390 px; axe clean');
	await page.getByTestId('receipt-id').fill('sub_' + '0'.repeat(32));
	await page.getByTestId('receipt-refresh').click();
	await page.getByTestId('receipt-error').filter({ hasText: 'Receipt not found' }).waitFor();
	await page.getByTestId('try-example').click();
	for (let i = 0; i < 3; i++) await page.getByTestId('next').click();
	let releasePost;
	let sawPost;
	let markContinued;
	let delayedPost = false;
	const postReached = new Promise((resolve) => { sawPost = resolve; });
	const firstPostContinued = new Promise((resolve) => { markContinued = resolve; });
	await page.route('**/v1/submit', async (route) => {
		if (route.request().method() !== 'POST') return route.continue();
		if (delayedPost) return route.continue();
		delayedPost = true;
		sawPost();
		await new Promise((resolve) => { releasePost = resolve; });
		await route.continue();
		markContinued();
	});
	await page.getByTestId('send').evaluate((button) => { button.click(); button.click(); });
	await postReached;
	assert.equal(await page.getByTestId('try-keyed').isDisabled(), true, 'draft replacement is disabled while send is pending');
	assert.equal(await page.getByTestId('back').isDisabled(), true, 'navigation is disabled while send is pending');
	assert.match(await page.getByTestId('send-json').textContent(), /U\.S\. total public debt outstanding/);
	releasePost();
	await firstPostContinued;
	await page.unroute('**/v1/submit');
	await page.getByTestId('sent').waitFor();
	assert.equal(posts.length, 1, 'double activation sends one proposal');
	const sentText = await page.getByTestId('sent').innerText();
	const id = sentText.match(/sub_[0-9a-f]{32}/)?.[0]; assert.ok(id);
	await page.getByTestId('receipt-status').filter({ hasText: id }).waitFor();
	const [download] = await Promise.all([page.waitForEvent('download'), page.getByTestId('receipt-download').click()]);
	const receipt = JSON.parse(await readFile(await download.path(), 'utf8'));
	assert.deepEqual(receipt, { format: 'onyx-submission-receipt-v1', id, status: 'pending_review' });
	assert.equal(download.suggestedFilename(), 'onyx-receipt.json');
	console.log('ok U2 real submit, single POST, downloadable receipt contains no proposal data');
	await page.reload();
	await page.getByTestId('receipt-id').fill(id);
	await page.getByTestId('receipt-refresh').click();
	await page.getByTestId('receipt-status').filter({ hasText: 'Waiting for review' }).waitFor();
	const stored = await page.evaluate(() => JSON.stringify([Object.entries(localStorage), Object.entries(sessionStorage)]));
	assert.ok(!stored.includes(id) && !stored.includes(reviewedId) && !stored.includes('U.S. total public debt outstanding'), 'receipts and proposal content are not persisted in browser storage');
	// Failure-only transport injection: successful statuses always came from the actual node.
	await page.route(`**/v1/submit/${id}`, (route) => route.fulfill({ status: 503, body: '{}' }));
	await page.getByTestId('receipt-refresh').click();
	await page.getByTestId('receipt-error').filter({ hasText: 'unavailable right now' }).waitFor();
	await page.unroute(`**/v1/submit/${id}`);
	console.log('ok U2 return visit, unknown receipt and unavailable-server states; no intake data in browser storage');
	const ownPage = await context.newPage();
	await ownPage.goto(base + '#/submit');
	for (const choice of ['q-what-own', 'q-nologin-no', 'q-rights-own_data', 'q-person-no', 'q-changes-once']) await ownPage.getByTestId(choice).click();
	await ownPage.getByTestId('next').click();
	await ownPage.getByTestId('f-title').fill('Small local dataset');
	await ownPage.getByTestId('f-description').fill('A local sample I own and have permission to share.');
	await ownPage.getByTestId('f-source-name').fill('My records');
	const sample = 'date,value\n2026-01-01,12\n';
	await ownPage.getByTestId('payload-paste').fill(sample);
	await ownPage.getByTestId('payload-preview').click();
	await ownPage.getByTestId('payload-ready').waitFor();
	await ownPage.getByTestId('next').click();
	await ownPage.getByTestId('next').click();
	const proposalResponse = ownPage.waitForResponse((r) => r.url().endsWith('/v1/submit') && r.request().method() === 'POST');
	await ownPage.getByTestId('send').click();
	const storedResponse = await proposalResponse;
	assert.equal(storedResponse.status(), 201, 'actual node accepted URL-free own data');
	const sentPayload = JSON.parse(storedResponse.request().postData());
	assert.equal(sentPayload.dataPayload.content, sample);
	assert.equal(sentPayload.sourceUrl, '');
	const ownIdText = await ownPage.getByTestId('sent').innerText();
	const ownId = ownIdText.match(/sub_[0-9a-f]{32}/)?.[0]; assert.ok(ownId);
	const publicStatus = await ownPage.evaluate(async (receiptId) => (await fetch(`/v1/submit/${receiptId}`)).json(), ownId);
	assert.deepEqual(publicStatus, { id: ownId, status: 'pending_review' }, 'actual node keeps sample out of public status');
	assert.equal(await ownPage.evaluate(() => JSON.stringify([Object.entries(localStorage), Object.entries(sessionStorage)])), '[[],[]]');
	console.log('ok U3 real node accepts own CSV without URL, retains it for review, and returns receipt-only status');
	await ownPage.close();
	for (const width of [1280, 390]) {
		await page.setViewportSize({ width, height: 900 });
		await page.getByTestId('receipt-section').scrollIntoViewIfNeeded();
		assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false);
		await page.addScriptTag({ content: axeSrc });
		const a11y = await page.evaluate(async () => (await window.axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa', 'best-practice'] } })).violations.filter((v) => ['serious', 'critical'].includes(v.impact)).map((v) => v.id));
		assert.deepEqual(a11y, []);
		await page.screenshot({ path: `/tmp/onyx-u2-receipt-${width}.png`, fullPage: true });
	}
	assert.deepEqual(errors, []);
	console.log('SUBMISSION BROWSER PASSED');
} finally {
	await browser?.close();
	try { process.kill(-server.pid, 'SIGTERM'); } catch {}
}
