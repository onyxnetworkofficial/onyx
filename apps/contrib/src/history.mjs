// Past contributions on this computer (CL10): the receipt id Onyx gave, the provider's name, the
// title, the public endpoint (no key: it is the body that passed the key guard) and the time.
// A plain JSON file in the config folder (0600), newest first, at most MAX_HISTORY entries.
import { chmodSync, existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

export const MAX_HISTORY = 200;
const ID_RE = /^sub_[0-9a-f]{32}$/;

/** The history file (<config>/contributions.json). */
export function history(dir) {
	const path = join(dir, 'contributions.json');
	const load = () => {
		if (!existsSync(path)) return [];
		try {
			const v = JSON.parse(readFileSync(path, 'utf8'));
			return Array.isArray(v?.contributions) ? v.contributions.filter((c) => c && ID_RE.test(c.id ?? '')) : [];
		} catch {
			return [];
		}
	};
	return {
		path,
		list: load,
		/** Record one sent contribution (fields cut to what the intake took anyway). */
		add({ id, provider, title, endpoint, pointer, status }) {
			if (!ID_RE.test(id ?? '')) return;
			const cut = (s, n) => (typeof s === 'string' ? s.slice(0, n) : null);
			const all = [{ id, provider: cut(provider, 40), title: cut(title, 120), endpoint: cut(endpoint, 600), pointer: cut(pointer, 256), status: cut(status, 64), sentAt: new Date().toISOString().slice(0, 19) + 'Z' }, ...load().filter((c) => c.id !== id)].slice(0, MAX_HISTORY);
			mkdirSync(dir, { recursive: true, mode: 0o700 });
			const tmp = `${path}.${process.pid}.tmp`;
			writeFileSync(tmp, JSON.stringify({ version: 1, contributions: all }, null, 1) + '\n', { mode: 0o600 });
			renameSync(tmp, path);
			chmodSync(path, 0o600);
		},
	};
}
