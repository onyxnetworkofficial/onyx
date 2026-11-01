// The test-network release the site links (scripts/release-build.sh for the command-line files,
// .github/workflows/desktop.yml for the desktop app). RELEASE is the one place to change for a new
// release: every file keeps its name from release to release, so the links follow the tag.
// Pure (no Svelte, no DOM), so `node --test tests/release.test.mjs` imports it as is.

/** The release the Add data and Download pages link. */
export const RELEASE = 'v0.3.1-testnet';
/**
 * The first release that carries the desktop installers (fixed: never changes). v0.3.0-testnet ran
 * the workflow first, but its macOS and Windows jobs failed, so it has the command-line files only.
 */
export const DESKTOP_SINCE = 'v0.3.1-testnet';
export const REPO = 'https://github.com/onyxnetworkofficial/onyx';

/** Where `release`'s files are. */
export const baseFor = (release: string) => `${REPO}/releases/download/${release}/`;
/** The release's page on GitHub (every file and its notes). */
export const pageFor = (release: string) => `${REPO}/releases/tag/${release}`;
export const BASE = baseFor(RELEASE);
export const PAGE = pageFor(RELEASE);
export const SUMS = 'SHA256SUMS';

/** [major, minor, patch] of a test-network tag (vX.Y.Z-testnet), or null. */
export function versionOf(tag: string): [number, number, number] | null {
	const m = /^v(\d+)\.(\d+)\.(\d+)-testnet$/.exec(tag);
	return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null;
}

/** Whether `release` carries the desktop installers (it is DESKTOP_SINCE or later). */
export function hasDesktop(release: string = RELEASE): boolean {
	const a = versionOf(release);
	const b = versionOf(DESKTOP_SINCE)!;
	if (!a) return false;
	for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] > b[i];
	return true;
}

export type System = 'windows' | 'macos' | 'linux' | 'other';

/**
 * The visitor's computer from what the browser says: `userAgent`, `navigator.platform` and
 * `navigator.userAgentData.platform` (Chromium only). Phones and tablets are 'other': the app is
 * for a computer (an iPad asking for the desktop site says "Macintosh", but has a touch screen,
 * so `touch` > 1 on a "Mac" is an iPad).
 */
export function detectSystem(userAgent = '', platform = '', uaDataPlatform = '', touch = 0): System {
	const ua = userAgent.toLowerCase();
	const pf = `${uaDataPlatform} ${platform}`.toLowerCase();
	if (/android|iphone|ipad|ipod|mobile|cros|crkey/.test(ua) || /android|ios|iphone|ipad/.test(pf)) return 'other';
	if (/windows|win32|win64/.test(pf) || /windows nt/.test(ua)) return 'windows';
	if (/mac/.test(pf) || /macintosh|mac os x/.test(ua)) return touch > 1 ? 'other' : 'macos';
	if (/linux|x11|ubuntu|fedora/.test(pf) || /linux|x11/.test(ua)) return 'linux';
	return 'other';
}

export type Installer = { file: string; kind: string };
export type DesktopSystem = { id: Exclude<System, 'other'>; name: string; needs: string; files: Installer[] };

/** The desktop app's installers (the workflow's stable names); the first file of each is the button's. */
export const DESKTOP: DesktopSystem[] = [
	{
		id: 'windows',
		name: 'Windows',
		needs: 'Windows 10 and 11, 64-bit',
		files: [
			{ file: 'onyx-desktop-windows-x64-setup.exe', kind: 'Installer (.exe)' },
			{ file: 'onyx-desktop-windows-x64.msi', kind: 'Installer package (.msi)' }
		]
	},
	{
		id: 'macos',
		name: 'macOS',
		needs: 'macOS 11 or later, Apple silicon and Intel',
		files: [{ file: 'onyx-desktop-macos-universal.dmg', kind: 'Disk image (.dmg)' }]
	},
	{
		id: 'linux',
		name: 'Linux',
		needs: 'Linux x86-64 (Ubuntu 22.04, Debian 12 or newer)',
		files: [
			{ file: 'onyx-desktop-linux-x86_64.AppImage', kind: 'AppImage, any distribution' },
			{ file: 'onyx-desktop-linux-amd64.deb', kind: 'Package for Debian and Ubuntu (.deb)' }
		]
	}
];

/** The single-file command-line program (no Node.js needed), per system. */
export const CLI_FILES = ['onyx-contrib-linux-x64', 'onyx-contrib-macos-arm64', 'onyx-contrib-macos-x64', 'onyx-contrib-windows-x64.exe'];

export type Program = { id: string; title: string; what: string; needs: string; files: string[] };

/** The command-line programs of `release` (scripts/release-build.sh). */
export function programs(release: string = RELEASE): Program[] {
	return [
		{
			id: 'contrib',
			title: 'Contributor app',
			what: 'Keep your API keys in your computer’s keychain and add data from those APIs through Claude Desktop, Claude Code or another MCP client. The keys never leave your computer.',
			needs: 'Node.js 20 or later; Linux, macOS or Windows',
			files: [`onyx-contrib-${release}.zip`, `onyx-contrib-${release}.tar.gz`]
		},
		{
			id: 'join',
			title: 'Observer kit',
			what: 'Take part in level-2 rounds: your computer fetches the public source itself and posts its own signed answer on the test chain.',
			needs: 'Linux x86-64',
			files: ['onyx-join-linux-x86_64.tar.gz']
		},
		{
			id: 'publish',
			title: 'Publisher tools',
			what: 'Publish a large dataset of your own: sign it, anchor its fingerprint on the test chain and upload it. Anyone can then prove each row they read.',
			needs: 'Linux x86-64',
			files: ['onyx-publish-linux-x86_64.tar.gz']
		}
	];
}
