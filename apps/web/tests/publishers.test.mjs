// ROADMAP S27 (ADR-0066 slice 4): the publish page's pure parts. publishers.ts reads the status
// list in its exact shape or not at all, prints a state as the list has it, and looks an
// application up by its hash or a publisher by its id; publish-steps.ts holds the ten steps, each
// command word for word from the guide docs/ops/bulk-publish.md.
// Run: node --test apps/web/tests/publishers.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { APPLICATION_STATES, BEFORE_RECORD, DECIDE_DAYS, MIN_PREFIX, PUBLISHER_STATES, REASONS, lookup, nextStep, parseStatus, parseStatusText, utcMinute } from '../src/lib/publishers.ts';
import { BOOK, CHAIN_ID, EXAMPLE_ID, EXAMPLE_ORIGIN, HOSTED_NODE, ISSUE_LINES, ISSUE_TITLE, ISSUES, SCHEMA_EXAMPLE, STATUS_URL, TOOLS_FILE, TOOLS_URL, UNPACK, WORDS, buildFromSource, steps, subcommands } from '../src/lib/publish-steps.ts';
import { BASE, ONBOARDING_SINCE, RELEASE, REPO, hasOnboarding } from '../src/lib/release.ts';

const repo = (p) => new URL(`../../../${p}`, import.meta.url);
const committed = await readFile(repo('apps/web/static/publishers.json'), 'utf8');

// Made-up publishers and applications (no real id, origin or key).
const A = { refused: '0x' + 'ab'.repeat(32), expired: '0x' + 'abababab' + 'cd'.repeat(28), received: '0x' + '12'.repeat(32), withdrawn: '0x' + '9f'.repeat(32) };
const good = () => ({
	version: 1,
	chain_id: 84532,
	book: '0x' + '04'.repeat(20),
	publishers: [
		{ publisher: 'river-levels.open', state: 'approved (record)', date: '2026-11-17', origin: 'data.example.org/river-levels', signers: ['0x' + '11'.repeat(20)], registered: [{ tx: '0x' + '22'.repeat(32), block: 60000001 }] },
		{ publisher: 'tide-tables.open', state: 'registered-before-record', date: '2026-10-30', signers: ['0x' + '33'.repeat(20)], registered: [{ tx: '0x' + '44'.repeat(32), block: 52000000 }] },
		{ publisher: 'deadbeef', state: 'paused', date: '2026-11-18', origin: 'beef.example.org', signers: [], registered: [], reason: 'no-capacity' }
	],
	applications: [
		{ application: A.refused, state: 'refused', date: '2026-11-17', reason: 'rights-unclear', blocks_signer_until: 1795478700 },
		{ application: A.expired, state: 'expired', date: '2026-11-16' },
		{ application: A.received, state: 'received', date: '2026-11-17' },
		{ application: A.withdrawn, state: 'withdrawn', date: '2026-11-15' }
	]
});

test('the committed status list is read, with its states word for word', () => {
	const list = parseStatusText(committed);
	assert.ok(list, 'apps/web/static/publishers.json is in the shape the page reads');
	const raw = JSON.parse(committed);
	assert.deepEqual(list.publishers.map((p) => [p.publisher, p.state, p.date, p.origin]), raw.publishers.map((p) => [p.publisher, p.state, p.date, p.origin ?? null]));
	// Review S4-R1 M2: a publisher from before the record is listed without an origin.
	assert.ok(raw.publishers.length > 0 && raw.publishers.every((p) => p.state !== BEFORE_RECORD || !('origin' in p)));
	assert.ok(!committed.includes('"origin"'), 'the committed list holds no origin today: all three publishers are from before the record');
	assert.equal(list.applications.length, raw.applications.length);
	assert.equal(list.book, raw.book);
	assert.equal(list.chainId, raw.chain_id);
});

test('every state and reason the checker and the build guard know has its plain words, and no other', async () => {
	const guard = await readFile(repo('adapters/source-btc/tests/public_build.rs'), 'utf8');
	const listOf = (name) => [...new RegExp(`const ${name}: &\\[&str\\] = &\\[([^;]*)\\];`).exec(guard)[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
	assert.deepEqual(Object.keys(PUBLISHER_STATES).sort(), listOf('PUBLISHER_STATES').sort());
	assert.deepEqual(Object.keys(APPLICATION_STATES).sort(), listOf('APPLICATION_STATES').sort());
	assert.deepEqual(Object.keys(REASONS).sort(), listOf('REASON_CODES').sort());
	const checker = await readFile(repo('scripts/check-publishers.py'), 'utf8');
	const py = (name) => [...new RegExp(`^${name} = \\[([^\\]]*)\\]`, 'm').exec(checker)[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
	assert.deepEqual(Object.keys(REASONS).sort(), [...py('REASONS'), ...py('TOOL_REASONS')].sort());
	assert.match(checker, /^UNCONFIRMED = "approved \(record\)"$/m);
	assert.match(checker, new RegExp(`^DECIDE_DAYS = ${DECIDE_DAYS}$`, 'm'));
	// A sentence never says more than its state: an approval only the record shows is not "on chain".
	assert.match(PUBLISHER_STATES['approved (record)'], /has not run/);
	assert.equal(BEFORE_RECORD, 'registered-before-record');
	assert.equal(PUBLISHER_STATES[BEFORE_RECORD], 'Registered before this record began.');
	for (const t of [...Object.values(PUBLISHER_STATES), ...Object.values(APPLICATION_STATES), ...Object.values(REASONS)]) assert.doesNotMatch(t, /ADR|THREATS|PUBON|SUB-\d/);
});

test('a list in shape is read; snake_case fields become the page\'s own', () => {
	const list = parseStatus(good());
	assert.equal(list.publishers.length, 3);
	assert.equal(list.publishers[2].reason, 'no-capacity');
	assert.equal(list.publishers[0].reason, null);
	assert.deepEqual([list.publishers[0].origin, list.publishers[1].origin], ['data.example.org/river-levels', null]);
	assert.deepEqual(list.applications[0], { application: A.refused, state: 'refused', date: '2026-11-17', reason: 'rights-unclear', blocksSignerUntil: 1795478700 });
	assert.deepEqual(list.applications[1], { application: A.expired, state: 'expired', date: '2026-11-16', reason: null, blocksSignerUntil: null });
	// The network fields are optional (a record without a registered key has none).
	const bare = good();
	delete bare.chain_id;
	delete bare.book;
	assert.deepEqual([parseStatus(bare).chainId, parseStatus(bare).book], [null, null]);
	assert.deepEqual(parseStatus({ version: 1, publishers: [], applications: [] }), { chainId: null, book: null, publishers: [], applications: [] });
});

test('a missing, broken or out-of-shape file is refused whole', () => {
	for (const text of ['', 'not json', '<html>404</html>', 'null', '[]', '"x"', '{', '{"version":1}']) assert.equal(parseStatusText(text), null, text);
	const cases = {
		'another version': (v) => (v.version = 2),
		'publishers not a list': (v) => (v.publishers = {}),
		'applications missing': (v) => delete v.applications,
		'an unknown top field': (v) => (v.note = 'x'),
		'an unknown publisher field': (v) => (v.publishers[0].contact = 'x'),
		'an unknown application field': (v) => (v.applications[0].publisher = 'river-levels.open'),
		'an unknown field of a registration': (v) => (v.publishers[0].registered[0].by = 'x'),
		'markup in an id': (v) => (v.publishers[0].publisher = '<img src=x onerror=alert(1)>'),
		'markup in an origin': (v) => (v.publishers[0].origin = 'data.example.org/<script>'),
		'an origin with a scheme': (v) => (v.publishers[0].origin = 'https://data.example.org'),
		'an origin that is a script link': (v) => (v.publishers[0].origin = 'javascript:alert(1)'),
		'an id with capitals and a space': (v) => (v.publishers[0].publisher = 'Some Person'),
		'a state the list does not have': (v) => (v.publishers[0].state = 'approved, run by the network'),
		'a state named like an object key': (v) => (v.publishers[0].state = 'constructor'),
		'an application state on a publisher': (v) => (v.publishers[0].state = 'refused'),
		'a publisher state on an application': (v) => (v.applications[0].state = 'approved'),
		'a reason that is free text': (v) => (v.applications[0].reason = 'the reviewer did not like it'),
		'a reason named like an object key': (v) => (v.applications[0].reason = 'toString'),
		'a short key address': (v) => (v.publishers[0].signers[0] = '0x1234'),
		'a key address in capitals': (v) => (v.publishers[0].signers[0] = '0x' + 'AB'.repeat(20)),
		'a date with text': (v) => (v.publishers[0].date = '2026-11-17 by hand'),
		'a short application hash': (v) => (v.applications[0].application = '0xabab'),
		'a block that is text': (v) => (v.publishers[0].registered[0].block = '60000001'),
		'a blocking time that is text': (v) => (v.applications[0].blocks_signer_until = 'soon'),
		'a negative blocking time': (v) => (v.applications[0].blocks_signer_until = -1),
		'a book that is not an address': (v) => (v.book = 'the book'),
		'a publisher that is a string': (v) => (v.publishers[1] = 'tide-tables.open'),
		'a very long id': (v) => (v.publishers[0].publisher = 'a'.repeat(129)),
		// Review S4-R1 M2: an origin only where the list may carry one.
		'an origin on a publisher from before the record': (v) => (v.publishers[1].origin = 'tides.example.net'),
		'an approved publisher without an origin': (v) => delete v.publishers[0].origin,
		'a paused publisher without an origin': (v) => delete v.publishers[2].origin,
		// Review S4-R1 L1: what must stand once stands once.
		'a publisher id twice': (v) => (v.publishers[1].publisher = 'river-levels.open'),
		'an origin twice': (v) => (v.publishers[2].origin = 'data.example.org/river-levels'),
		'a key address under two publishers': (v) => (v.publishers[1].signers[0] = v.publishers[0].signers[0]),
		'a key address twice under one publisher': (v) => v.publishers[0].signers.push(v.publishers[0].signers[0]),
		'a transaction twice': (v) => (v.publishers[1].registered[0].tx = v.publishers[0].registered[0].tx),
		'a registration twice under one publisher': (v) => v.publishers[0].registered.push({ ...v.publishers[0].registered[0] }),
		'an application hash twice': (v) => (v.applications[1].application = v.applications[0].application)
	};
	for (const [what, change] of Object.entries(cases)) {
		const v = good();
		change(v);
		assert.equal(parseStatus(v), null, what);
	}
});

test('a lookup finds an application by its hash prefix and a publisher by its id', () => {
	const list = parseStatus(good());
	const found = (q) => {
		const r = lookup(list, q);
		return r.kind === 'found' ? [r.publishers.map((p) => p.publisher), r.applications.map((a) => a.state)] : r.kind;
	};
	assert.equal(found(''), 'empty');
	assert.equal(found('   '), 'empty');
	// A publisher: its exact id only (never a prefix, so a look-alike is not shown as the same).
	assert.deepEqual(found('river-levels.open'), [['river-levels.open'], []]);
	assert.deepEqual(found('  River-Levels.Open '), [['river-levels.open'], []]);
	assert.equal(found('river-levels'), 'none');
	assert.equal(found('river-levels.open.x'), 'none');
	// An application: the first digits of its hash, with or without 0x, in either case.
	assert.deepEqual(found(A.received), [[], ['received']]);
	assert.deepEqual(found('12121212'), [[], ['received']]);
	assert.deepEqual(found('0x9F9F9F9F9f'), [[], ['withdrawn']]);
	// A prefix two applications share shows both; a longer one tells them apart.
	assert.deepEqual(found('abababab'), [[], ['refused', 'expired']]);
	assert.deepEqual(found('ababababab'), [[], ['refused']]);
	assert.deepEqual(found('0xababababcd'), [[], ['expired']]);
	assert.equal(MIN_PREFIX, 8);
	// Fewer digits than that are never matched against a hash (they read as an id nobody has).
	assert.equal(found('0xabab'), 'none');
	assert.equal(found('abab'), 'none');
	assert.equal(found('0x' + 'ab'.repeat(33)), 'none', 'longer than a hash');
	assert.equal(found('ffffffff'), 'none');
	// Eight hex digits are an id too: both are looked up.
	assert.deepEqual(found('deadbeef'), [['deadbeef'], []]);
	// Anything else is refused, never searched or shown back.
	for (const q of ['<script>', 'river levels', 'a@b', 'ÄÖ', '../x', 'a..b', 'x'.repeat(200)]) assert.equal(found(q), 'invalid', q);
	// The id of a refused application is not in the list, so it cannot be found by an id.
	assert.equal(lookup(parseStatus({ version: 1, publishers: [], applications: good().applications }), 'river-levels.open').kind, 'none');
});

test('each state that is not an approval says how to go on', () => {
	const list = parseStatus(good());
	const [refused, expired, received, withdrawn] = list.applications;
	assert.equal(utcMinute(1795478700), '2026-11-24 00:05 UTC');
	assert.match(nextStep(refused), /apply with a new key/);
	assert.match(nextStep(refused), /blocked until 2026-11-24 00:05 UTC/);
	// A refusal that blocks no key (its signature did not hold) names no key and no time.
	const open = nextStep({ ...refused, blocksSignerUntil: null });
	assert.match(open, /fix the reason named above/);
	assert.doesNotMatch(open, /blocked|new key/);
	assert.match(nextStep(expired), /--reapply/);
	assert.match(nextStep(expired), /the same key and the same sample folder/);
	assert.match(nextStep(received), new RegExp(`within ${DECIDE_DAYS} days`));
	assert.match(nextStep(withdrawn), /closed/);
	assert.equal(utcMinute(Number.MAX_SAFE_INTEGER), String(Number.MAX_SAFE_INTEGER));
});

const guide = await readFile(repo('docs/ops/bulk-publish.md'), 'utf8');
// The guide writes the book, the node and the release tag as placeholders; the page names them.
const inGuide = (line) => [line, line.replaceAll(BOOK, '<signed book address>').replaceAll(HOSTED_NODE, '<node address>'), line.replaceAll(RELEASE, '<release tag>')].some((l) => guide.includes(l.trim()));

test('the ten steps are those of the guide, by number, title and command', async () => {
	const S = steps();
	// Review S4-R1 L3: the same ten steps under the same numbers and titles as the guide's table.
	const table = [...guide.matchAll(/^\| (\d+) \| ([^|]+) \| [^|]+ \| [^|]+ \|$/gm)].map((m) => [Number(m[1]), m[2].replaceAll('`', '').trim()]);
	assert.deepEqual(table.map((r) => r[0]), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
	assert.deepEqual(S.map((s, i) => [i + 1, s.title]), table);
	const lines = S.flatMap((s, i) => (i === 6 ? [] : s.commands.flatMap((c) => c.split('\n'))));
	assert.equal(lines.length, 2 + 1 + SCHEMA_EXAMPLE.split('\n').length + 2 + 1 + 1 + 2 + 1 + 2);
	for (const line of lines) assert.ok(inGuide(line), `in the guide: ${line}`);
	for (const word of ['keygen', 'keys-doc', 'build', 'sign', 'apply', 'doctor --live', 'doctor --publishers', 'upload', 'onyx-check --bulk']) assert.ok(lines.some((l) => l.includes(word)), word);
	assert.ok(S[4].commands[0].includes(`--chain-id ${CHAIN_ID} --book ${BOOK}`));
	assert.ok(guide.includes(BOOK) && guide.includes(HOSTED_NODE) && guide.includes(`chain id ${CHAIN_ID}`));
	// The id and the origin are picked before the steps, as in the guide, with made-up examples.
	assert.ok(guide.includes(EXAMPLE_ID) && guide.includes(EXAMPLE_ORIGIN));
	assert.ok(guide.indexOf('Pick a neutral publisher id') < guide.indexOf('## Step by step'));
	// Step 7 is the issue's title and two lines.
	const template = await readFile(repo('.github/ISSUE_TEMPLATE/publisher-application.md'), 'utf8');
	assert.deepEqual(S[6].commands, [ISSUE_LINES.join('\n')]);
	assert.equal(ISSUE_LINES.length, 2);
	for (const l of ISSUE_LINES) assert.ok(template.includes(l), l);
	assert.ok(template.includes(`title: "${ISSUE_TITLE}"`) && S[6].what.includes(ISSUE_TITLE));
	assert.match(S[6].what, /Today an application is filed as a public issue on the source repository/);
	assert.match(S[6].warn, /without an account is planned, not built/);
	assert.equal(ISSUES, `${REPO}/issues`);
	assert.ok(guide.includes(ISSUES));
	// Review S4-R1 L2: step 8 says where the status list comes from before the doctor reads it.
	assert.equal(STATUS_URL, `${HOSTED_NODE}/publishers.json`);
	assert.deepEqual(S[7].commands, [`curl -s -o publishers.json ${STATUS_URL}\nonyx-publish doctor --publishers publishers.json`]);
	// Review S4-R1 L4: the node in a command is the one the page is given, by default the hosted
	// test node, and the page gives none (a mirror's or a preview's own origin is not a node).
	assert.ok(steps('https://node.example').slice(8).every((s) => s.commands.every((c) => c.includes('https://node.example') && !c.includes(HOSTED_NODE))));
	assert.ok(S.slice(8).every((s) => s.commands.every((c) => c.includes(HOSTED_NODE))));
	const page = await readFile(repo('apps/web/src/routes/publish/+page.svelte'), 'utf8');
	const api = await readFile(repo('apps/web/src/routes/api/+page.svelte'), 'utf8');
	assert.match(page, /const STEPS = steps\(\);/);
	for (const src of [page, api]) assert.doesNotMatch(src, /location\.(origin|host|protocol)/);
	// Visitor text carries no internal code.
	for (const s of S) for (const t of [s.title, s.time, s.what, s.warn ?? '']) assert.doesNotMatch(t, /ADR|THREATS|PUBON|READINESS|ROADMAP/);
	for (const w of WORDS) assert.doesNotMatch(w.word + w.means, /ADR|THREATS|PUBON|READINESS|ROADMAP/);
	assert.deepEqual(WORDS.map((w) => w.word), ['Signed book', 'Hash', 'SPDX id', 'Test ETH']);
});

test('every subcommand the page names is one the packaged tool has, by its guide and its own usage', async () => {
	// Review S4-R1 H1: the page must not send a newcomer to a command the download does not have.
	// The guide is the README inside the download (scripts/release-build.sh), the usage is the
	// program's own; a page command with a subcommand neither lists fails here.
	const usage = await readFile(repo('crates/onyx-publish/src/lib.rs'), 'utf8');
	const has = [...usage.matchAll(/^\/\/! onyx-publish ([a-z-]+)/gm)].map((m) => m[1]);
	const subs = subcommands();
	assert.deepEqual(subs, ['keygen', 'keys-doc', 'build', 'sign', 'apply', 'doctor', 'upload']);
	for (const sub of subs) {
		assert.ok(has.includes(sub), `onyx-publish ${sub} is in the program's usage`);
		assert.ok(guide.includes(`onyx-publish ${sub} `), `onyx-publish ${sub} is in the guide`);
	}
	// The same for every flag of a page command: the guide shows it on that subcommand.
	for (const line of steps().flatMap((s) => s.commands.flatMap((c) => c.split('\n'))).filter((l) => l.startsWith('onyx-publish '))) {
		const [, sub] = line.split(' ');
		for (const flag of line.match(/--[a-z-]+/g) ?? []) assert.ok(new RegExp(`onyx-publish ${sub} [^\\n\`]*${flag}\\b`).test(guide) || new RegExp(`\`${flag}[ \`]`).test(guide), `${sub} ${flag} is in the guide`);
	}
	// A planted command with a subcommand the tool does not have would be caught.
	assert.ok(!has.includes('enroll') && !guide.includes('onyx-publish enroll '));
	const release = await readFile(repo('scripts/release-build.sh'), 'utf8');
	// The download holds the two programs and this guide as its README (the page says so).
	assert.ok(release.includes('cp docs/ops/bulk-publish.md "$stage/onyx-publish/README.md"'));
	assert.ok(release.includes('cp "$target/release/onyx-publish" "$target/release/onyx-check" "$stage/onyx-publish/bin/"'));
	assert.ok(release.includes(`pack onyx-publish ${TOOLS_FILE}`));
});

test('the tools: the file of the release, its programs on the path, a build pinned to the same release', () => {
	assert.equal(TOOLS_FILE, 'onyx-publish-linux-x86_64.tar.gz');
	assert.equal(TOOLS_URL, BASE + TOOLS_FILE);
	// Review S4-R1 M1: every command is a bare `onyx-publish ...`, so both ways end with a PATH line.
	assert.deepEqual(UNPACK, [`tar -xzf ${TOOLS_FILE}`, 'export PATH="$PWD/onyx-publish/bin:$PATH"']);
	assert.deepEqual(buildFromSource('v9.9.9-testnet'), [`git clone --branch v9.9.9-testnet ${REPO} && cd onyx`, 'cargo build --release -p onyx-publish -p onyx-check', 'export PATH="$PWD/target/release:$PATH"']);
	assert.deepEqual(buildFromSource(), buildFromSource(RELEASE));
	for (const c of [...UNPACK, ...buildFromSource()]) assert.ok(inGuide(c), `in the guide: ${c}`);
	// Review S4-R1 H1: a release from before the onboarding commands is never offered as the tools.
	assert.equal(ONBOARDING_SINCE, 'v0.5.0-testnet');
	assert.equal(hasOnboarding('v0.4.0-testnet'), false);
	assert.equal(hasOnboarding('v0.4.9-testnet'), false);
	assert.equal(hasOnboarding('v0.5.0-testnet'), true);
	assert.equal(hasOnboarding('v0.5.1-testnet'), true);
	assert.equal(hasOnboarding('v1.0.0-testnet'), true);
	for (const bad of ['v0.5.0', 'latest', '']) assert.equal(hasOnboarding(bad), false, bad);
	assert.equal(hasOnboarding(), hasOnboarding(RELEASE));
	assert.ok(guide.includes(ONBOARDING_SINCE));
});

test('the page and the status box build no markup and no link from the list', async () => {
	// Review S4-R1 M3: the parser refusing a hostile file is one guard; the other is that nothing
	// the list holds can become markup or a link target. Read from the two component files.
	const box = await readFile(repo('apps/web/src/lib/components/PublisherStatus.svelte'), 'utf8');
	const page = await readFile(repo('apps/web/src/routes/publish/+page.svelte'), 'utf8');
	for (const [name, src] of [['the status box', box], ['the page', page]]) {
		for (const bad of ['{@html', 'innerHTML', 'outerHTML', 'insertAdjacentHTML', 'document.write', 'srcdoc', 'DOMParser', 'eval(', 'new Function']) assert.ok(!src.includes(bad), `${name} has no ${bad}`);
		assert.doesNotMatch(src, /\son[a-z]+="/, `${name} has no inline handler`);
	}
	// The status box holds no link and no source at all: an origin, an id or a hash is never one.
	assert.doesNotMatch(box, /<a[\s>]|href|src=|window\.open|location\./);
	// The page's links are its own constants, never a value of the list.
	const hrefs = [...page.matchAll(/href=\{([^}]+)\}|href="([^"]+)"/g)].map((m) => m[1] ?? m[2]);
	assert.deepEqual([...new Set(hrefs)].sort(), ['GUIDE', 'ISSUES', 'TOOLS_URL', "resolve('/submit')"]);
	assert.doesNotMatch(page, /publishers\.ts'.*\b(parseStatus|lookup)\b/);
});
