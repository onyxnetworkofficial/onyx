// U1: historical "home" verification assertions now target #/how-it-works. Keep their
// names stable for THREATS citations; tests/product.mjs covers the new contributor homepage.
// W12 continuous styling, round 1 (docs/website-plan.md), in a real browser. Called by
// tests/e2e.mjs; also runs alone against a build: node tests/w12.mjs [build dir].
// Section openers (number and rule on every titled section, kept out of the heading's name);
// a kicker on each inner page's slab in nav order; sentences in the body face (no long mono
// paragraphs, section ledes at body size in a 68-character measure); one figure tile on Home,
// Data and Run; a footer with the page list and the network's state; the Reserves comparison
// table fits its panel; the proof ladder's status reads in the body face.
// Round 2: one section rhythm; two-panel sections side by side; Home's preview charts show their
// scale; phone check rows tucked under 100 px with 44 px folds; a proof card's focus ring.
// Round 3: code names and values in running text never break inside themselves or overflow; notes
// are their panel's footer row; Run's fact cards and tiles share the figure tile's surface (one
// grouped card a row per fact on phones, 44 px rows); Home's reserves card column is the wider one;
// the OKX preview cuts the one outlier block with a break mark and its value.
// Round 4: one data-table switch per panel of small multiples; chain operators travel with their
// values; Run's notes are footers or note bars; Reserves' API facts are cards; Home's proof cards
// end on a link row at one height; level-2 facts stack on a phone; the company switch sits right.
// Round 5: Data's long ledes and the SEC note in two text columns on a wide screen; the pool note a
// footer; the API's "served so far" a full-width panel; Reserves' cards share one table switch and
// end on a footer; Home's title leads and its checks sit two a row.
// Round 6: Reserves' lede, API intro and note bar in two columns, each week line two labelled
// figures; the API's last section heading beside its text; Home's testnet note hugs its text; Run's
// steps on a numbered rail, check left and transactions right; Explorer's proof tree at the left
// with its controls beside it, the live blocks' companies a column each.
// Round 7: two-paragraph .duo columns end on about the same line; long panel footers in two columns,
// short ones on one line; Explorer's and Labels' notes are footers, side-by-side panels' footers
// line up; the site footer's list and state line sit beside the wordmark; the rehearsal's steps
// ring their numbers on a rail like the Base Sepolia steps.
// Round 8: Home's first screen (a 400 px drawing beside a two-line title, the proof cards' text on the
// first screen, a 44-48 px title on a phone); panel titles in the display face; Data's all-blocks
// table a panel in the section it lists; Labels' set card fills the right half and the vector
// results fill their card.
// Round 9: Explorer's live blocks scroll (keyboard too) when motion is off; Home's proof levels title in
// the display face and the bento's second row splits evenly; Explorer's rerun and validator panels pair;
// each inner page's lede is a card beside its title.
// Round 10: Explorer's paired panels end on their controls; the API's side card holds its perks two a
// row; Home's proof levels are cards on a phone; Run's picker and Data's figure tiles sit beside their
// headings; Labels' buttons face the signed text; one nesting rule for card corners and shadows, one
// motion scale and one opener height.
// Round 11: Run's run switch reaches the right edge and its options hover in place; Run's step panels
// take the panel padding; Reserves' company names and Home's proof titles in the display face;
// Explorer's proof select on its own row; Labels' attestation buttons stack where they cannot share a
// line; the Reserves comparison's figures align right; tabular figures and no double rule under tables.
// Round 12: item titles (Home's roles, Run's steps and earning cards) in the display face; Run's round facts
// on one line each; Labels' test vectors end on their button; Data's first heading on one line.
// Round 13: Data's "Example" heading in the display face; Home's pipeline notes in two lines; Run's gantt names a
// size above their detail; sections below the first screen fade in (motion on only, never the first screen).
// Round 14: Home's check names and Data's track-record headlines are item titles in the display face (the empty
// panel's title too); Home's pipeline notes in two lines at 761-999 px; Labels' Brier panel a form beside the vectors.
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { attr, capWaits, settled, WAIT_CAP } from './wait.mjs';

const PAGES = [
	['', 'How it works'],
	['data', 'Browse data'],
	['reserves', 'Reserves'],
	['run', 'Run'],
	['explorer', 'Explorer'],
	['labels', 'Labels'],
	['api', 'API'],
	['submit', 'Add data'],
	['markets', 'Markets']
];

// The chart helpers' BTC tick and scale step (src/lib/chart.ts tickBtc, src/lib/live.ts nice).
const nice = (x) => {
	if (!(x > 0)) return 1;
	const p = 10 ** Math.floor(Math.log10(x));
	for (const m of [1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10]) if (m * p >= x - 1e-9) return m * p;
	return 10 * p;
};
// The shape of a .duo (W12 rounds 5-7), run in the page: 'two' (two text columns with a rule between:
// one paragraph flowing in CSS columns, or two paragraphs side by side, the second's left edge the
// rule), 'one' (one column), else 'other'; for two paragraphs side by side, how far apart the two
// last lines end, in lines.
function duoShape(sel) {
	const lastLine = (e) => { const r = document.createRange(); r.selectNodeContents(e); const q = [...r.getClientRects()].filter((x) => x.width > 1); return Math.max(...q.map((x) => x.bottom)); };
	return [...document.querySelectorAll(sel)].filter((d) => d.checkVisibility()).map((d) => {
		const s = getComputedStyle(d);
		const ps = [...d.children].filter((c) => c.matches('p') && c.checkVisibility());
		if (s.display === 'flex' && ps.length === 2) {
			const [a, b] = ps.map((x) => x.getBoundingClientRect());
			const lh = parseFloat(getComputedStyle(ps[0]).lineHeight);
			// One rule: the second paragraph's edge, no column rule painted in the gap as well.
			const side = b.left > a.right && Math.abs(a.top - b.top) < 2 && getComputedStyle(ps[1]).borderLeftStyle === 'solid' && s.columnRuleStyle === 'none';
			return { shape: side ? 'two' : 'other', paras: 2, apart: Math.abs(lastLine(ps[0]) - lastLine(ps[1])) / lh, text: d.textContent.trim().slice(0, 30) };
		}
		const r = document.createRange();
		r.selectNodeContents(d);
		const box = d.getBoundingClientRect();
		const pl = parseFloat(s.paddingLeft);
		const inner = d.clientWidth - pl - parseFloat(s.paddingRight);
		const gap = parseFloat(s.columnGap) || 0;
		const lefts = [...r.getClientRects()].filter((q) => q.width > 4).map((q) => q.left - box.left - d.clientLeft - pl);
		const left = lefts.some((x) => Math.abs(x) < 2);
		const right = lefts.some((x) => Math.abs(x - (inner + gap) / 2) < 2);
		const plefts = ps.map((x) => x.getBoundingClientRect().left);
		const shape = s.columnCount === '2' && s.columnRuleStyle === 'solid' && left && right ? 'two' : s.columnCount === 'auto' && s.display !== 'flex' && left && new Set(plefts.map(Math.round)).size <= 1 ? 'one' : 'other';
		// Two paragraphs in CSS columns (one a column, as before round 7): how far apart they end.
		const apart = shape === 'two' && ps.length === 2 ? Math.abs(lastLine(ps[0]) - lastLine(ps[1])) / parseFloat(getComputedStyle(ps[0]).lineHeight) : 0;
		return { shape, paras: ps.length, apart, text: d.textContent.trim().slice(0, 30) };
	});
}

const tick = (a) => (a >= 100 ? Math.round(a).toLocaleString('en-US') : a >= 1 ? a.toFixed(1) : a.toFixed(2));

export async function w12(browser, base, root, expect) {
	const demo = JSON.parse(await readFile(join(root, 'data/demo.json'), 'utf8'));
	const PUB = demo.public === true;
	// ROADMAP L14: a rights-fallback build has no reserve table (its section is held back).
	const FB = demo.rights === 'fallback';
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce', isMobile: width < 500, hasTouch: width < 500 });
		const at = `W12 ${width} px`;
		const tiles = [];
		for (const [i, [page, label]] of PAGES.entries()) {
			const p = await ctx.newPage();
			await p.goto(base + (page ? `#/${page}` : '#/how-it-works'));
			await p.locator('main').waitFor();
			await settled(p);
			const name = page || 'home';
			const S = await p.evaluate(() => {
				const vis = (e) => e.checkVisibility();
				const secs = [...document.querySelectorAll('main section.section')].filter((s) => vis(s) && s.querySelector('h2'));
				const openers = secs.map((s) => {
					const b = getComputedStyle(s, '::before');
					const sr = s.getBoundingClientRect();
					return { content: b.content, color: b.color, line: b.backgroundImage.startsWith('linear-gradient'), full: Math.abs(parseFloat(b.width) - sr.width) < 2 || b.gridColumnStart === '1', h2: s.querySelector('h2').textContent.trim() };
				});
				// Accessible name of each h2: Chrome's computed name (pseudo content with alt "" adds nothing).
				const kick = document.querySelector('[data-testid="kicker"]');
				const h1 = document.querySelector('main h1');
				const mono = [...document.querySelectorAll('main p')].filter((e) => vis(e) && /Plex Mono|monospace/.test(getComputedStyle(e).fontFamily)).map((e) => e.textContent.trim().replace(/\s+/g, ' '));
				const ledes = [...document.querySelectorAll('main .section .lede')].filter(vis).map((e) => [getComputedStyle(e).fontSize, getComputedStyle(e).maxWidth, getComputedStyle(e).fontFamily]);
				const tiles = [...document.querySelectorAll('main .stat')].filter(vis).map((t) => {
					const s = getComputedStyle(t);
					const b = t.querySelector(':scope > b');
					const l = t.querySelector(':scope > span');
					const bs = getComputedStyle(b);
					return { pad: s.padding, radius: s.borderRadius, shadow: s.boxShadow, fig: bs.fontSize, figFont: bs.fontFamily, label: getComputedStyle(l).fontSize, oneLine: b.getBoundingClientRect().height < parseFloat(bs.fontSize) * 1.9 };
				});
				const foot = document.querySelector('footer');
				const flinks = [...foot.querySelectorAll('[data-testid="footer-nav"] a')].map((a) => [a.textContent.trim(), a.getAttribute('href'), a.getAttribute('aria-current')]);
				const hlinks = [...document.querySelectorAll('header nav a')].map((a) => [a.textContent.trim(), a.getAttribute('href')]);
				const fstate = foot.querySelector('.fstate')?.textContent.trim().replace(/\s+/g, ' ');
				const ftaps = [...foot.querySelectorAll('a')].map((a) => a.getBoundingClientRect()).map((r) => [r.width, r.height]);
				return {
					openers,
					kick: kick && { text: kick.textContent.trim(), hidden: kick.getAttribute('aria-hidden'), before: kick.compareDocumentPosition(h1) & Node.DOCUMENT_POSITION_FOLLOWING, font: getComputedStyle(kick).fontFamily },
					mono,
					ledes,
					tiles,
					flinks,
					hlinks,
					fstate,
					ftaps,
					landmarks: document.querySelectorAll('nav').length
				};
			});
			// 1. Section openers: every titled section, numbered 01, 02, ... in page order, a hairline
			//    across the section, the number in --muted, the heading's spoken name without it.
			// Explorer is one step timeline with its own rail (no sections), so it has none.
			const counters = S.openers.map((o) => o.content);
			expect(
				(S.openers.length > 0 || page === 'explorer') && S.openers.every((o) => /counter\(section, decimal-leading-zero\) \/ ""/.test(o.content) && o.line && o.full),
				`${at} ${name}: all ${S.openers.length} titled sections open with a number and a hairline across the section${S.openers.length || page === 'explorer' ? '' : ' (none found)'}${counters.some((c) => !/counter/.test(c)) ? ': ' + counters.join(' | ') : ''}`
			);
			if (width === 1280) {
				const names = await p.evaluate(() => [...document.querySelectorAll('main section.section h2')].filter((h) => h.checkVisibility()).map((h) => h.innerText.trim()));
				// The spoken names, from Chrome's accessibility tree.
				const cdp = await p.context().newCDPSession(p);
				const { nodes } = await cdp.send('Accessibility.getFullAXTree');
				const spoken = nodes.filter((n) => n.role?.value === 'heading' && n.properties?.some((q) => q.name === 'level' && q.value.value === 2)).map((n) => n.name?.value ?? '');
				await cdp.detach();
				const nums = S.openers.length;
				expect(
					names.length >= nums && spoken.length >= nums && names.every((n) => !/^\d{2}\b/.test(n)) && spoken.every((n) => !/^\d{2}\b/.test(n)),
					`${at} ${name}: section numbers are not part of any h2's text or spoken name (${names.length} headings)`
				);
			}
			// 2. Kicker: inner pages show "0N / Page" in nav order above the h1 (hidden from screen
			//    readers, the nav already says it); Home has none (its hero opens the page).
			if (i === 0) expect(S.kick === null, `${at} home: no kicker above the hero`);
			else
				expect(
					S.kick && S.kick.text === label && S.kick.hidden === 'true' && S.kick.before && /Plex Mono/.test(S.kick.font),
					`${at} ${name}: the slab's kicker reads "${label}" above the title (${S.kick?.text})`
				);
			// 3. Sentences in the body face: no paragraph over 120 characters is set in mono; section
			//    ledes at --text-base (15 px) in a 68-character measure.
			const longMono = S.mono.filter((t) => t.length > 120);
			expect(longMono.length === 0, `${at} ${name}: no paragraph over 120 characters in mono${longMono.length ? ': ' + longMono.map((t) => t.slice(0, 50)).join(' | ') : ''}`);
			expect(
				S.ledes.every(([fs, mw, ff]) => fs === '15px' && /^\d/.test(mw) && /Plex Sans/.test(ff)),
				`${at} ${name}: all ${S.ledes.length} section ledes are 15 px body text in a measure${S.ledes.length ? ' (' + [...new Set(S.ledes.map((l) => l.join(' ')))].join('; ') + ')' : ''}`
			);
			tiles.push(...S.tiles.map((t) => ({ ...t, page: name })));
			// 4. Footer: every page in nav order (same links as the header), the current one marked,
			//    the network's state, one nav landmark per page (W3), 44 px links on phones (W2).
			expect(
				S.flinks.length === 10 && S.hlinks.length === 4 && S.hlinks.every(([t, h]) => S.flinks.some(([ft, fh]) => ft === t && fh === h)) && S.flinks.filter((l) => l[2] === 'page').map((l) => l[0]).join() === label && S.landmarks === 1,
				`${at} ${name}: the footer lists all 10 pages and includes the three primary destinations, ${label} current, without a second nav landmark`
			);
			expect(S.fstate === `Testnet only · ${PUB ? 'early preview' : 'private preview'}`, `${at} ${name}: the footer states "Testnet only · ${PUB ? 'early preview' : 'private preview'}" (${S.fstate})`);
			if (width < 500) expect(S.ftaps.every(([w, h]) => w >= 43.5 && h >= 43.5), `${at} ${name}: every footer link is a 44 x 44 px tap target`);
			// 5. Reserves: the comparison table fits its panel (its last column was cut at 1280 px).
			if (page === 'reserves' && width === 1280 && FB) {
				const held = await p.evaluate(() => !!document.querySelector('[data-testid="reserves-held-back"]') && !document.querySelector('[data-testid="reserve-live"]'));
				expect(held, 'W12 1280 px reserves: rights fallback, the reserve section is held back and no comparison table is drawn');
			} else if (page === 'reserves' && width === 1280) {
				const fit = await p.evaluate(() => {
					const t = document.querySelector('[data-testid="reserve-live"] table');
					const panel = t.closest('.panel').getBoundingClientRect();
					return { sw: t.scrollWidth, cw: t.clientWidth, right: t.getBoundingClientRect().right, edge: panel.right };
				});
				expect(fit.sw <= fit.cw + 1 && fit.right <= fit.edge, `${at} reserves: the "Checked against" table fits its panel (${fit.sw} / ${fit.cw} px)`);
			}
			// 6. Home: the proof ladder's status is body text, left-set (was right-set mono).
			if (i === 0) {
				const st = await p.evaluate(() => [...document.querySelectorAll('.lstat')].map((e) => [getComputedStyle(e).fontFamily, getComputedStyle(e).textAlign, Math.round(e.getBoundingClientRect().height / parseFloat(getComputedStyle(e).lineHeight))]));
				expect(st.length === 5 && st.every(([f, a, lines]) => /Plex Sans/.test(f) && a !== 'right' && lines <= 3), `${at} home: the 5 ladder statuses are body text, at most 3 lines (${st.map((s) => s[2]).join(', ')})`);
			}
			// Round 2: one vertical rhythm (48 px above and below each section, 32 on a phone).
			const pads = await p.evaluate(() => [...new Set([...document.querySelectorAll('main section.section')].filter((s) => s.checkVisibility() && !s.matches('.two, .section + .section') ).map((s) => getComputedStyle(s).paddingBottom))]);
			expect(pads.length <= 1 && pads.every((v) => v === (width < 500 ? '32px' : '48px')), `${at} ${name}: every section has ${width < 500 ? 32 : 48} px below it (${pads.join(', ')})`);
			// ROADMAP L14: a rights-fallback build holds the reserve section back, so only Labels has the pair there.
			if (width === 1280 && ((page === 'reserves' && !FB) || page === 'labels')) {
				const two = await p.evaluate(() => [...document.querySelectorAll('section.section.two > .panel')].map((e) => e.getBoundingClientRect()).map((r) => [Math.round(r.top), Math.round(r.height)]));
				expect(two.length === 2 && two[0][0] === two[1][0] && two[0][1] === two[1][1], `${at} ${name}: the two-panel section sits side by side, equal heights (${JSON.stringify(two)})`);
			}
			if (i === 0) {
				const R2 = await p.evaluate(() => ({
					scale: [...document.querySelectorAll('.chart.compact')].map((c) => [...c.querySelectorAll('svg text.axis')].map((t) => t.textContent.trim())),
					rows: [...document.querySelectorAll('.checks li')].map((l) => l.getBoundingClientRect().height),
					sums: [...document.querySelectorAll('.why summary')].map((e) => e.getBoundingClientRect().height)
				}));
				expect(R2.scale.length >= 1 && R2.scale.every((t) => t.length >= 2 && t.includes('0')), `${at} home: all ${R2.scale.length} drawn preview charts show their scale (${R2.scale.map((t) => t.join(' ')).join(' | ')})`);
				if (width < 500) expect(R2.rows.length === (FB ? 5 : 7) && R2.rows.every((h) => h <= 100) && R2.sums.every((h) => h >= 43.5), `${at} home: check rows at most 100 px with 44 px folds (${R2.rows.map(Math.round).join(', ')})`);
				else {
					await p.locator(`[data-testid="${FB ? 'proof-held-back' : 'proof-reserves'}"] .plink`).focus();
					const ring = await p.evaluate(() => { const a = document.activeElement; const s = getComputedStyle(a, '::after'); const r = a.closest('.proof').getBoundingClientRect(); const q = a.getBoundingClientRect(); return { ring: s.outlineStyle === 'solid' && parseFloat(s.outlineWidth) >= 2, card: r.height > q.height + 40 }; });
					expect(ring.ring && ring.card, `${at} home: a focused proof card draws its ring round the whole card`);
				}
			}
			// Round 3, 1: inline code and mono values in running text sit on one line, inside their block.
			const split = await p.evaluate(() =>
				[...document.querySelectorAll(':is(p, li, dd, label) > :is(code, span.mono), :is(p, li, dd, label) > .cmd .nw')]
					.filter((e) => e.checkVisibility() && !e.classList.contains('cmd'))
					.filter((e) => e.getClientRects().length > 1 || e.getBoundingClientRect().right > e.closest('p, li, dd, label').getBoundingClientRect().right + 1)
					.map((e) => e.textContent.trim().slice(0, 40))
			);
			expect(split.length === 0, `${at} ${name}: no code name or value in running text breaks inside itself or overflows${split.length ? ': ' + split.join(' | ') : ''}`);
			// Round 3, 2: a note under a panel is that panel's last row, its hairline across the panel.
			if (page === 'run') for (const t of ['net-steps', 'idx-events-list']) await p.getByTestId(t).waitFor({ timeout: 150000 });
			// Round 13 gate: the Brier panel's scores (and its footer) arrive from the verifier once the browser is idle
			// (W4); at load average 30 the check once ran before them and counted 2 of Labels' 3 footers.
			if (page === 'labels') await p.locator('.score').waitFor({ timeout: 150000 });
			const feet = await p.evaluate(() =>
				[...document.querySelectorAll('.pfoot')].filter((e) => e.checkVisibility()).map((e) => {
					const panel = e.parentElement.closest('.panel');
					const ps = panel && getComputedStyle(panel);
					const inner = panel ? panel.clientWidth - parseFloat(ps.paddingLeft) - parseFloat(ps.paddingRight) : 0;
					return { last: !!panel && e === e.parentElement.lastElementChild && e.parentElement === panel, rule: getComputedStyle(e).borderTopStyle === 'solid', full: Math.abs(e.getBoundingClientRect().width - inner) < 2 };
				})
			);
			// Round 4: Run's fee split and dispute panels, Home's proof cards and Reserves' comparison
			// table end on a footer row too.
			const want = { run: 5, data: 2, home: 3, reserves: FB ? 0 : 5, api: 1, explorer: (PUB ? 1 : 0) + (demo.live ? 1 : 0), labels: 3 }[name] ?? 0;
			expect(feet.length >= want && feet.every((f) => f.last && f.rule && f.full), `${at} ${name}: ${feet.length} panel notes are their panel's last row under a full-width hairline${want ? ` (at least ${want})` : ''}`);
			if (page === 'run') {
				// Round 3, 3: one card family on Run; on a phone each group of facts is one card.
				const F = await p.evaluate(() => {
					const look = (e) => { const s = getComputedStyle(e); return [s.borderTopLeftRadius, s.boxShadow, s.backgroundImage].join('|'); };
					const stat = document.querySelector('main .stat');
					const cards = [...document.querySelectorAll('main .fact, main .tile')].filter((e) => e.checkVisibility());
					const groups = [...document.querySelectorAll('main .facts')].filter((e) => e.checkVisibility());
					return {
						n: cards.length,
						same: cards.filter((c) => !c.parentElement.matches('.facts') || innerWidth > 560).every((c) => look(c) === look(stat)),
						groups: groups.map((g) => ({ look: look(g) === look(stat), rows: [...g.children].map((c) => [c.getBoundingClientRect().height, getComputedStyle(c).boxShadow]) }))
					};
				});
				if (width < 500) expect(F.groups.length >= 2 && F.groups.every((g) => g.look && g.rows.every(([h, sh]) => h >= 43.5 && sh === 'none')), `${at} run: ${F.groups.length} fact groups are one card each, a row of at least 44 px per fact`);
				else expect(F.n >= 6 && F.same, `${at} run: all ${F.n} fact cards and tiles share the figure tile's surface`);
			}
			if (i === 0 && width === 1280) {
				// Round 3, 4: the reserves card's column is the wider one (it holds two figures); equal
				// columns when the card is held back (rights fallback).
				const cols = await p.evaluate(() => [...document.querySelectorAll('.pgrid > li')].map((l) => l.getBoundingClientRect().width));
				expect(cols.length === 3 && (FB ? Math.abs(cols[1] - cols[0]) < 2 : cols[1] > cols[0] * 1.3 && Math.abs(cols[0] - cols[2]) < 2), `${at} home: proof card columns ${cols.map(Math.round).join(' / ')} px (${FB ? 'equal, card held back' : 'reserves card widest'})`);
			}
			if (i === 0 && !FB && demo.live?.per_block) {
				// Round 3, 5: the OKX preview fits the next-busiest block and cuts the outlier, with its value.
				const v = demo.live.per_block.map((b) => b.rows.find((r) => r.entity === 'entity:okx')).map((r) => [Number(r?.received_sats ?? 0) / 1e8, Number(r?.sent_sats ?? 0) / 1e8]);
				const peaks = v.map(([a, b]) => Math.max(a, b)).sort((a, b) => b - a);
				const max = peaks[0] > 3 * peaks[1] ? nice(peaks[1]) : nice(peaks[0]);
				const cuts = v.flat().filter((x) => x > max).map(tick).sort();
				await p.locator('a.panel.preview').scrollIntoViewIfNeeded();
				await p.waitForFunction(() => document.querySelectorAll('a.panel.preview .chart.compact').length === 2, null, { timeout: 150000 });
				const C = await p.evaluate(() => {
					const c = document.querySelectorAll('a.panel.preview .chart.compact')[1];
					return { cuts: c.querySelectorAll('path.cut').length, labels: [...c.querySelectorAll('g.mark text.axis')].map((t) => t.textContent.trim()).sort(), top: c.querySelector('svg > text.axis')?.textContent.trim() };
				});
				expect(C.cuts === cuts.length && JSON.stringify(C.labels) === JSON.stringify(cuts) && C.top === tick(max), `${at} home: the OKX preview's scale is ${C.top} (export: ${tick(max)}), ${C.cuts} bars cut with their values ${C.labels.join(', ')} (export: ${cuts.join(', ')})`);
			}
			// Round 4, 1: one data-table switch per panel of small multiples; it opens every chart's
			// table right under its chart and closes them again (no "Data table" fold per chart).
			if (page === 'data' && !FB) {
				for (const id of ['series-legend', 'history-legend', 'eth-legend']) await p.getByTestId(id).waitFor({ timeout: 150000 });
				const G = await p.evaluate(() =>
					[...document.querySelectorAll('.multiples')].map((m) => ({
						switches: m.querySelectorAll('.tablesw').length,
						charts: m.querySelectorAll('.chart').length,
						tabled: [...m.querySelectorAll('.chart')].filter((c) => c.nextElementSibling?.matches('details.datatable.grouped')).length,
						folds: [...m.querySelectorAll('details.datatable > summary')].filter((e) => e.checkVisibility()).length,
						// A disclosure button: collapsed by default, aria-controls names every table it opens.
						collapsed: m.querySelector('.tablesw')?.getAttribute('aria-expanded') === 'false',
						controls: (() => { const want = [...m.querySelectorAll('details.datatable')].map((d) => d.id).sort(); const got = (m.querySelector('.tablesw')?.getAttribute('aria-controls') ?? '').split(' ').filter(Boolean).sort(); return want.length > 0 && want.every(Boolean) && JSON.stringify(want) === JSON.stringify(got); })(),
						h: m.querySelector('.tablesw')?.getBoundingClientRect().height ?? 0
					}))
				);
				// ADR-0043 and ADR-0044: the Bitcoin and the Ethereum on-chain sections are the third and
				// fourth panels of small multiples.
				expect(G.length === 4 && G.every((g) => g.switches === 1 && g.charts > 0 && g.tabled === g.charts && g.folds === 0 && g.collapsed && g.controls && g.h >= (width < 500 ? 43.5 : 30)), `${at} data: ${G.length} panels of small multiples, one collapsed table switch each naming its tables in aria-controls, no per-chart fold (${G.map((g) => `${g.charts} charts`).join(', ')})`);
				const sw = p.getByTestId('history-tables');
				if ((await sw.count()) === 0) expect(false, `${at} data: the history has a table switch`);
				else {
				await sw.scrollIntoViewIfNeeded();
				await sw.click();
				// Waits on the state the click sets (wait.mjs); a timeout throws, never swallowed.
				await attr(sw, 'aria-expanded', 'true');
				await p.waitForFunction(() => { const m = document.querySelector('[data-testid="history-tables"]').closest('.multiples'); const d = [...m.querySelectorAll('details.datatable')]; return d.length > 0 && d.every((e) => e.open && e.querySelector('tbody tr')); }, null, { timeout: WAIT_CAP, polling: 100 });
				const O = await p.evaluate(() => {
					const m = document.querySelector('[data-testid="history-tables"]').closest('.multiples');
					return { exp: document.querySelector('[data-testid="history-tables"]').getAttribute('aria-expanded'), open: [...m.querySelectorAll('.chart')].map((c) => (c.nextElementSibling.open && c.nextElementSibling.querySelector('tbody')?.rows.length) || 0) };
				});
				await sw.click();
				await attr(sw, 'aria-expanded', 'false');
				await p.waitForFunction(() => ![...document.querySelector('[data-testid="history-tables"]').closest('.multiples').querySelectorAll('details.datatable')].some((e) => e.open), null, { timeout: WAIT_CAP, polling: 100 });
				const shut = await p.evaluate(() => [...document.querySelector('[data-testid="history-tables"]').closest('.multiples').querySelectorAll('details.datatable')].filter((e) => e.open).length);
				expect(O.exp === 'true' && O.open.length > 0 && O.open.every((n) => n > 0) && shut === 0, `${at} data: the history's switch opens all ${O.open.length} tables under their charts (${O.open.join(', ')} rows) and closes them`);
				}
				// Round 4, 6: the company switch ends at the heading row's right edge (its class had
				// collided with the page container's .wrap: mid-row on a wide screen).
				if (width === 1280) {
				const seg = await p.evaluate(() => { const s = document.querySelector('.seg[aria-label="Company"]'); const r = s.getBoundingClientRect(); const h = s.parentElement.getBoundingClientRect(); return { gap: Math.round(h.right - r.right), pad: getComputedStyle(s).paddingLeft }; });
				expect(seg.gap <= 1 && seg.pad === '4px', `${at} data: the company switch sits at the heading row's right edge (${seg.gap} px short, padding ${seg.pad})`);
				}
			}
			// Round 4, 2: in a check's chain every operator sits on one line with its value.
			if (page === 'data' || page === 'run') {
				if (page === 'run') await p.getByTestId('dispute').waitFor({ timeout: 150000 });
				const ops = await p.evaluate(() =>
					[...document.querySelectorAll('.eq .op')].filter((o) => o.checkVisibility()).map((o) => {
						const pair = o.parentElement.matches('.eqp') ? [...o.parentElement.children].find((c) => c !== o) : null;
						if (!pair) return { alone: true };
						const a = o.getBoundingClientRect(), b = pair.getBoundingClientRect();
						return { alone: false, same: Math.abs(a.top + a.height / 2 - (b.top + b.height / 2)) < 4 };
					})
				);
				// Meeting polish MP16: on a phone the data page stacks a check's chain one step a line and hides
				// the operators, so none is left alone there.
				expect(ops.length >= (page === 'run' ? 2 : FB || (page === 'data' && width < 500) ? 0 : 6) && ops.every((o) => !o.alone && o.same), `${at} ${name}: all ${ops.length} chain operators travel with a value on one line`);
			}
			if (page === 'run') {
				// Round 4, 3: no note floats loose between Run's cards (each is a panel footer or a note bar).
				const loose = await p.evaluate(() => [...document.querySelectorAll('main section.section > p.fine')].filter((e) => e.checkVisibility()).map((e) => e.textContent.trim().slice(0, 40)));
				expect(loose.length === 0, `${at} run: no loose notes between the cards${loose.length ? ': ' + loose.join(' | ') : ''}`);
				const pf = await p.evaluate(() => { const c = document.querySelector('[data-testid="dispute-chain"]'); if (!c) return false; const panel = c.closest('.panel'); return !!panel && c.parentElement.matches('.pfoot') && panel.lastElementChild === c.parentElement; });
				expect(pf, `${at} run: the dispute chain is its panel's footer row`);
			}
			// Round 4, 4: Reserves' API facts are four cards: two a row on a wide screen, one grouped card
			// of stacked rows on a phone.
			if (page === 'reserves' && !FB) {
				const A = await p.evaluate(() => {
					const f = [...document.querySelectorAll('[data-testid="reserve-api"] dl.facts > .fact')];
					return { n: f.length, tops: [...new Set(f.map((e) => Math.round(e.getBoundingClientRect().top)))].length, stacked: f.every((e) => e.querySelector('dt').getBoundingClientRect().bottom <= e.querySelector('dd').getBoundingClientRect().top + 1), ep: (() => { const d = document.querySelector('[data-testid="reserve-api-endpoint"]'); return Math.round(d.getBoundingClientRect().height / parseFloat(getComputedStyle(d).lineHeight)); })() };
				});
				expect(A.n === 4 && A.stacked && (width < 500 ? A.tops === 4 : A.tops === 2 && A.ep === 1), `${at} reserves: the 4 API facts are cards, ${width < 500 ? 'one a row' : 'two a row, the endpoint on one line'}, name over value (${A.tops} rows)`);
			}
			// Round 4, 5: Home's proof cards end on their link row, under a hairline at one height.
			if (i === 0 && width === 1280) {
				const tops = await p.evaluate(() => [...document.querySelectorAll('.pgrid .proof > .pfoot')].map((e) => Math.round(e.getBoundingClientRect().top)));
				expect(tops.length === 3 && new Set(tops).size === 1, `${at} home: the 3 proof cards' link rows sit at one height (${tops.join(', ')})`);
			}
			// Round 4, 7: on a phone each level-2 fact's name sits over its value.
			if (page === 'data' && width < 500) {
				await p.getByTestId('level2-card').waitFor({ timeout: 150000 });
				const L2 = await p.evaluate(() => [...document.querySelectorAll('.l2facts > div')].filter((d) => d.checkVisibility()).map((d) => d.querySelector('dt').getBoundingClientRect().bottom <= d.querySelector('dd').getBoundingClientRect().top + 1));
				expect(L2.length >= 7 && L2.every(Boolean), `${at} data: all ${L2.length} level-2 facts put the name over the value`);
			}
			// Round 5, 1: Data's long ledes and the SEC source note read in two text columns on a wide
			// screen (the text's line boxes start at two lefts, a rule between them), one on a phone.
			if (page === 'data') {
				await p.getByTestId('sec-copy').waitFor({ timeout: 150000 });
				// ADR-0044: the Ethereum panel's footer exists once its window checked out.
				await p.getByTestId('eth-source').waitFor({ timeout: 150000 });
				const D = await p.evaluate(duoShape, 'main .duo');
				// Round 7: the pool panel's footer is a .duo too. ROADMAP P2c: the ETF section adds its lede and its note bar. ADR-0043: the on-chain section's ledes and
				// its panel's footer are two more (in every build).
				// ADR-0044: the Ethereum section's ledes and its panel's footer are two more (in every build).
				// NS3: the earthquakes and weather section's lede is one more (in every build).
				// Data review: a one-paragraph lede or note no longer flows in CSS columns (a sentence split across
				// the rule); it reads as one column. The .duo blocks left are two paragraphs side by side: the
				// on-chain and Ethereum footers, the history ledes, the earthquakes and weather ledes, the ETF note bar.
				const want = FB ? 4 : 5;
				expect(
					D.length === want && D.every((d) => d.shape === (width < 500 ? 'one' : 'two')),
					`${at} data: ${D.length} long ledes and notes (want ${want}) read in ${width < 500 ? 'one column' : 'two columns with a rule between'}`
				);
				const pn = await p.evaluate(() => { const n = document.querySelector('[data-testid="pool-note"]'); const f = n?.parentElement; return !!f && f.matches('.pfoot') && f.parentElement.matches('.panel') && f.parentElement.lastElementChild === f; });
				expect(pn, `${at} data: the pool panel's closing note is its footer row`);
			}
			// Round 5, 2: the API's "served so far" is its own panel across both columns, its four datasets
			// rows of one table (the per-run detail folded under "Run notes"), its closing line a footer.
			if (page === 'api') {
				const A = await p.evaluate(() => {
					const s = document.querySelector('[data-testid="api-scope"]');
					const sec = s.closest('section').getBoundingClientRect();
					const rows = s.querySelectorAll('table tbody > tr').length;
					return { panel: s.matches('.panel'), span: Math.abs(s.getBoundingClientRect().width - sec.width) < 2, n: rows, notes: !!s.querySelector('details [data-testid="api-scope-flow"]'), foot: s.lastElementChild.matches('.pfoot') };
				});
				// Meeting polish MP16: the mining-pool row (served nowhere) left the table for the run notes.
				expect(A.panel && A.span && A.n === 3 && A.notes && A.foot, `${at} api: "served so far" is a full-width panel, ${A.n} datasets in one table, run notes folded, a footer last`);
			}
			// Round 5, 3: Reserves' four cards share one "Data tables" switch (no fold per card); it opens
			// every card's table right under its chart, with rows.
			if (page === 'reserves' && !FB) {
				const sw = p.getByTestId('reserve-tables');
				const R = await p.evaluate(() => {
					const b = document.querySelector('[data-testid="reserve-tables"]');
					const ids = [...document.querySelectorAll('[data-testid="reserve-cards"] details.datatable')].map((d) => d.id).sort();
					const got = (b?.getAttribute('aria-controls') ?? '').split(' ').filter(Boolean).sort();
					return { exp: b?.getAttribute('aria-expanded'), named: ids.length === 4 && JSON.stringify(ids) === JSON.stringify(got), folds: [...document.querySelectorAll('[data-testid="reserve-cards"] summary')].filter((e) => e.checkVisibility()).length, h: b?.getBoundingClientRect().height ?? 0 };
				});
				let rows = [];
				if ((await sw.count()) === 1) {
					await sw.scrollIntoViewIfNeeded();
					await sw.click();
					await attr(sw, 'aria-expanded', 'true');
					await p.waitForFunction(() => { const d = [...document.querySelectorAll('[data-testid="reserve-cards"] details.datatable')]; return d.length === 4 && d.every((e) => e.open && e.querySelector('tbody tr')); }, null, { timeout: WAIT_CAP, polling: 100 });
					rows = await p.evaluate(() => [...document.querySelectorAll('[data-testid="reserve-cards"] .chart')].map((c) => (c.nextElementSibling?.matches('details.datatable[open]') && c.nextElementSibling.querySelector('tbody').rows.length) || 0));
					await sw.click();
					await attr(sw, 'aria-expanded', 'false');
				}
				expect(R.exp === 'false' && R.named && R.folds === 0 && R.h >= (width < 500 ? 43.5 : 30) && rows.length === 4 && rows.every((n) => n > 0), `${at} reserves: one collapsed "Data tables" switch names the 4 cards' tables, no fold per card, and opens them under their charts (${rows.join(', ')} rows)`);
			}
			// U1: the technical page has a shorter title (one or two lines), and its
			// checks sit two a row on a wide screen.
			if (i === 0) {
				const H = await p.evaluate(() => {
					const h = document.querySelector('main h1');
					const s = getComputedStyle(h);
					const li = [...document.querySelectorAll('.checks > li')].map((l) => l.getBoundingClientRect());
					// Review: in list order the checks run down the left column, then down the right.
					const down = li.every((r, k) => k === 0 || r.top > li[k - 1].top || (r.left > li[k - 1].left && r.top < li[k - 1].top));
					const firstCol = li.filter((r) => Math.round(r.left) === Math.round(li[0].left)).length;
					return { size: parseFloat(s.fontSize), lines: Math.round(h.getBoundingClientRect().height / parseFloat(s.lineHeight)), cols: new Set(li.map((r) => Math.round(r.left))).size, down, firstCol, n: li.length };
				});
				expect((width < 500 || (H.size >= 76 && (H.lines === 1 || H.lines === 2))) && H.cols === (width < 500 ? 1 : 2) && H.down && H.firstCol === (width < 500 ? H.n : Math.ceil(H.n / 2)), `${at} home: the title is ${H.size} px on ${H.lines} lines; the checks sit ${H.cols} a row, in list order down each column (${H.firstCol} of ${H.n} in the first)`);
			}
			// Round 6, 1: Reserves' lede and API intro read in two columns on a wide screen; each card's week
			// line is two labelled figures, each value on one line; the closing note spans the cards.
			if (page === 'reserves' && !FB) {
				const R6 = await p.evaluate(() => {
					const cols = [...document.querySelectorAll('[data-testid="reserves"] > .duo:not(.notebar), [data-testid="reserve-api"] .duo')].map((d) => getComputedStyle(d).columnCount);
					const wk = [...document.querySelectorAll('[data-testid^="reserve-change-"]')].map((w) => {
						const f = [...w.querySelectorAll(':scope > .fig')].map((x) => x.getBoundingClientRect());
						const v = [...w.querySelectorAll('.val')].map((x) => x.getBoundingClientRect().height / parseFloat(getComputedStyle(x).lineHeight));
						return { n: f.length, side: f.length === 2 && f[1].left > f[0].right - 1 && Math.abs(f[1].top - f[0].top) < 2, stacked: f.length === 2 && f[1].top >= f[0].bottom - 1, oneLine: v.every((l) => l < 1.5) };
					});
					const note = document.querySelector('[data-testid="reserves-note"]').parentElement;
					const cards = document.querySelector('[data-testid="reserve-cards"]').getBoundingClientRect();
					return { cols, wk, span: Math.abs(note.getBoundingClientRect().width - cards.width) < 2, noteCols: getComputedStyle(note).columnCount };
				});
				const wide = width >= 900;
				expect(
					R6.cols.length === 2 && R6.cols.every((c) => c === (wide ? '2' : 'auto')) && R6.noteCols === (wide ? '2' : 'auto') && R6.span,
					`${at} reserves: the lede, the API intro and the note bar read in ${wide ? 'two columns' : 'one column'}; the bar spans the cards`
				);
				expect(R6.wk.length === 4 && R6.wk.every((w) => w.n === 2 && w.oneLine && (wide ? w.side : w.stacked)), `${at} reserves: each week line is two labelled figures ${wide ? 'side by side' : 'one a row'}, each value on one line`);
			}
			// Round 6, 2: the API's closing section sets its heading beside its paragraph on a wide screen.
			if (page === 'api') {
				const A6 = await p.evaluate(() => {
					const s = document.querySelector('[data-testid="api-verify-default"]');
					const h = s.querySelector('h2').getBoundingClientRect();
					const t = s.querySelector('p').getBoundingClientRect();
					return { side: t.left > h.right && t.top < h.bottom, under: t.top >= h.bottom - 1 };
				});
				expect(width >= 900 ? A6.side : A6.under, `${at} api: "Your node checks the root" ${width >= 900 ? 'heading beside its paragraph' : 'heading over its paragraph'}`);
			}
			// Round 6, 3: Home's testnet note box hugs its text (on a phone the sentence wraps to the box).
			if (i === 0 && width >= 900) {
				const N = await p.evaluate(() => {
					const n = document.querySelector('[data-testid="home-note"]');
					const s = getComputedStyle(n);
					const inner = n.getBoundingClientRect().width - parseFloat(s.paddingLeft) - parseFloat(s.paddingRight) - 2;
					const kids = [...n.children].map((c) => { const r = document.createRange(); r.selectNodeContents(c); return Math.max(...[...r.getClientRects()].map((q) => q.right)) - n.getBoundingClientRect().left - parseFloat(s.paddingLeft) - 1; });
					return { slack: inner - Math.max(...kids) };
				});
				expect(N.slack < 6, `${at} home: the testnet note box ends with its text (${Math.round(N.slack)} px spare)`);
			}
			// Round 6, 4: Run's steps: each number in a ring on a rail; on a wide screen what happened and its
			// check on the left, the transactions and log on the right (left first in the source).
			if (page === 'run') {
				const S6 = await p.evaluate(() => {
					const lis = [...document.querySelectorAll('[data-testid="net-steps"] > li')];
					return lis.map((li) => {
						const n = li.querySelector('.n').getBoundingClientRect();
						const w = li.querySelector('.what');
						const d = li.querySelector('.did');
						if (!w) return { ring: false, rail: false, order: false, side: false, under: false };
						const wr = w.getBoundingClientRect();
						const dr = d?.getBoundingClientRect();
						return { ring: Math.abs(n.width - 28) < 1 && Math.abs(n.height - 28) < 1 && getComputedStyle(li.querySelector('.n')).borderTopLeftRadius === '50%', rail: getComputedStyle(li, '::before').width === '1px', order: !d || !!(w.compareDocumentPosition(d) & Node.DOCUMENT_POSITION_FOLLOWING), side: !dr || (dr.left > wr.right && Math.abs(dr.top - wr.top) < 2), under: !dr || dr.top >= wr.bottom - 1 };
					});
				});
				const wide = width >= 900;
				expect(S6.length >= 10 && S6.every((x) => x.ring && x.rail && x.order && (wide ? x.side : x.under)), `${at} run: ${S6.length} steps numbered in rings on a rail, ${wide ? 'check left and transactions right' : 'transactions under the check'}`);
				const nb = await p.evaluate(() => { const n = document.querySelector('[data-testid="net-notes"]'); const st = n?.parentElement.querySelector('.stats'); if (!st) return { span: false }; return { span: Math.abs(n.getBoundingClientRect().width - st.getBoundingClientRect().width) < 2 }; });
				const [ns] = await p.evaluate(duoShape, '[data-testid="net-notes"]');
				expect(nb.span && ns?.shape === (wide ? 'two' : 'one'), `${at} run: the run's note bar spans the tiles above it, in ${wide ? 'two columns' : 'one column'}`);
			}
			// Round 6, 5: Explorer's row proof: the tree at the panel's left, its controls beside it; the live
			// blocks' companies a column each on a wide screen.
			if (page === 'explorer' && !FB) {
				const E6 = await p.evaluate(() => {
					const pr = document.querySelector('.proof');
					if (!pr?.querySelector('.ctl')) return { left: false, beside: false, under: false };
					const panel = pr.closest('.panel');
					const ps = getComputedStyle(panel);
					const t = pr.querySelector('.tree').getBoundingClientRect();
					const c = pr.querySelector('.ctl').getBoundingClientRect();
					return { left: Math.abs(t.left - panel.getBoundingClientRect().left - parseFloat(ps.paddingLeft) - parseFloat(ps.borderLeftWidth)) < 2, beside: c.left >= t.right, under: c.top >= t.bottom - 1 };
				});
				const wide = width > 640;
				expect((wide ? E6.left && E6.beside : E6.under), `${at} explorer: the proof tree ${wide ? 'at the panel\'s left with its controls beside it' : 'over its controls'}`);
				if (demo.live) {
					await p.waitForFunction(() => document.querySelectorAll('[data-testid="live-flows"] > .fl').length >= 2, null, { timeout: 120000, polling: 100 });
					const lefts = await p.evaluate(() => [...document.querySelector('[data-testid="live-flows"]').children].map((f) => Math.round(f.getBoundingClientRect().left)));
					expect(new Set(lefts).size === (width >= 900 ? Math.min(lefts.length, 3) : 1), `${at} explorer: the live blocks' ${lefts.length} companies sit ${width >= 900 ? 'a column each' : 'one under another'}`);
				}
			}
			// Round 7, 1: every .duo reads in two columns with a rule on a wide screen and one on a phone; two
			// paragraphs side by side end within a line and a half of each other (they split 3 lines beside 5).
			if (page === 'run') await p.getByTestId('honest').waitFor({ timeout: 150000 });
			const DU = await p.evaluate(duoShape, 'main .duo');
			const wide7 = width >= 900;
			expect(
				DU.every((d) => d.shape === (wide7 ? 'two' : 'one') && (!wide7 || d.paras < 2 || d.apart <= 1.5)),
				`${at} ${name}: ${DU.length} .duo blocks read in ${wide7 ? 'two columns, two paragraphs ending within 1.5 lines' : 'one column'}${DU.filter((d) => d.shape !== (wide7 ? 'two' : 'one') || d.apart > 1.5).map((d) => `; "${d.text}" ${d.shape} ${d.apart.toFixed(1)}`).join('')}`
			);
			if (page === 'run' || page === 'data') {
				// Long footers are two-column .duo on Data; Run's footers read in one column (two CSS
				// columns split their sentences mid-line, 2026-10-29 review).
				const long = await p.evaluate(() => [...document.querySelectorAll('.pfoot.duo')].filter((e) => e.checkVisibility()).length);
				expect(page === 'run' ? long === 0 : long >= 1, `${at} ${name}: ${long} long panel footers are .duo`);
			}
			// Round 7, 2: a short footer line reads on one line on a wide screen (API: it broke at 68 characters).
			if (page === 'api' && wide7) {
				const one = await p.evaluate(() => { const f = document.querySelector('[data-testid="api-scope"] > .pfoot > .fine'); return f.getBoundingClientRect().height / parseFloat(getComputedStyle(f).lineHeight); });
				expect(one < 1.5, `${at} api: the served panel's footer is one line (${one.toFixed(1)} lines)`);
			}
			// Round 7, 3: two panels side by side end their footers on one line (Labels' vectors and Brier score).
			if (page === 'labels') {
				await p.waitForFunction(() => document.querySelectorAll('.section.two > .panel > .pfoot:last-child').length === 2, null, { timeout: 120000, polling: 100 });
				const T = await p.evaluate(() => [...document.querySelectorAll('.section.two > .panel > .pfoot:last-child')].map((f) => Math.round(f.getBoundingClientRect().top)));
				expect(width < 861 || Math.abs(T[0] - T[1]) < 2, `${at} labels: the two side-by-side panels' footers line up (${T.join(', ')})`);
			}
			// Round 7, 4: the site footer's page list and state line share the column beside the wordmark,
			// the list four a row; on a phone they stack under it, two a row.
			if (i === 0) {
				const F7 = await p.evaluate(() => {
					const big = document.querySelector('footer .big').getBoundingClientRect();
					const nav = document.querySelector('[data-testid="footer-nav"]').getBoundingClientRect();
					const line = document.querySelector('footer .fline').getBoundingClientRect();
					const lefts = new Set([...document.querySelectorAll('[data-testid="footer-nav"] li')].map((l) => Math.round(l.getBoundingClientRect().left)));
					return { gap: nav.left - big.right, lineLeft: Math.abs(line.left - nav.left) < 2, perRow: lefts.size, under: line.top >= nav.bottom - 1, stacked: nav.top >= big.bottom - 1 };
				});
				expect(
					width > 760 ? F7.gap > 0 && F7.gap <= 80 && F7.lineLeft && F7.perRow === 4 && F7.under : F7.stacked && F7.perRow === 2 && F7.under,
					`${at} footer: the page list ${width > 760 ? `${Math.round(F7.gap)} px beside the wordmark, four a row, the state line under it` : 'and state line under the wordmark, two a row'}`
				);
			}
			// Round 7, 5: the local rehearsal's steps: each number a 28 px ring on a 1 px rail, as Run's
			// Base Sepolia steps; the number is real text (its position, 1 first), so it is read out too.
			if (page === 'run') {
				const G = await p.evaluate(() =>
					[...document.querySelectorAll('ol.gantt > li')].map((li) => {
						const n = li.querySelector(':scope > .n');
						const r = n ? n.getBoundingClientRect() : null;
						const a = getComputedStyle(li, '::after');
						const lab = li.querySelector('.lab').getBoundingClientRect();
						const pos = [...li.parentElement.children].indexOf(li) + 1;
						return { ring: !!r && Math.abs(r.width - 28) < 1 && Math.abs(r.height - 28) < 1 && getComputedStyle(n).borderTopLeftRadius === '50%' && n.textContent.trim() === String(pos) && n.getAttribute('aria-hidden') === null, rail: a.width === '1px' && a.position === 'absolute', after: lab.left - li.getBoundingClientRect().left >= 28 };
					})
				);
				expect(G.length >= 10 && G.every((g) => g.ring && g.rail && g.after), `${at} run: ${G.length} rehearsal steps numbered in rings (the number read out) on a rail, the step name after the ring`);
			}
			// Round 8, 1: Home's first screen. Wide: the drawing is a 380-400 px column beside a two-line
			// title, and the first proof card's text starts on the first screen (900 px tall). Phone: the
			// title is 44-48 px on two lines, the drawing left out.
			if (i === 0) {
				const H = await p.evaluate(() => {
					const h = document.querySelector('main h1');
					const v = document.querySelector('.vizslot .viz');
					const vr = v?.getBoundingClientRect();
					return { fs: parseFloat(getComputedStyle(h).fontSize), lines: Math.round(h.getBoundingClientRect().height / parseFloat(getComputedStyle(h).lineHeight)), viz: vr && v.checkVisibility() ? vr.width : 0, vizRight: vr ? vr.left > h.getBoundingClientRect().right : false, ptop: document.querySelector('.proof p').getBoundingClientRect().top + scrollY, vh: innerHeight };
				});
				if (width === 1280) expect((H.lines === 1 || H.lines === 2) && H.viz >= 380 && H.viz <= 401 && H.vizRight && H.ptop < H.vh - 20, `${at} home: a ${Math.round(H.viz)} px drawing beside a ${H.lines}-line title, the first proof card's text from ${Math.round(H.ptop)} px down the page (first screen ${H.vh} px)`);
				else expect((H.lines === 1 || H.lines === 2) && H.fs >= 44 && H.fs <= 48 && H.viz === 0, `${at} home: the title ${H.fs} px on ${H.lines} lines, no drawing`);
			}
			// Round 8, 2: a panel's own title that is not a heading (Home's tiles, Data's check cards) is
			// set like the panel headings: the display face at the panel heading's size, not bold.
			if (i === 0 || page === 'data') {
				const T = await p.evaluate(() => {
					const probe = Object.assign(document.createElement('div'), { className: 'panel' });
					probe.innerHTML = '<div class="sh"><h2>x</h2></div>';
					document.querySelector('main').append(probe);
					const ref = getComputedStyle(probe.querySelector('h2'));
					const want = [ref.fontFamily, ref.fontSize, ref.fontWeight].join('|');
					probe.remove();
					const ts = [...document.querySelectorAll('main .panel .ptitle')].filter((e) => e.checkVisibility());
					// Round 14: the track records' empty panel has a panel title too, so the check cards are counted apart.
					return { n: ts.length, nc: ts.filter((e) => e.closest('.panel.check')).length, no: ts.filter((e) => e.closest('[data-testid="onchain-check"]')).length, nh: ts.filter((e) => e.closest('[data-testid="eth-check"]')).length, ne: ts.filter((e) => e.closest('[data-testid="track-empty"]')).length, nl: ts.filter((e) => e.closest('[data-testid="track-live"]')).length, same: ts.every((e) => { const c = getComputedStyle(e); return [c.fontFamily, c.fontSize, c.fontWeight].join('|') === want && /Instrument Serif/.test(c.fontFamily); }), sans: [...document.querySelectorAll('main .panel.tile > .ti, main .panel.check .ct > b')].filter((e) => !e.classList.contains('ptitle')).length };
				});
				// A rights-fallback build holds the company flows, the series and the history back, so Data
				// has none of its check cards there but the on-chain one (ADR-0043: chain data ships in every
				// build, and every build has exactly that card; round 14: the track records' empty panel keeps its title).
				// Review of round 14: counted apart, not loosened: on Data every panel title is a check card's or the empty
				// panel's one title, so a fallback build still shows no other panel title (it used to require none at all).
				// ADR-0043 and ADR-0044: the Bitcoin and the Ethereum on-chain check cards ship in every build
				// (public chain data), so a fallback build has exactly those two check cards. NL4: the live track
				// record panel (a testnet forecaster) has one title too, counted apart.
				const ok = i === 0 ? T.n === 4 : T.ne <= 1 && T.nl <= 1 && T.n === T.nc + T.ne + T.nl && T.no === 1 && T.nh === 1 && (FB ? T.nc === 2 : T.nc >= 2);
				expect(ok && T.same && T.sans === 0, `${at} ${name}: ${T.n} panel titles (${T.nc} on check cards, ${T.no} the on-chain one, ${T.nh} the Ethereum one, ${T.ne} the empty track panel's, ${T.nl} the live track panel's) in the display face at the panel heading's size`);
			}
			// Round 8, 3: Data's all-blocks table is one panel whose top row (at least 44 px, the panel's
			// width) opens it, in the section it lists: the company flows, or the block chart's section
			// when the company flows are held back.
			if (page === 'data' && demo.live) {
				const D = await p.evaluate(() =>
					[...document.querySelectorAll('main .tablefold')].map((f) => {
						const s = f.querySelector('summary').getBoundingClientRect();
						return { h: s.height, full: Math.abs(s.width - f.clientWidth) < 2, sec: f.closest('section.section')?.querySelector('h2')?.textContent.trim() ?? null, last: !f.nextElementSibling };
					})
				);
				// Owner review: the block chart's own "Data table" is the one table of every block (a second
				// "All blocks as a table" fold opened the same rows), so no all-blocks panel is left.
				expect(D.length === 0, `${at} data: no second all-blocks table (${D.length} found)`);
			}
			// Round 8, 4: Labels. The private set panel: the set's name, count and hash a card in the right
			// half beside the donut on a wide screen, under it on a phone. The 29 vector results fill their
			// card on a wide screen (ten a row, squares of at least 30 px), 22 px squares on a phone.
			if (page === 'labels') {
				const L = await p.evaluate(() => {
					const row = document.querySelector('.setrow');
					const m = row?.querySelector('.setmeta')?.getBoundingClientRect();
					const d = row?.querySelector('.donut')?.getBoundingClientRect();
					const rr = row?.getBoundingClientRect();
					const dots = [...document.querySelectorAll('ul.dots > li')].map((e) => e.getBoundingClientRect());
					return { set: !!row, right: m && d ? m.left > d.right && Math.abs(m.right - rr.right) < 2 && m.left - rr.left >= rr.width / 2 - 20 : false, under: m && d ? m.top >= d.bottom - 1 : false, dots: dots.length, size: dots[0]?.width ?? 0, square: dots.every((r) => Math.abs(r.width - r.height) < 1), perRow: dots.filter((r) => Math.abs(r.top - dots[0].top) < 1).length };
				});
				if (!PUB) expect(L.set && (width === 1280 ? L.right : L.under), `${at} labels: the set's name, count and hash ${width === 1280 ? 'a card in the right half beside the donut' : 'under the donut'}`);
				expect(L.dots === 29 && L.square && (width === 1280 ? L.perRow === 10 && L.size >= 30 : Math.abs(L.size - 22) < 1), `${at} labels: 29 vector results, ${L.perRow} a row of ${Math.round(L.size)} px squares`);
			}
			// Round 9, 1: Explorer's live blocks with motion off (this context asks for reduced motion): one
			// row that scrolls sideways, reachable by keyboard, every block reachable (the still strip
			// clipped all blocks past the sixth at 1,280 px); the first block unfaded, an edge fades only
			// while blocks are hidden past it.
			// 2026-10-29 review: up to 860 px the blocks wrap into a grid of whole cards (two a row on a
			// phone, the first 8 and a button for the rest), so no card is cut at the panel's edge.
			if (page === 'explorer' && demo.live && width <= 860) {
				const G = await p.evaluate(() => {
					const m = document.querySelector('[data-testid="live-strip"]');
					if (!m) return null;
					const r = m.getBoundingClientRect();
					const blks = [...m.querySelectorAll('.blk')].filter((b) => b.checkVisibility()).map((b) => b.getBoundingClientRect());
					return { grid: getComputedStyle(m).display === 'grid', shown: blks.length, inside: blks.every((b) => b.left >= r.left - 1 && b.right <= r.right + 1), perRow: blks.filter((b) => Math.abs(b.top - blks[0].top) < 1).length };
				});
				expect(!!G && G.grid && G.inside && G.perRow === 2 && G.shown === Math.min(8, demo.live.blocks.length), `${at} explorer: the live blocks are a grid of whole cards, ${G?.perRow} a row (${G?.shown} shown)`);
			}
			if (page === 'explorer' && demo.live && width > 860) {
				const st = () =>
					p.evaluate(() => {
						const m = document.querySelector('[data-testid="live-strip"]');
						if (!m) return null;
						const r = m.getBoundingClientRect();
						const blks = [...m.querySelectorAll('.blk')].filter((b) => b.checkVisibility());
						const f = blks[0]?.getBoundingClientRect();
						const l = blks.at(-1)?.getBoundingClientRect();
						const c = getComputedStyle(m);
						return { overflow: c.overflowX, tab: m.tabIndex, role: m.getAttribute('role'), shown: blks.length, scroll: m.scrollWidth - m.clientWidth, left: m.scrollLeft, firstIn: !!f && f.left >= r.left - 1, lastIn: !!l && l.right <= r.right + 1, less: m.classList.contains('less'), more: m.classList.contains('more'), next: !!document.querySelector('.strip .nudge.next'), prev: !!document.querySelector('.strip .nudge.prev') };
					});
				const A = await st();
				const n = demo.live.blocks.length;
				expect(!!A && A.overflow === 'auto' && A.tab === 0 && A.role === 'region' && A.shown === n && A.scroll > 0 && A.firstIn && !A.lastIn && !A.less && A.more && A.next && !A.prev, `${at} explorer: with motion off the ${n} live blocks are one keyboard-scrollable row (${A ? `${A.shown} drawn, overflow ${A.overflow}, ${A.scroll} px to scroll` : 'no strip'}), the first block unfaded, a fade and an arrow at the right`);
				if (A?.tab === 0) {
					await p.locator('[data-testid="live-strip"]').focus();
					// A focused row: an arrow steps one block, End goes to the last.
					await p.keyboard.press('ArrowRight');
					await p.waitForFunction(() => document.querySelector('[data-testid="live-strip"]').scrollLeft > 100, null, { timeout: WAIT_CAP, polling: 100 });
					await p.keyboard.press('End');
					await p.waitForFunction(() => { const m = document.querySelector('[data-testid="live-strip"]'); return m.scrollLeft + m.clientWidth >= m.scrollWidth - 1 && m.classList.contains('less') && !m.classList.contains('more'); }, null, { timeout: WAIT_CAP, polling: 100 });
				}
				const B = A?.tab === 0 ? await st() : null;
				expect(!!B && B.lastIn && B.less && !B.more && B.prev && !B.next, `${at} explorer: an arrow key steps the live blocks a block on and End scrolls them to the last one, fully in view, the fade and arrow moved to the left`);
			}
			// Round 9, 2: Home's proof levels title in the display face (the last bold sans titles there), the
			// examples a bordered tag.
			if (i === 0) {
				const R = await p.evaluate(() =>
					[...document.querySelectorAll('ol.ladder > li')].map((li) => {
						const t = getComputedStyle(li.querySelector('.lt > b'));
						const g = li.querySelector('.leg > *');
						return { serif: /Instrument Serif/.test(t.fontFamily) && t.fontWeight === '400' && parseFloat(t.fontSize) >= 22, tag: !!g && getComputedStyle(g).borderTopStyle === 'solid' };
					})
				);
				expect(R.length === 5 && R.every((r) => r.serif && r.tag), `${at} home: the ${R.length} proof levels title in the display face, each example a tag`);
				// Round 9, 3: the "What stops cheating" second row splits evenly: the labels and bond tiles the
				// same width and height side by side, the labels title on one line (wide); stacked on a phone.
				const G = await p.evaluate(() => {
					const a = document.querySelector('[data-testid="tile-labels"]')?.getBoundingClientRect();
					const b = document.querySelector('[data-testid="tile-bond"]')?.getBoundingClientRect();
					const t = document.querySelector('[data-testid="tile-labels"] > .ti');
					return a && b ? { side: Math.abs(a.top - b.top) < 1 && b.left > a.right, same: Math.abs(a.width - b.width) < 2 && Math.abs(a.height - b.height) < 2, line: t.getBoundingClientRect().height / parseFloat(getComputedStyle(t).lineHeight), under: b.top >= a.bottom - 1 } : null;
				});
				expect(!!G && (width === 1280 ? G.side && G.same && G.line < 1.5 : G.under), `${at} home: the labels and bond tiles ${width === 1280 ? 'split their row evenly, the labels title on one line' : 'stack'}${G ? '' : ' (tiles not found)'}`);
			}
			// Round 9, 4: Explorer's two one-button steps (rerun, validator check) side by side on a wide screen.
			if (page === 'explorer' && !PUB) {
				const P = await p.evaluate(() => {
					const a = document.querySelector('[data-testid="step-rerun"] > .panel')?.getBoundingClientRect();
					const b = document.querySelector('[data-testid="step-validate"] > .panel')?.getBoundingClientRect();
					return a && b ? { side: Math.abs(a.top - b.top) < 1 && b.left > a.right && Math.abs(a.height - b.height) < 1, under: b.top >= a.bottom - 1 } : null;
				});
				expect(!!P && (width === 1280 ? P.side : P.under), `${at} explorer: the rerun and validator panels ${width === 1280 ? 'side by side, one height' : 'one under the other'}`);
			}
			// Round 9, 5: an inner page's opener: the lede or fact lines in one block beside the title on a wide
			// screen (a card, its foot on the title's foot), under it on a phone; the first section starts
			// higher than the stacked opener left it.
			if (i > 0) {
				const O = await p.evaluate(() => {
					const h = document.querySelector('main h1').getBoundingClientRect();
					const sd = document.querySelector('.slab [data-testid="slabside"]');
					const r = sd?.getBoundingClientRect();
					const slab = document.querySelector('.slab').getBoundingClientRect();
					return r ? { beside: r.left > h.right && Math.abs(r.bottom - h.bottom) < 24 && r.top >= slab.top && getComputedStyle(sd).borderTopStyle === 'solid', under: r.top >= h.bottom - 1, slab: slab.bottom + scrollY } : null;
				});
				expect(!!O && (width === 1280 ? O.beside : O.under), `${at} ${name}: the opener's lede ${width === 1280 ? 'a card beside the title, its foot on the title\'s foot' : 'under the title'}${O ? ` (slab ends at ${Math.round(O.slab)} px)` : ' (no side block)'}`);
			}
			// Round 10, 1: Explorer's paired panels end on their controls: the rerun and validate buttons share
			// a line, the validator's label over its select, the select and its button on one row.
			if (page === 'explorer' && !PUB && width === 1280) {
				const X = await p.evaluate(() => {
					const rb = document.querySelector('[data-testid="step-rerun"] button')?.getBoundingClientRect();
					const vp = document.querySelector('[data-testid="step-validate"] > .panel');
					const vb = vp?.querySelector('button')?.getBoundingClientRect();
					const sel = vp?.querySelector('select')?.getBoundingClientRect();
					const lab = vp?.querySelector('label')?.getBoundingClientRect();
					return rb && vb && sel && lab ? { line: Math.abs(rb.top - vb.top) < 1, row: Math.abs(sel.top + sel.height / 2 - (vb.top + vb.height / 2)) < 2 && vb.left > sel.right, over: lab.bottom <= sel.top + 1 && Math.abs(lab.left - sel.left) < 2 } : null;
				});
				expect(!!X && X.line && X.row && X.over, `${at} explorer: the rerun and validate buttons share a line, the validator's label over its select and the select beside its button${X ? '' : ' (not found)'}`);
				// Review: still on one line once the validator's log shows under its controls.
				await p.locator('[data-testid="step-validate"] > .panel > .row button').click();
				await p.locator('[data-testid="step-validate"] .log').waitFor({ timeout: WAIT_CAP });
				const Y = await p.evaluate(() => {
					const rb = document.querySelector('[data-testid="step-rerun"] button')?.getBoundingClientRect();
					const vb = document.querySelector('[data-testid="step-validate"] > .panel > .row button')?.getBoundingClientRect();
					const lg = document.querySelector('[data-testid="step-validate"] .log')?.getBoundingClientRect();
					const vp = document.querySelector('[data-testid="step-validate"] > .panel')?.getBoundingClientRect();
					return rb && vb && lg && vp ? { line: Math.abs(rb.top - vb.top) < 2, log: lg.top >= vb.bottom && lg.bottom <= vp.bottom } : null;
				});
				expect(!!Y && Y.line && Y.log, `${at} explorer: with the validator's log shown, the two buttons still share a line and the log sits inside its panel${Y ? ` (${Y.line ? 'one line' : 'buttons apart'})` : ' (not found)'}`);
			}
			// Round 10, 2: the API's side card: its fact line on two lines and its perks two a row (wide), so it
			// stands at most 40 px above the kicker and title beside it (it stood 87 px above, 203 px tall);
			// on a phone the perks stay under the fact line.
			if (page === 'api') {
				const A = await p.evaluate(() => {
					const card = document.querySelector('.slab [data-testid="slabside"]').getBoundingClientRect();
					const kick = document.querySelector('[data-testid="kicker"]').getBoundingClientRect();
					const m = document.querySelector('[data-testid="api-status"]');
					const ps = [...document.querySelectorAll('.perks > span')].map((e) => e.getBoundingClientRect());
					return { n: ps.length, rows: new Set(ps.map((r) => Math.round(r.top))).size, two: ps.length === 3 && Math.abs(ps[0].top - ps[1].top) < 2 && ps[2].top > ps[0].bottom, lines: Math.round(m.getBoundingClientRect().height / parseFloat(getComputedStyle(m).lineHeight)), card: card.height, title: card.bottom - kick.top };
				});
				if (width === 1280) expect(A.two && A.lines === 2 && A.card <= A.title + 40, `${at} api: the side card's fact line on ${A.lines} lines and its ${A.n} perks two a row (${A.rows} rows), the card ${Math.round(A.card)} px tall beside a ${Math.round(A.title)} px title block`);
				else expect(A.n === 3 && A.rows >= 2, `${at} api: the ${A.n} perks under the fact line (${A.rows} rows)`);
			}
			// Round 10, 3: on a phone each proof level is a small card: number, icon and name on its top row,
			// then the "how", the example tag and the status across the card's full width.
			if (i === 0) {
				const R = await p.evaluate(() =>
					[...document.querySelectorAll('ol.ladder > li')].map((li) => {
						const r = li.getBoundingClientRect();
						const pl = parseFloat(getComputedStyle(li).paddingLeft);
						const t = li.querySelector('.lt > b').getBoundingClientRect();
						const lv = li.querySelector('.lv').getBoundingClientRect();
						const parts = [li.querySelector('.lt > .muted'), li.querySelector('.leg'), li.querySelector('.lstat')].map((e) => e.getBoundingClientRect());
						return { top: Math.abs(t.top + t.height / 2 - (lv.top + lv.height / 2)) < 6, full: parts.every((q) => Math.abs(q.left - (r.left + pl)) < 2 && q.top >= t.bottom - 1), cols: parts[0].left > t.left - 2 && parts[1].left > parts[0].right, h: r.height };
					})
				);
				const tall = Math.round(R.reduce((a, r) => a + r.h, 0));
				if (width === 390) expect(R.length === 5 && R.every((r) => r.top && r.full) && tall <= 880, `${at} home: the 5 proof levels are cards, the name on the number's row and the rest full width (${tall} px of rungs)`);
				else expect(R.length === 5 && R.every((r) => r.cols), `${at} home: the 5 proof levels keep their columns`);
			}
			// Round 10, 4: Run's heading at the left, its badges and the run picker in a column beside it.
			if (page === 'run') {
				const H = await p.evaluate(() => {
					const sec = document.querySelector('[data-testid="testnet-run"]');
					const h = sec.querySelector('.sh h2').getBoundingClientRect();
					const b = sec.querySelector('.badges').getBoundingClientRect();
					const k = sec.querySelector('[data-testid="net-picker"]')?.getBoundingClientRect();
					return k ? { beside: b.left > h.right && k.left > h.right && Math.abs(b.left - k.left) < 2 && b.bottom <= k.top + 1 && k.top < h.bottom, under: k.top >= h.bottom - 1, stats: sec.querySelector('.stats')?.getBoundingClientRect().top + scrollY } : null;
				});
				expect(!!H && (width === 1280 ? H.beside : H.under), `${at} run: the badges and the run picker ${width === 1280 ? 'in a column beside the heading' : 'under the heading'}${H ? ` (tiles from ${Math.round(H.stats)} px)` : ' (no picker)'}`);
			}
			// Round 10, 5: Data's block section (its first chart section; PP4 put the dataset list above
			// it): the heading and its status at the left, the four figure tiles beside them on one row
			// (wide); under them on a phone.
			if (page === 'data' && demo.live) {
				const D = await p.evaluate(() => {
					const sec = document.querySelector('[data-testid="blocks"]');
					const h = sec.querySelector('.sh h2').getBoundingClientRect();
					const pill = sec.querySelector('.sh .pill')?.getBoundingClientRect();
					const ts = [...sec.querySelectorAll('.stats > .stat')].map((e) => e.getBoundingClientRect());
					const chart = sec.querySelector('.chartbox')?.getBoundingClientRect();
					// Round 12: the heading runs across the section and the tiles sit beside its status under it (they
					// sat beside a two-line heading); no row of its own for the tiles: the chart follows them one
					// section gap down (it was pinned at under 600 px; measured against the tiles, not a fixed px).
					const gap = parseFloat(getComputedStyle(sec).rowGap);
					return { n: ts.length, beside: ts.every((t) => !!pill && t.left > pill.right && Math.abs(t.top - ts[0].top) < 1), chartGap: chart ? chart.top - ts[0].bottom - gap : 99, under: ts.every((t) => t.top >= h.bottom), chart: chart ? chart.top + scrollY : 0 };
				});
				if (width === 1280) expect(D.n === 4 && D.beside && Math.abs(D.chartGap) <= 1, `${at} data: the 4 figure tiles beside the status on one row, the block chart one section gap under them (from ${Math.round(D.chart)} px)`);
				else expect(D.n === 4 && D.under, `${at} data: the 4 figure tiles under the heading`);
			}
			// Round 10, 6: Labels' attestation: the two buttons under the seals in the right half, facing the
			// signed text (wide); under the text on a phone.
			if (page === 'labels') {
				const L = await p.evaluate(() => {
					const pn = document.querySelector('.attpanel');
					const st = pn?.querySelector('.stmt')?.getBoundingClientRect();
					const se = pn?.querySelector('.seals')?.getBoundingClientRect();
					const bs = [...(pn?.querySelectorAll(':scope > .row button') ?? [])].map((e) => e.getBoundingClientRect());
					return st && se && bs.length === 2 ? { right: bs.every((b) => b.left > st.right && b.top >= se.bottom - 1 && b.bottom <= st.bottom + 1) && Math.abs(bs[0].top - bs[1].top) < 1, under: bs.every((b) => b.top >= st.bottom) } : null;
				});
				expect(!!L && (width === 1280 ? L.right : L.under), `${at} labels: the attestation's buttons ${width === 1280 ? 'on one row under the seals, beside the signed text' : 'under the signed text'}${L ? '' : ' (not found)'}`);
			}
			// Round 10, 7: one nesting rule: every card inside a panel takes the inner radius (12 px) and no
			// shadow of its own; every card on the slab takes the top-level radius (18 px).
			{
				const N = await p.evaluate(() => {
					const card = (e) => {
						const s = getComputedStyle(e);
						const r = e.getBoundingClientRect();
						return r.width >= 60 && r.height >= 36 && ['top', 'right', 'bottom', 'left'].every((x) => parseFloat(s.getPropertyValue(`border-${x}-width`)) > 0 && s.getPropertyValue(`border-${x}-style`) !== 'none') && parseFloat(s.borderTopLeftRadius) > 0 && s.borderTopLeftRadius !== '999px' && s.borderTopLeftRadius !== '50%' && !['BUTTON', 'SELECT', 'INPUT', 'A', 'SUMMARY'].includes(e.tagName);
					};
					const bad = [];
					for (const e of document.querySelectorAll('main .panel *')) {
						if (!e.checkVisibility() || !card(e)) continue;
						const s = getComputedStyle(e);
						if (s.borderTopLeftRadius !== '12px' || s.boxShadow !== 'none') bad.push(`${e.className.split(' ')[0]} ${s.borderTopLeftRadius}${s.boxShadow !== 'none' ? ' shadow' : ''}`);
					}
					for (const e of document.querySelectorAll('.slab *')) if (e.checkVisibility() && card(e) && !e.closest('.viz *') && getComputedStyle(e).borderTopLeftRadius !== '18px') bad.push(`slab ${e.className.split(' ')[0]} ${getComputedStyle(e).borderTopLeftRadius}`);
					return bad;
				});
				expect(N.length === 0, `${at} ${name}: every card in a panel has 12 px corners and no shadow, every card on the slab 18 px${N.length ? ': ' + [...new Set(N)].slice(0, 5).join('; ') : ''}`);
			}
			// Round 11, 1: Run's run switch spans its column to the section's right edge (it stopped 47 px
			// short), each run on one line; its options lighten in place on hover (no lift, no glow).
			if (page === 'run' && width === 1280) {
				const K = await p.evaluate(() => {
					const sec = document.querySelector('[data-testid="testnet-run"]').getBoundingClientRect();
					const ch = document.querySelector('[data-testid="net-picker"] .chips')?.getBoundingClientRect();
					const bs = [...document.querySelectorAll('[data-testid="net-pick"]')];
					return ch ? { gap: sec.right - ch.right, one: bs.every((b) => b.getBoundingClientRect().height < 2 * parseFloat(getComputedStyle(b).lineHeight)) } : null;
				});
				expect(!!K && Math.abs(K.gap) <= 1 && K.one, `${at} run: the run switch ends on the section's right edge (${K ? Math.round(K.gap) : '?'} px short), each run on one line`);
				const off = p.locator('[data-testid="net-pick"][aria-pressed="false"]').first();
				const look = () => off.evaluate((e) => { const s = getComputedStyle(e); return { t: s.transform, sh: s.boxShadow, bg: s.backgroundColor, c: s.color }; });
				const a = await look();
				await off.hover();
				const b = await look();
				await p.mouse.move(0, 0);
				expect(b.t === 'none' && b.sh === 'none' && b.bg !== a.bg && b.c !== a.c, `${at} run: a run option lightens on hover without lifting or glowing (${b.t}, ${b.sh === 'none' ? 'no shadow' : 'shadow'}, ${a.bg} -> ${b.bg})`);
			}
			// Round 11, 2: Run's step panels take the panel's 24 px (16 px sides on a phone); the first step's
			// ring and the first gantt row start 24 px inside the panel.
			if (page === 'run') {
				// Review: measured against a plain panel on the same page (not a pinned 24 px), so a change of
				// the panel padding token moves both and the check holds.
				const R = await p.evaluate(() => {
					// A bare .panel in the same section: the default panel padding (some panels are compact).
					const host = document.querySelector('.netbox, .ganttbox')?.parentElement;
					const ref = host ? host.appendChild(document.createElement('div')) : null;
					ref?.classList.add('panel');
					const rs = ref ? getComputedStyle(ref) : null;
					const want = rs ? { v: parseFloat(rs.paddingTop), h: parseFloat(rs.paddingLeft) } : null;
					ref?.remove();
					const boxes = [...document.querySelectorAll('.netbox, .ganttbox')].filter((e) => e.checkVisibility()).map((e) => {
						const s = getComputedStyle(e);
						const top = e.getBoundingClientRect().top + parseFloat(s.borderTopWidth);
						const first = e.querySelector('.netsteps > li:first-child .n, .gantt > li:first-child');
						return { t: parseFloat(s.paddingTop), b: parseFloat(s.paddingBottom), l: parseFloat(s.paddingLeft), r: parseFloat(s.paddingRight), off: first ? first.getBoundingClientRect().top - top : -1 };
					});
					return { want, boxes };
				});
				const side = width === 1280 ? R.want?.h : 16;
				expect(!!R.want && R.boxes.length >= 3 && R.boxes.every((r) => r.t === R.want.v && r.b === R.want.v && r.l === side && r.r === side && Math.abs(r.off - R.want.v) <= 1), `${at} run: all ${R.boxes.length} step panels take a plain panel's padding (${R.want?.v} px${width === 1280 ? '' : ', 16 px sides on a phone'}) and start their first row that far down (${R.boxes.map((r) => `${r.t}/${r.l}/${Math.round(r.off)}`).join(', ')})`);
			}
			// Round 11, 3: Reserves' company names and Home's proof-card titles in the display face, like every
			// panel title (they were bold sans).
			if (page === 'reserves' || i === 0) {
				const T = await p.evaluate((sel) => [...document.querySelectorAll(sel)].filter((e) => e.checkVisibility()).map((e) => { const s = getComputedStyle(e); return { serif: /Instrument Serif/.test(s.fontFamily), w: s.fontWeight, size: parseFloat(s.fontSize) }; }), page === 'reserves' ? '[data-testid="reserve-cards"] h3' : '.proof h3');
				const want = page === 'reserves' ? (FB ? 0 : 4) : 3;
				expect(T.length === want && T.every((t) => t.serif && t.w === '400' && t.size >= 22), `${at} ${name}: ${T.length} ${page === 'reserves' ? 'company card' : 'proof card'} titles in the display face at 400, at least 22 px`);
			}
			// Round 11, 4: Explorer's row proof: the select on a row of its own, both checks side by side on the
			// next one (wide); on a phone the checks stack at the select's width.
			if (page === 'explorer' && !FB) {
				const E = await p.evaluate(() => {
					const c = document.querySelector('.ctl');
					const sel = c?.querySelector('select')?.getBoundingClientRect();
					const bs = [...(c?.querySelectorAll(':scope > button') ?? [])].map((b) => b.getBoundingClientRect());
					return sel && bs.length === 2 ? { under: bs.every((b) => b.top >= sel.bottom), row: Math.abs(bs[0].top - bs[1].top) < 1, stack: bs[1].top >= bs[0].bottom && bs.every((b) => Math.abs(b.width - sel.width) < 1) } : null;
				});
				expect(!!E && E.under && (width === 1280 ? E.row : E.stack), `${at} explorer: the row proof's select on its own row, both checks ${width === 1280 ? 'side by side' : 'stacked at its width'} under it`);
			}
			// Round 11, 5: the Reserves comparison's two figure columns align right (decimals line up).
			if (page === 'reserves' && !FB && width === 1280) {
				const F = await p.evaluate(() => {
					const t = document.querySelector('[data-testid="reserve-live"] table');
					return [...t.querySelectorAll('tbody tr')].map((r) => [1, 2].map((c) => { const e = r.children[c]; const g = document.createRange(); g.selectNodeContents(e); return { al: getComputedStyle(e).textAlign, end: g.getBoundingClientRect().right }; }));
				});
				const ends = [0, 1].map((c) => new Set(F.map((r) => Math.round(r[c].end))).size);
				expect(F.length === 3 && F.every((r) => r.every((x) => x.al === 'right')) && ends.every((n) => n === 1), `${at} reserves: the "Theirs" and "Onyx" figures align right, each column's figures ending on one edge (${ends.join(', ')} edges)`);
			}
			// Round 11, 6: micro-details on every page: figures in switches, buttons, chips, pills, fact and
			// key/value lists, legends and figure tiles are tabular; a table in a panel draws no rule under its
			// last row (the panel's edge or the footer's hairline closes it).
			{
				const M = await p.evaluate(() => ({
					prop: [...document.querySelectorAll('main :is(button, .btn, .chip, .pill, .kv, .facts, .chart-legend, .stat, .log, label)')].filter((e) => e.checkVisibility() && !/tabular-nums/.test(getComputedStyle(e).fontVariantNumeric)).map((e) => `${e.tagName.toLowerCase()}.${[...e.classList][0] ?? ''}`),
					// Review: the row too (a phone's stacked table draws the rule on the row, not its cells).
					rule: [...document.querySelectorAll('main .panel tbody tr:last-child, main .panel tbody tr:last-child > *')].filter((e) => e.checkVisibility() && parseFloat(getComputedStyle(e).borderBottomWidth) > 0).length
				}));
				expect(M.prop.length === 0, `${at} ${name}: every switch, button, chip, pill, fact list, legend and figure tile uses tabular figures${M.prop.length ? ': ' + [...new Set(M.prop)].slice(0, 5).join(', ') : ''}`);
				expect(M.rule === 0, `${at} ${name}: no table in a panel draws a rule under its last row (${M.rule} rows or cells do)`);
			}
			// Round 12, 1: a title that names its item (Home's five roles, Run's steps and indexer rounds, Run's
			// three earning cards) is set in the display face at 400, like the page's headings (they were bold
			// sans); a label inside a chart (the rehearsal's gantt rows) stays in the body face.
			if (i === 0 || page === 'run') {
				const T = await p.evaluate((run) => {
					// The reference: the heading of the section the title sits in.
					const head = (e) => e.closest('section')?.querySelector('h2');
					const face = (e) => getComputedStyle(e).fontFamily;
					const sel = run ? '.netsteps .what > b, .netsteps .body > b, .tiles .tile > b' : '.pipe .node > b';
					const ts = [...document.querySelectorAll(sel)].filter((e) => e.checkVisibility()).map((e) => {
						const s = getComputedStyle(e);
						// Its note or check line: the next sibling's text size.
						const nx = e.nextElementSibling ?? e.parentElement.nextElementSibling;
						return { face: s.fontFamily, ref: head(e) ? face(head(e)) : '', w: s.fontWeight, size: parseFloat(s.fontSize), note: nx ? parseFloat(getComputedStyle(nx).fontSize) : 0 };
					});
					const gantt = [...document.querySelectorAll('.gantt .lab > b')].filter((e) => e.checkVisibility()).map((e) => face(e) !== face(head(e)));
					return { ts, gantt };
				}, page === 'run');
				const want = page === 'run' ? 19 : 5;
				expect(T.ts.length >= want && T.ts.every((t) => !!t.ref && t.face === t.ref && t.w === '400' && t.size >= t.note * 1.3) && T.gantt.every(Boolean), `${at} ${name}: ${T.ts.length} item titles in the headings' face at 400, larger than their notes${page === 'run' ? `; ${T.gantt.length} gantt labels stay in the body face` : ''}`);
			}
			// Round 12, 2: Run's round facts each read on one line (the "Client checks" value wrapped to two lines
			// beside three one-line facts); the cards size to their content where they share a row.
			if (page === 'run' && width === 1280) {
				const F = await p.evaluate(() => {
					const g = [...document.querySelectorAll('.facts')].find((f) => /Client checks/.test(f.textContent));
					return g ? [...g.querySelectorAll('.fact > b')].map((b) => b.getBoundingClientRect().height / parseFloat(getComputedStyle(b).lineHeight)) : null;
				});
				expect(!!F && F.length === 4 && F.every((l) => l < 1.5), `${at} run: the round's ${F?.length ?? 0} facts each on one line (${F ? F.map((l) => Math.round(l)).join('/') : 'not found'})`);
			}
			// Round 12, 3: Labels' test vectors end on their control: the run button sits right above the footer
			// (one panel gap), the results right under the title; beside the taller Brier panel the spare height
			// sat under the button (a 62 px gap above the footer).
			if (page === 'labels' && width === 1280) {
				const V = await p.evaluate(() => {
					const pn = document.querySelector('.dots')?.closest('.panel');
					if (!pn) return null;
					const g = parseFloat(getComputedStyle(pn).rowGap);
					const r = (s) => pn.querySelector(s).getBoundingClientRect();
					return { g, foot: r(':scope > .pfoot').top - r(':scope > .row').bottom, head: r('.dots').top - r(':scope > .sh').bottom };
				});
				expect(!!V && Math.abs(V.foot - V.g) <= 1 && Math.abs(V.head - V.g) <= 1, `${at} labels: the vectors' button one panel gap (${V?.g} px) above the footer (${V ? Math.round(V.foot) : '?'} px), the results one gap under the title (${V ? Math.round(V.head) : '?'} px)`);
			}
			// Round 12, 4: Data's block section heading (its first chart heading; PP4 put the dataset list
			// above it) on one line across the section (it broke after "block" in the 248 px left column);
			// its status sits beside the four tiles under it, centred on their row.
			if (page === 'data' && width === 1280 && demo.live) {
				const D = await p.evaluate(() => {
					const sec = document.querySelector('[data-testid="blocks"]');
					const h2 = sec.querySelector('h2');
					const h = h2.getBoundingClientRect();
					const pill = sec.querySelector('.pill').getBoundingClientRect();
					const ts = [...sec.querySelectorAll('.stats > .stat')].map((e) => e.getBoundingClientRect());
					return { lines: h.height / parseFloat(getComputedStyle(h2).lineHeight), under: ts.every((t) => t.top >= h.bottom) && pill.top >= h.bottom, beside: ts.every((t) => t.left > pill.right), mid: Math.abs((pill.top + pill.bottom) / 2 - (ts[0].top + ts[0].bottom) / 2) };
				});
				expect(D.lines < 1.5 && D.under && D.beside && D.mid <= 2, `${at} data: the first heading on one line (${D.lines.toFixed(1)} lines), its status beside the tiles under it, centred on their row (${D.mid.toFixed(1)} px off)`);
			}
			// Round 13, 1: Data's "Example" heading (the track records' sub-heading) in the face of the section's
			// heading at 400 and the size of the page's panel titles (it was bold 15 px sans, the last such heading).
			if (page === 'data') {
				const X = await p.evaluate(() => {
					const h = document.querySelector('[data-testid="track"] h3');
					if (!h) return null;
					// The reference: a bare panel title (a fallback build's Data page has no other .ptitle on it).
					const ref = document.createElement('span');
					ref.className = 'ptitle';
					document.querySelector('main').append(ref);
					const rs = parseFloat(getComputedStyle(ref).fontSize);
					ref.remove();
					const s = getComputedStyle(h);
					return { face: s.fontFamily === getComputedStyle(h.closest('section').querySelector('h2')).fontFamily, w: s.fontWeight, size: parseFloat(s.fontSize), ref: rs, tag: getComputedStyle(h.querySelector('.ex-tag')).fontFamily === getComputedStyle(document.body).fontFamily };
				});
				expect(!!X && X.face && X.w === '400' && Math.abs(X.size - X.ref) < 0.5 && X.tag, `${at} data: the track records' "Example" heading in the section heading's face at 400, a panel title's size (${X?.size} / ${X?.ref} px), its tag in the body face`);
			}
			// Round 13, 3: Run's gantt rows: a step's name one step above its detail (both were 13 px), still in
			// the body face (a chart's labels, round 12).
			if (page === 'run') {
				const G = await p.evaluate(() =>
					[...document.querySelectorAll('.gantt .lab')].filter((e) => e.checkVisibility()).map((l) => {
						const b = getComputedStyle(l.querySelector(':scope > b'));
						const d = l.querySelector(':scope > span');
						return { up: !d || parseFloat(b.fontSize) >= parseFloat(getComputedStyle(d).fontSize) * 1.1, body: b.fontFamily === getComputedStyle(document.body).fontFamily };
					})
				);
				expect(G.length > 0 && G.every((g) => g.up && g.body), `${at} run: ${G.length} gantt step names a size above their detail, in the body face`);
			}
			// Round 14, 1: Home's check names are item titles (round 12's rule): the face of their section's
			// heading at 400, well above their fold's size (they were medium 15 px sans beside the pipeline's
			// display-face roles).
			if (i === 0) {
				const C = await p.evaluate(() => {
					const h2 = document.querySelector('[data-testid="checks-heading"]');
					const face = getComputedStyle(h2).fontFamily;
					return [...document.querySelectorAll('.checks .ct > span:first-child')].filter((e) => e.checkVisibility()).map((e) => {
						const s = getComputedStyle(e);
						const fold = e.closest('li').querySelector('.why summary');
						return { face: s.fontFamily === face, w: s.fontWeight, up: parseFloat(s.fontSize) >= parseFloat(getComputedStyle(fold).fontSize) * 1.3 };
					});
				});
				expect(C.length >= 5 && C.every((c) => c.face && c.w === '400' && c.up), `${at} home: ${C.length} check names in their heading's face at 400, larger than their folds`);
			}
			// Round 14, 4: Data's track records: each record's headline is the card's title (the section heading's
			// face at 400, the size of an item title; it was a mono 18 px sentence), and the empty panel's "No real
			// track records yet." its panel's title (it was bold 15 px sans). The source id stays in mono.
			if (page === 'data') {
				// Data review: the track records section is folded by default.
				await p.getByTestId('track-fold').locator('> summary').click().catch(() => {});
				await p.locator('[data-testid^="track-headline-"]').first().waitFor({ timeout: 120000 }).catch(() => {});
				const R = await p.evaluate(() => {
					const sec = document.querySelector('[data-testid="track"]');
					if (!sec) return null;
					const face = getComputedStyle(sec.querySelector('h2')).fontFamily;
					// The references: a bare item title and a bare panel title.
					const size = (c) => {
						const e = document.createElement('span');
						e.className = c;
						document.querySelector('main').append(e);
						const v = parseFloat(getComputedStyle(e).fontSize);
						e.remove();
						return v;
					};
					const it = size('ititle');
					const pt = size('ptitle');
					const ok = (e, ref) => {
						const s = getComputedStyle(e);
						return s.fontFamily === face && s.fontWeight === '400' && Math.abs(parseFloat(s.fontSize) - ref) < 0.5;
					};
					const heads = [...sec.querySelectorAll('[data-testid^="track-headline-"]')];
					const empty = sec.querySelector('[data-testid="track-empty"] > p:first-child > b');
					const ids = [...sec.querySelectorAll('[data-testid^="track-ex-"] .label > .mono')];
					return { n: heads.length, heads: heads.every((h) => ok(h, it)), empty: !!empty && ok(empty, pt), ids: ids.length === heads.length && ids.every((e) => /Plex Mono/.test(getComputedStyle(e).fontFamily)) };
				});
				expect(!!R && R.n >= 2 && R.heads && R.empty && R.ids, `${at} data: ${R?.n ?? 0} track-record headlines item titles in the section heading's face, the empty panel's title a panel title, the source ids in mono`);
			}
			// Review of round 11: the shared switch style stays on switches. Data's pool-share bars are drawn
			// to scale: each segment's width is its share of the bar (less the 2 px gaps), not padded or
			// bordered (a bare .seg rule once gave each segment 10 px of padding and border).
			if (page === 'data') {
				const P = await p.evaluate(() =>
					[...document.querySelectorAll('main .chart .bar')].filter((b) => b.checkVisibility()).map((b) => {
						const ss = [...b.querySelectorAll('.seg')];
						const gap = parseFloat(getComputedStyle(b).columnGap) || 0;
						const free = b.clientWidth - gap * (ss.length - 1);
						const total = ss.reduce((a, x) => a + (parseFloat(x.style.flexGrow) || 0), 0);
						return Math.max(...ss.map((x) => Math.abs(x.getBoundingClientRect().width - Math.max(2, (free * (parseFloat(x.style.flexGrow) || 0)) / total))));
					})
				);
				expect(P.length > 0 && P.every((d) => d <= 2.5), `${at} data: ${P.length} pool-share bars drawn to scale (worst segment off by ${P.length ? Math.max(...P).toFixed(1) : '?'} px)`);
			}
			await p.close();
		}
		// Figure tiles: one look on Home, Data and Run (padding, corners, shadow, sizes), the figure
		// in mono well above its label's size and on one line.
		const looks = new Set(tiles.map((t) => [t.pad, t.radius, t.shadow, t.fig, t.label].join('|')));
		const pages = [...new Set(tiles.map((t) => t.page))].sort();
		expect(
			JSON.stringify(pages) === JSON.stringify(['data', 'home', 'run']) && looks.size === 1 && tiles.every((t) => /Plex Mono/.test(t.figFont) && parseFloat(t.fig) >= parseFloat(t.label) * 1.35 && t.oneLine),
			`${at}: all ${tiles.length} figure tiles on ${pages.join(', ')} share one look, figure in mono ≥ 1.35 × its label, on one line${looks.size > 1 ? ': ' + [...looks].join(' / ') : ''}${tiles.filter((t) => !t.oneLine).map((t) => ' wraps on ' + t.page).join('')}`
		);
		await ctx.close();
	}
	// Round 10, 8: one motion scale (hover and focus --dur, status changes --dur-slow; motion on) and one
	// opener height: the not-found page's slab is the inner pages' slab (it alone used 96 / 80 px).
	{
		const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, reducedMotion: 'no-preference' });
		const durs = new Set();
		const pads = [];
		for (const page of ['', 'explorer', 'labels', 'does-not-exist']) {
			const p = await ctx.newPage();
			await p.goto(base + (page ? `#/${page}` : '#/how-it-works'));
			await p.locator('main').waitFor();
			await p.locator('main h1').waitFor({ timeout: WAIT_CAP });
			const r = await p.evaluate(() => ({
				d: [...document.querySelectorAll('body *')].flatMap((e) => getComputedStyle(e).transitionDuration.split(', ')).filter((x) => x !== '0s'),
				pad: getComputedStyle(document.querySelector('.slab .inner')).paddingBlock,
				sp: ['--sp-11', '--sp-12'].map((n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim()).join('')
			}));
			r.d.forEach((x) => durs.add(x));
			pads.push([page || 'home', r.pad, r.sp]);
			await p.close();
		}
		expect([...durs].every((x) => x === '0.16s' || x === '0.32s'), `W12 1280 px: every transition takes 160 ms or 320 ms (${[...durs].join(', ')})`);
		expect(new Set(pads.map((x) => x[1])).size === 1 && pads.every((x) => x[2] === ''), `W12 1280 px: one slab padding on every page, the 404 page's too, and no --sp-11 or --sp-12 (${pads.map((x) => x.join(' ')).join('; ')})`);
		await ctx.close();
	}
	// Review of round 11: Explorer's two checks share a line only where both fit; in the narrow right
	// column at 800 px they stack at the select's width (they wrapped to two rows of different widths).
	if (!FB) {
		for (const width of [800, 1000]) {
			const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
			const p = await ctx.newPage();
			await p.goto(base + '#/explorer');
			const has = await p.locator('.ctl').first().waitFor({ timeout: WAIT_CAP }).then(() => true, () => false);
			if (has) {
				await settled(p);
				const E = await p.evaluate(() => {
					const c = document.querySelector('.ctl');
					const sel = c.querySelector('select').getBoundingClientRect();
					const bs = [...c.querySelectorAll(':scope > button')].map((b) => b.getBoundingClientRect());
					return { row: bs.length === 2 && Math.abs(bs[0].top - bs[1].top) < 1, stack: bs.length === 2 && bs[1].top >= bs[0].bottom && bs.every((b) => Math.abs(b.width - sel.width) < 1) };
				});
				expect(width === 800 ? E.stack : E.row, `W12 ${width} px explorer: the row proof's checks ${width === 800 ? 'stack at the select\'s width in the narrow column' : 'share a line'}`);
			}
			await ctx.close();
		}
	}
	// Round 12, 2 and 3 at 1,000, 1,080 and 761 px: Run's round facts each on one line (at 761 px the fourth
	// takes its own row); at 1,000 px (two panels side by side) the vectors' button sits right above the footer.
	for (const width of [1000, 1080, 761]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const p = await ctx.newPage();
		await p.goto(base + '#/run');
		await settled(p);
		const F = await p.evaluate(() => {
			const g = [...document.querySelectorAll('.facts')].find((f) => /Client checks/.test(f.textContent));
			return g ? [...g.querySelectorAll('.fact > b')].map((b) => b.getBoundingClientRect().height / parseFloat(getComputedStyle(b).lineHeight)) : null;
		});
		expect(!!F && F.length === 4 && F.every((l) => l < 1.5), `W12 ${width} px run: the round's ${F?.length ?? 0} facts each on one line (${F ? F.map((l) => Math.round(l)).join('/') : 'not found'})`);
		if (width === 1000) {
			await p.goto(base + '#/labels');
			// Review of round 14: the Brier scores arrive from the verifier once the browser is idle (W4, the round 13
			// gate's wait); measured before them the Brier panel is short (236 px for 404 at load 42) and the vectors'
			// panel beside it is laid out against that.
			await p.locator('.score').waitFor({ timeout: 150000 });
			await settled(p);
			const V = await p.evaluate(() => {
				const pn = document.querySelector('.dots').closest('.panel');
				const g = parseFloat(getComputedStyle(pn).rowGap);
				const row = pn.querySelector(':scope > .row').getBoundingClientRect();
				return { g, foot: pn.querySelector(':scope > .pfoot').getBoundingClientRect().top - row.bottom, spare: row.top - pn.querySelector('.dots > li:last-child').getBoundingClientRect().bottom };
			});
			// Here 8 a row fill the panel: the last result ends at most two panel gaps above the button.
			expect(Math.abs(V.foot - V.g) <= 1 && V.spare <= 2 * V.g + 1, `W12 ${width} px labels: the vectors' button one panel gap above the footer (${Math.round(V.foot)} px), the results filling the panel above it (${Math.round(V.spare)} px to the button)`);
		}
		// Review of round 12: the vectors' grid fills its panel without outgrowing the Brier panel beside it
		// (at 1,059-1,099 px 8 a row made the vectors panel up to 50 px taller and left the empty band under
		// the Brier scores). Heights compared with each panel at its own content height.
		if (width !== 761) {
			if (width !== 1000) await p.goto(base + '#/labels');
			// The Brier panel measured with its scores (review of round 14), at every width.
			await p.locator('.score').waitFor({ timeout: 150000 });
			await settled(p);
			const H = await p.evaluate(() => {
				const pn = document.querySelector('.dots').closest('.panel');
				const br = [...pn.parentElement.querySelectorAll(':scope > .panel')].find((e) => e !== pn);
				const g = parseFloat(getComputedStyle(pn).rowGap);
				pn.style.alignSelf = br.style.alignSelf = 'start';
				pn.querySelector('.dots').style.flexGrow = '0';
				const r = { g, vec: pn.getBoundingClientRect().height, brier: br.getBoundingClientRect().height };
				pn.style.alignSelf = br.style.alignSelf = pn.querySelector('.dots').style.flexGrow = '';
				return r;
			});
			expect(H.vec - H.brier <= 1.5 * H.g, `W12 ${width} px labels: the vectors panel (${Math.round(H.vec)} px of content) no more than one and a half gaps taller than the Brier panel's (${Math.round(H.brier)} px)`);
		}
		await ctx.close();
	}
	// Review of round 12: an item title in the display face breaks a word longer than its column (a run
	// step's title comes from the run file) instead of running out of its card and scrolling a phone page
	// sideways. One title of each kind gets a 50-letter word.
	{
		const ctx = await browser.newContext({ viewport: { width: 375, height: 844 }, reducedMotion: 'reduce', isMobile: true, hasTouch: true });
		for (const page of ['', 'run']) {
			const p = await ctx.newPage();
			await p.goto(base + (page ? `#/${page}` : '#/how-it-works'));
			await settled(p);
			const W = await p.evaluate(() => {
				const kinds = new Set();
				const out = [];
				for (const e of document.querySelectorAll('.ititle')) {
					if (!e.checkVisibility() || kinds.has(e.parentElement.className)) continue;
					kinds.add(e.parentElement.className);
					e.textContent = 'Recountverificationandsettlementcheckpointsreached';
					const box = (e.closest('li, .tile') ?? e.parentElement).getBoundingClientRect();
					out.push(e.getBoundingClientRect().right <= box.right + 1 && e.scrollWidth <= e.clientWidth + 1);
				}
				return { n: out.length, fit: out.every(Boolean), hs: document.documentElement.scrollWidth - document.documentElement.clientWidth };
			});
			expect(W.n >= (page ? 3 : 1) && W.fit && W.hs <= 0, `W12 375 px ${page || 'home'}: a 50-letter word in ${W.n} kinds of item title breaks inside its card (page scrolls ${W.hs} px sideways)`);
			await p.close();
		}
		await ctx.close();
	}
	// Round 13, 2: Home's pipeline notes take their column's width (capped at 18ch they ran to three lines,
	// "Rerun a sample; cheaters lose their bond" at 1,000-1,600 px): at most two lines each.
	for (const width of [1000, 1280, 1600]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const p = await ctx.newPage();
		await p.goto(base + '#/how-it-works');
		await settled(p);
		const N = await p.evaluate(() =>
			[...document.querySelectorAll('.pipe .note')].map((n) => {
				const r = document.createRange();
				r.selectNodeContents(n);
				return new Set([...r.getClientRects()].filter((q) => q.width > 1).map((q) => Math.round(q.top))).size;
			})
		);
		expect(N.length === 5 && N.every((l) => l <= 2), `W12 ${width} px home: the five pipeline notes at most two lines each (${N.join('/')})`);
		await ctx.close();
	}
	// Round 14, 2: at 761-999 px each role of Home's pipeline takes a width in proportion to its text, so every
	// note fits in two lines (five equal 102-145 px columns gave "Rerun a sample; cheaters lose their bond" three);
	// neighbouring notes stay at least half a beam apart (the beams keep their width).
	for (const width of [761, 861, 999]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const p = await ctx.newPage();
		await p.goto(base + '#/how-it-works');
		await settled(p);
		const N = await p.evaluate(() => {
			const box = [...document.querySelectorAll('.pipe .note')].map((n) => {
				const r = document.createRange();
				r.selectNodeContents(n);
				const q = [...r.getClientRects()].filter((x) => x.width > 1);
				return { lines: new Set(q.map((x) => Math.round(x.top))).size, l: Math.min(...q.map((x) => x.left)), r: Math.max(...q.map((x) => x.right)) };
			});
			const beam = document.querySelector('.pipe .beam').getBoundingClientRect().width;
			return { lines: box.map((b) => b.lines), gap: Math.min(...box.slice(1).map((b, i) => b.l - box[i].r)), beam };
		});
		expect(N.lines.length === 5 && N.lines.every((l) => l <= 2) && N.gap >= N.beam / 2, `W12 ${width} px home: the five pipeline notes at most two lines each (${N.lines.join('/')}), at least half a beam apart (${Math.round(N.gap)} px, beam ${Math.round(N.beam)} px)`);
		await ctx.close();
	}
	// Round 14, 3: from 1,059 px (10 results a row) Labels' Brier panel is a form: each slider's label beside its
	// slider, the scores' names in the same column (bars and slider tracks start on one edge), so the vectors'
	// panel is no longer stretched to the Brier panel's height: its button sits one panel gap under the last
	// result (a 45-49 px band at 1,059-1,600 px), and the Brier scores end one gap above their footer.
	for (const width of [1059, 1280]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const p = await ctx.newPage();
		await p.goto(base + '#/labels');
		// Review of round 14: a wait that times out fails (tests/wait.mjs), never measures a panel without its scores.
		await p.locator('.score').waitFor({ timeout: 150000 });
		await settled(p);
		const B = await p.evaluate(() => {
			const vp = document.querySelector('.dots')?.closest('.panel');
			const bp = document.querySelector('.sliders')?.closest('.panel');
			if (!vp || !bp || !bp.querySelector('.score')) return null;
			const g = parseFloat(getComputedStyle(vp).rowGap);
			const r = (e) => e.getBoundingClientRect();
			const band = r(vp.querySelector(':scope > .row')).top - r(vp.querySelector('.dots > li:last-child')).bottom;
			const tail = r(bp.querySelector(':scope > .pfoot')).top - r(bp.querySelector('.score')).bottom;
			const rows = [...bp.querySelectorAll('.sliders > label')].map((l) => {
				const input = document.getElementById(l.htmlFor);
				const s = r(input);
				const lr = r(l);
				// The slider's box edge (Chrome gives a range input a 2 px margin of its own).
				return { beside: lr.right <= s.left && Math.abs((lr.top + lr.bottom) / 2 - (s.top + s.bottom) / 2) <= 2, left: s.left - parseFloat(getComputedStyle(input).marginLeft) };
			});
			const bars = [...bp.querySelectorAll('.sb .track')].map((e) => r(e).left);
			return { g, band, tail, rows, edge: Math.max(...[...rows.map((x) => x.left), ...bars].map((x) => Math.abs(x - bars[0]))) };
		});
		expect(!!B && Math.abs(B.band - B.g) <= 1 && Math.abs(B.tail - B.g) <= 1, `W12 ${width} px labels: the vectors' button one panel gap under the last result (${B ? Math.round(B.band) : '?'} px), the Brier scores one gap above their footer (${B ? Math.round(B.tail) : '?'} px)`);
		expect(!!B && B.rows.length === 3 && B.rows.every((x) => x.beside) && B.edge <= 1, `W12 ${width} px labels: the Brier panel's ${B?.rows.length ?? 0} labels each beside their slider, sliders and score bars starting on one edge (${B ? B.edge.toFixed(1) : '?'} px apart)`);
		await ctx.close();
	}
	// Round 13, 4: a section below the first screen fades in as it comes into view (motion on); none on the
	// first screen is ever dimmed, and with reduced motion or the pause button every section shows at once.
	for (const width of [1280, 390]) {
		for (const mode of ['on', 'reduce', 'paused']) {
			const ctx = await browser.newContext({ viewport: { width, height: width < 500 ? 844 : 900 }, reducedMotion: mode === 'reduce' ? 'reduce' : 'no-preference', isMobile: width < 500, hasTouch: width < 500 });
			if (mode === 'paused') await ctx.addInitScript(() => localStorage.setItem('onyx-motion', 'off'));
			const p = await ctx.newPage();
			await p.goto(base + '#/run');
			await settled(p);
			const at = `W12 ${width} px run, motion ${mode}`;
			// E12: the fade marks sections from its observer's first callback, which a loaded machine can run
			// after settled()'s quiet window; read once every section below the first screen is marked.
			if (mode === 'on')
				await p
					.waitForFunction(() => [...document.querySelectorAll('main section.section')].filter((s) => s.getBoundingClientRect().top >= innerHeight).every((s) => s.dataset.fade), null, { timeout: WAIT_CAP })
					.catch(() => {});
			const F = await p.evaluate(() =>
				[...document.querySelectorAll('main section.section')].map((s) => ({ on: s.getBoundingClientRect().top < innerHeight, op: parseFloat(getComputedStyle(s).opacity), wait: s.dataset.fade === 'wait' }))
			);
			const first = F.filter((f) => f.on);
			const below = F.filter((f) => !f.on);
			expect(first.length > 0 && first.every((f) => f.op === 1 && !f.wait), `${at}: the ${first.length} sections on the first screen fully shown at load`);
			if (mode === 'on') {
				expect(below.length > 0 && below.every((f) => f.wait && f.op === 0), `${at}: the ${below.length} sections below the first screen wait unshown (${below.filter((f) => f.op === 0).length} at opacity 0)`);
				// Review of round 13: printed, the sections still waiting show (the fade is screen only). Review of
				// round 14: before the scroll below, which now shows every section it passes, so some still wait here;
				// back on screen they fade out again, and the last one's fade in is waited on below.
				await p.emulateMedia({ media: 'print' });
				const printed = await p.evaluate(() => { const S = [...document.querySelectorAll('main section.section')]; return { wait: S.filter((s) => s.dataset.fade === 'wait').length, dim: S.filter((s) => getComputedStyle(s).opacity !== '1').length }; });
				await p.emulateMedia({ media: 'screen' });
				// Back on screen the waiting sections fade out from print's full opacity: let that end, or the wait
				// below would read the last section's opacity 1 before it fades in.
				await p.evaluate(() => Promise.all(document.getAnimations().filter((a) => a instanceof CSSTransition).map((a) => a.finished.catch(() => {}))));
				expect(printed.wait > 0 && printed.dim === 0, `${at}: printed, every section shows, the ${printed.wait} still waiting too (${printed.dim} at opacity under 1)`);
				// The last one scrolls into view and fades in over the --dur-slow token.
				// E12: waited for until faded in or wait.mjs's cap (it was 60 polls of 50 ms, 3 s).
				const shown = await p.evaluate(async (cap) => {
					const s = [...document.querySelectorAll('main section.section')].at(-1);
					s.scrollIntoView();
					for (const t0 = Date.now(); Date.now() - t0 < cap && (s.dataset.fade !== 'in' || getComputedStyle(s).opacity !== '1'); ) await new Promise((r) => setTimeout(r, 50));
					const ms = (v) => Math.round(v.trim().endsWith('ms') ? parseFloat(v) : parseFloat(v) * 1000);
					return { op: getComputedStyle(s).opacity, fade: s.dataset.fade, dur: ms(getComputedStyle(s).transitionDuration), token: ms(getComputedStyle(document.documentElement).getPropertyValue('--dur-slow')) };
				}, WAIT_CAP);
				expect(shown.op === '1' && shown.fade === 'in' && shown.dur > 0 && shown.dur === shown.token, `${at}: the last section fades in (${shown.dur} ms, --dur-slow) once it scrolls into view (opacity ${shown.op})`);
			} else {
				expect(below.length > 0 && below.every((f) => f.op === 1), `${at}: the ${below.length} sections below the first screen shown at once`);
			}
			await ctx.close();
		}
	}
	// Round 14 review: a section the reader scrolls past between two drawn frames is shown, not left at opacity 0.
	// An IntersectionObserver samples only drawn frames, so on a loaded machine (e2e's scroll left one Home section
	// dimmed) a section can go from below the screen to above it unseen. Two scrolls in one task draw no frame
	// between them: the first puts the section in view, the second scrolls past it. Sections still below wait.
	// Home and Run (the same failure was seen on Run at load 56).
	for (const [width, page] of [[1280, ''], [390, ''], [1280, 'run'], [390, 'run']]) {
		const ctx = await browser.newContext({ viewport: { width, height: width < 500 ? 844 : 800 }, reducedMotion: 'no-preference', isMobile: width < 500, hasTouch: width < 500 });
		const p = await ctx.newPage();
		await p.goto(base + (page ? `#/${page}` : '#/how-it-works'));
		await settled(p);
		const at = `W12 ${width} px ${page || 'home'}, motion on`;
		const J = await p.evaluate(async (cap) => {
			const S = [...document.querySelectorAll('main section.section')];
			const abs = (s) => ({ top: s.getBoundingClientRect().top + scrollY, bottom: s.getBoundingClientRect().bottom + scrollY });
			const max = document.documentElement.scrollHeight - innerHeight;
			// The first waiting section with a screen of page and another section below its end.
			const k = S.findIndex((s) => s.dataset.fade === 'wait' && abs(s).bottom + 1 <= max && S.some((x) => abs(x).top >= abs(s).bottom + 1 + innerHeight));
			if (k < 0) return null;
			const s = S[k];
			const a = abs(s);
			scrollTo(0, a.top - innerHeight / 2);
			const seenMid = s.getBoundingClientRect().top < innerHeight && s.getBoundingClientRect().bottom > 0;
			scrollTo(0, a.bottom + 1);
			await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
			// The fade takes 320 ms; waited for until shown or wait.mjs's cap (E12; it was 3 s).
			for (const t0 = Date.now(); Date.now() - t0 < cap && getComputedStyle(s).opacity !== '1'; ) await new Promise((r) => setTimeout(r, 50));
			const below = S.filter((x) => x.getBoundingClientRect().top >= innerHeight);
			return { k, seenMid, past: s.getBoundingClientRect().bottom <= 0, fade: s.dataset.fade, op: getComputedStyle(s).opacity, below: below.length, belowWait: below.filter((x) => x.dataset.fade === 'wait' && getComputedStyle(x).opacity === '0').length };
		}, WAIT_CAP);
		expect(!!J && J.seenMid && J.past && J.fade === 'in' && J.op === '1', `${at}: section ${J ? J.k + 1 : '?'}, scrolled past with no frame drawn while it was in view, is shown (data-fade ${J?.fade ?? '?'}, opacity ${J?.op ?? '?'})`);
		expect(!!J && J.below > 0 && J.belowWait === J.below, `${at}: the ${J?.below ?? 0} sections still below the screen keep waiting (${J?.belowWait ?? 0} at opacity 0)`);
		await ctx.close();
	}
	// Round 11, 7: Labels' attestation buttons: where the two cannot share a line (the right column at
	// 761-1,000 px, a phone) they stack at one width; at 1,280 px they share a row (round 10's check).
	for (const width of [900, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce', isMobile: width < 500, hasTouch: width < 500 });
		const p = await ctx.newPage();
		await p.goto(base + '#/labels');
		await p.locator('.attpanel').waitFor({ timeout: WAIT_CAP });
		await settled(p);
		const B = await p.evaluate(() => {
			const pn = document.querySelector('.attpanel');
			const st = pn.querySelector('.stmt').getBoundingClientRect();
			const bs = [...pn.querySelectorAll(':scope > .row button')].map((e) => e.getBoundingClientRect());
			return { n: bs.length, stack: bs.length === 2 && bs[1].top >= bs[0].bottom && Math.abs(bs[0].width - bs[1].width) < 1 && Math.abs(bs[0].left - bs[1].left) < 1, right: bs.every((b) => b.left > st.right), h: Math.min(...bs.map((b) => b.height)) };
		});
		expect(B.stack && (width === 900 ? B.right : true) && B.h >= (width < 500 ? 44 : 0), `W12 ${width} px labels: the attestation's ${B.n} buttons stack at one width${width === 900 ? ' in the right column' : ', each at least 44 px tall'}`);
		await ctx.close();
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const { chromium } = await import('playwright-core');
	const { resolve } = await import('node:path');
	const { serve } = await import('./serve.mjs');
	const root = resolve(process.argv[2] ?? new URL('../build/', import.meta.url).pathname);
	const server = await serve(0, root);
	const browser = capWaits(await chromium.launch({ executablePath: '/usr/bin/google-chrome', args: ['--headless=new'] }));
	let failed = 0;
	let passed = 0;
	const expect = (ok, what) => {
		if (ok) passed++;
		else failed++;
		console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
	};
	try {
		await w12(browser, `http://127.0.0.1:${server.address().port}/`, root, expect);
	} finally {
		await browser.close();
		server.close();
	}
	console.log(`${passed} ok, ${failed} failed`);
	process.exit(failed ? 1 : 0);
}
