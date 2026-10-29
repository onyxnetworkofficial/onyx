// The full check suite on the Overview page. Every reference value comes from
// the verifier's pinned anchors (compiled Rust), never from the data files the
// page is checking. On-chain anchoring replaces the pins in M6.
import { loadDemo, loadInput, loadPools, loadRecipe, type Demo } from './data.ts';
import { verifier } from './verifier.ts';
import { FALLBACK, PUBLIC } from './mode.ts';
import { group, int } from './format.ts';

export type Check = { id: string; title: string; detail: string; run: (d: Demo) => Promise<string> };

const must = (ok: boolean, msg: string) => {
	if (!ok) throw new Error(msg);
};

// The two checks that need the label rows (the recipe input), for the
// private preview only (AUD-2-F1).
const inputChecks: Check[] = [
	{
		id: 'source',
		title: 'Blocks and labels are the pinned ones',
		detail: 'Keccak-256 of the recipe input (3 blocks plus the label set) equals the hash pinned in the code.',
		run: async (d) => {
			const pin = await verifier.pinned();
			const i = await verifier.inspectInput(await loadInput(d.runs[0].input));
			must(i.input_keccak === pin.input_keccak, `input hash ${i.input_keccak} is not the pinned one`);
			const b = i.blocks;
			return `blocks ${group(b[0].height)}–${group(b[b.length - 1].height)}, ${i.label_set.labels.length} labels`;
		}
	},
	{
		id: 'recipe',
		title: 'Recipe rerun gives the pinned root',
		detail: 'The canonical engine (wasmi, the same fuel rules as a validator) reruns the pinned recipe here.',
		run: async (d) => {
			const pin = await verifier.pinned();
			const t = performance.now();
			const r = await verifier.runRecipe(await loadRecipe(), await loadInput(d.runs[0].input));
			const ms = performance.now() - t;
			must(r.recipe_id === pin.recipe_id, 'not the pinned recipe');
			must(r.outcome === 'Ok', `recipe ended with ${r.outcome}`);
			must(r.root === pin.root, `root ${r.root} is not the pinned root`);
			return `${int(r.fuel)} fuel, ${(ms / 1000).toFixed(1)} s in this browser`;
		}
	},
];

// The public build ships no label rows: the same two slots check what it does
// ship against the pinned values (AUD-2-F1).
const publicChecks: Check[] = [
	{
		id: 'source',
		title: 'Label set is the pinned one',
		detail: 'The label set the runs name has the content hash pinned in the code. Its rows are not shipped here: the test labels’ sharing rights are not cleared.',
		run: async (d) => {
			const pin = await verifier.pinned();
			must(d.label_set.content_hash === pin.label_set, `label set ${d.label_set.content_hash} is not the pinned one`);
			return `${d.label_set.name} v${d.label_set.version}, ${group(d.label_set.count)} labels (rows not shipped)`;
		}
	},
	{
		id: 'recipe',
		title: 'Run rows hash to the pinned root',
		detail: 'The Merkle root over the run’s published rows equals the root the canonical engine gave, pinned in the code.',
		run: async (d) => {
			const pin = await verifier.pinned();
			const root = await verifier.merkleRoot(d.runs[0].rows.map((r) => r.leaf));
			must(root === pin.root, `root ${root} is not the pinned root`);
			return `${d.runs[0].rows.length} rows, root ${root.slice(0, 10)}…`;
		}
	}
];

// A rights-fallback build (ROADMAP L14, ADR-0035) ships no row of a label-derived
// metric, so the three checks over run and live rows give way to one over the
// mining-pool history, whose rights are clear (MIT list, chain data).
const LABEL_ROW_CHECKS = new Set(['recipe', 'liveroot', 'proof']);
const poolCheck: Check = {
	id: 'pools',
	title: 'Pool shares hash to the pinned commitment',
	detail: 'Every day of the mining-pool history rebuilds its root from its leaves, counts each block once, and the commitment over the days is the one pinned in the code.',
	run: async () => {
		const c = await verifier.pools(await loadPools());
		must(c.ok, c.problems[0] ?? 'the pool history does not check out');
		const blocks = Object.values(c.totals).reduce((a, x) => a + x, 0);
		return `${blocks.toLocaleString('en-US')} blocks over ${c.days.length} days, commitment pinned`;
	}
};

const allChecks: Check[] = [
	...(PUBLIC ? publicChecks : inputChecks),
	{
		id: 'headers',
		title: 'Live blocks have real proof of work',
		detail: 'Every header from our node meets the pinned difficulty, links to the one before, and the chain ends at the pinned tip.',
		run: async (d) => {
			const pin = await verifier.pinned();
			must(!!d.live, 'no live range in this export');
			const blocks = d.live!.blocks;
			must(blocks.length === pin.live.to - pin.live.from + 1, `expected ${pin.live.to - pin.live.from + 1} headers, got ${blocks.length}`);
			let prev: string | null = null;
			for (const [k, b] of blocks.entries()) {
				must(b.height === pin.live.from + k, `height ${b.height} out of order`);
				const h = await verifier.checkHeader(b.header);
				must(h.hash === b.hash, `block ${b.height}: hash differs`);
				must(prev === null || h.parent === prev, `block ${b.height}: parent link broken`);
				prev = h.hash;
			}
			must(prev === pin.live.tip, 'chain does not end at the pinned tip');
			return `${blocks.length} headers, ${group(pin.live.from)}–${group(pin.live.to)}`;
		}
	},
	{
		id: 'liveroot',
		title: 'Live rows hash to the pinned root',
		detail: 'The Merkle root over the live rows equals the root pinned in the code. The numbers shown are decoded from these rows.',
		run: async (d) => {
			const pin = await verifier.pinned();
			const root = await verifier.merkleRoot(d.live!.rows.map((r) => r.leaf));
			must(root === pin.live.root, `root ${root} is not the pinned root`);
			return `${d.live!.rows.length} rows, root ${root.slice(0, 10)}…`;
		}
	},
	{
		id: 'attest',
		title: 'Attestation checker accepts a valid one',
		detail: 'BIP-322 by the address and EIP-191 by the company key, for a test company. Shows the checker works; test keys prove nothing about real data.',
		run: async (d) => {
			const a = d.attestation;
			const v = await verifier.verifyAttestation(a.statement, a.address_sig, a.entity_key, a.entity_sig, 921080, 921082);
			must(v === 'valid', v);
			const t = await verifier.verifyAttestation(a.statement.replace('nonce: 09', 'nonce: 0a'), a.address_sig, a.entity_key, a.entity_sig, 921080, 921082);
			must(t !== 'valid', 'a changed statement was accepted');
			return 'valid, and refused after one character changes';
		}
	},
	{
		id: 'proof',
		title: 'A delivered row proves it is in the data',
		detail: 'A customer gets a row plus a Merkle proof and checks it against the pinned root.',
		run: async (d) => {
			const pin = await verifier.pinned();
			const run = d.runs[0];
			const p = run.proofs![0];
			const ok = await verifier.verifyProof(pin.root, run.rows[0].leaf, p.index, p.leaf_count, p.siblings);
			must(ok, 'proof failed');
			const row = await verifier.decodeLeaf(run.rows[0].leaf);
			return `row ${row.entity}, ${p.siblings.length} sibling hash${p.siblings.length === 1 ? '' : 'es'}`;
		}
	},
	{
		id: 'bip322',
		title: 'Signature rules pass all test vectors',
		detail: 'The BIP-322 vectors compiled into the network code, run by the same checker the network uses.',
		run: async () => {
			const v = await verifier.bip322Vectors();
			const bad = v.filter((x) => x.got !== x.expect);
			must(v.length > 0 && bad.length === 0, `${bad.length} of ${v.length} vectors differ`);
			return `${v.length} of ${v.length} vectors give the expected verdict`;
		}
	}
];

export const checks: Check[] = FALLBACK
	? [...allChecks.filter((c) => !LABEL_ROW_CHECKS.has(c.id)).slice(0, 2), poolCheck, ...allChecks.filter((c) => !LABEL_ROW_CHECKS.has(c.id)).slice(2)]
	: allChecks;

export { loadDemo };
