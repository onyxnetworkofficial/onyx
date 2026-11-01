// CL10: the Download page's pure parts (src/lib/release.ts): which system a browser is on, which
// releases carry the desktop installers, and where every file is. Needs Node 22.18 or later (it
// imports the TypeScript file with its types stripped). Run: node --test apps/web/tests/release.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';

const R = await import(new URL('../src/lib/release.ts', import.meta.url).href);

// Real browsers' user agents and navigator.platform / userAgentData.platform values.
const UA = {
	edge11: ['Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0', 'Win32', 'Windows'],
	firefoxWin: ['Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:132.0) Gecko/20100101 Firefox/132.0', 'Win32', ''],
	safariMac: ['Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15', 'MacIntel', ''],
	chromeMac: ['Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36', 'MacIntel', 'macOS'],
	chromeLinux: ['Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36', 'Linux x86_64', 'Linux'],
	firefoxUbuntu: ['Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:132.0) Gecko/20100101 Firefox/132.0', 'Linux x86_64', ''],
	iphone: ['Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1', 'iPhone', ''],
	android: ['Mozilla/5.0 (Linux; Android 15; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36', 'Linux armv81', 'Android'],
	// iPadOS asks for the desktop site by default: a Mac user agent, told apart by its touch screen.
	ipad: ['Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15', 'MacIntel', '', 5],
	chromebook: ['Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36', 'Linux x86_64', 'Chrome OS']
};

test('detectSystem: Windows, macOS and Linux browsers; phones, tablets and Chromebooks are other', () => {
	const want = { edge11: 'windows', firefoxWin: 'windows', safariMac: 'macos', chromeMac: 'macos', chromeLinux: 'linux', firefoxUbuntu: 'linux', iphone: 'other', android: 'other', ipad: 'other', chromebook: 'other' };
	for (const [k, [ua, pf, uad, touch]] of Object.entries(UA)) assert.equal(R.detectSystem(ua, pf, uad, touch ?? 0), want[k], k);
	assert.equal(R.detectSystem('', '', ''), 'other');
	assert.equal(R.detectSystem(), 'other');
});

test('hasDesktop: DESKTOP_SINCE and later releases carry the installers; earlier ones and other tags do not', () => {
	assert.equal(R.hasDesktop(R.DESKTOP_SINCE), true);
	assert.equal(R.hasDesktop('v0.2.0-testnet'), false);
	assert.equal(R.hasDesktop('v0.2.9-testnet'), false);
	assert.equal(R.hasDesktop('v0.3.0-testnet'), false, 'its desktop run failed: no installers');
	assert.equal(R.hasDesktop('v0.3.1-testnet'), true);
	assert.equal(R.hasDesktop('v0.3.2-testnet'), true);
	assert.equal(R.hasDesktop('v0.10.0-testnet'), true);
	assert.equal(R.hasDesktop('v1.0.0-testnet'), true);
	for (const bad of ['v0.3.0', '0.3.0-testnet', 'v0.3.0-mainnet', '', 'latest']) assert.equal(R.hasDesktop(bad), false, bad);
	assert.equal(R.hasDesktop(), R.hasDesktop(R.RELEASE));
});

test('files: every link is a file of the release on the project repository; stable names', () => {
	const base = `https://github.com/onyxnetworkofficial/onyx/releases/download/${R.RELEASE}/`;
	assert.equal(R.BASE, base);
	assert.equal(R.PAGE, `https://github.com/onyxnetworkofficial/onyx/releases/tag/${R.RELEASE}`);
	assert.equal(R.baseFor('v0.3.0-testnet'), 'https://github.com/onyxnetworkofficial/onyx/releases/download/v0.3.0-testnet/');
	assert.match(R.RELEASE, /^v\d+\.\d+\.\d+-testnet$/);
	assert.match(R.DESKTOP_SINCE, /^v\d+\.\d+\.\d+-testnet$/);
	const primary = Object.fromEntries(R.DESKTOP.map((d) => [d.id, d.files[0].file]));
	assert.deepEqual(primary, { windows: 'onyx-desktop-windows-x64-setup.exe', macos: 'onyx-desktop-macos-universal.dmg', linux: 'onyx-desktop-linux-x86_64.AppImage' });
	const all = R.DESKTOP.flatMap((d) => d.files.map((f) => f.file));
	assert.deepEqual(all, ['onyx-desktop-windows-x64-setup.exe', 'onyx-desktop-windows-x64.msi', 'onyx-desktop-macos-universal.dmg', 'onyx-desktop-linux-x86_64.AppImage', 'onyx-desktop-linux-amd64.deb']);
	assert.deepEqual(R.CLI_FILES, ['onyx-contrib-linux-x64', 'onyx-contrib-macos-arm64', 'onyx-contrib-macos-x64', 'onyx-contrib-windows-x64.exe']);
	assert.equal(R.SUMS, 'SHA256SUMS');
	// No version in any desktop or single-file name: the page needs no edit per release but RELEASE.
	for (const f of [...all, ...R.CLI_FILES]) assert.ok(!/v\d+\.\d+/.test(f), f);
	const cli = R.programs('v9.9.9-testnet').flatMap((p) => p.files);
	assert.deepEqual(cli, ['onyx-contrib-v9.9.9-testnet.zip', 'onyx-contrib-v9.9.9-testnet.tar.gz', 'onyx-join-linux-x86_64.tar.gz', 'onyx-publish-linux-x86_64.tar.gz']);
});
