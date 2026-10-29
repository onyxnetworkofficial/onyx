// Check an API answer, or a payment offer, with the network's own Rust code (NS8, ADR-0047):
// this runs the `onyx-check` command (crates/onyx-check). Nothing is checked in JavaScript.
import { spawn } from 'node:child_process';

/** The command's exit codes. */
const CODES = { 0: 'verified', 1: 'refused', 2: 'usage', 3: 'undecided' };

function run(bin, args, stdin) {
	return new Promise((resolve, reject) => {
		// No shell: arguments go to the program as they are.
		const p = spawn(bin, args, { stdio: ['pipe', 'pipe', 'pipe'] });
		let out = '';
		let err = '';
		p.stdout.on('data', (d) => (out += d));
		p.stderr.on('data', (d) => (err += d));
		p.on('error', (e) => reject(new Error(`cannot run ${bin}: ${e.message} (build it: cargo build --release -p onyx-check)`)));
		p.on('close', (code) => {
			try {
				resolve({ exitCode: code, report: JSON.parse(out) });
			} catch {
				reject(new Error(`${bin} wrote no report (exit ${code}): ${err.slice(0, 300)}`));
			}
		});
		p.stdin.on('error', () => {});
		p.stdin.end(stdin);
	});
}

const binOf = (bin) => bin || process.env.ONYX_CHECK_BIN || 'onyx-check';

/**
 * Check `answer` (the API's JSON body) against `asked` (what you asked: see crates/onyx-check).
 * `node` is your own chain node { rpc, chainId, contract }; without it you must pass
 * `unchecked: true`. Returns { outcome, exitCode, report }. Only outcome 'verified' is verified:
 * a check that skipped the chain read is 'unverified_on_chain' even though the rest passed.
 */
export async function checkAnswer({ answer, asked, node, unchecked = false, bin }) {
	const args = ['--answer', '-', '--asked', JSON.stringify(asked)];
	if (node) args.push('--rpc', node.rpc, '--chain-id', String(node.chainId), '--contract', node.contract);
	if (unchecked) args.push('--unchecked');
	const { exitCode, report } = await run(binOf(bin), args, JSON.stringify(answer));
	let outcome = CODES[exitCode] ?? 'error';
	if (outcome === 'verified' && report.chainChecked !== true) outcome = 'unverified_on_chain';
	return { outcome, exitCode, report };
}

/**
 * Check a 402 offer before paying it: one option must pay the OnyxRevenue contract named in the
 * deployment manifest you trust (a file path), on its testnet, for at most `maxAmount` (the
 * token's smallest unit), and your node `rpc` must hold that contract's code. Returns
 * { outcome: 'payable' | 'refused' | 'undecided' | 'usage', exitCode, report }; report.pay is
 * the only option to sign.
 */
export async function checkOffer({ offer, manifest, maxAmount, rpc, bin }) {
	const args = ['--offer', '-', '--manifest', manifest, '--max-amount', String(maxAmount), '--rpc', rpc];
	const { exitCode, report } = await run(binOf(bin), args, JSON.stringify(offer));
	return { outcome: { 0: 'payable', 1: 'refused', 2: 'usage', 3: 'undecided' }[exitCode] ?? 'error', exitCode, report };
}

/** The node settings from the environment, or null. Half-set settings are an error. */
export function nodeFromEnv(env = process.env) {
	const { ONYX_CHECK_RPC: rpc, ONYX_CHECK_CHAIN_ID: chainId, ONYX_CHECK_CONTRACT: contract } = env;
	const set = [rpc, chainId, contract].filter(Boolean).length;
	if (set === 0) return null;
	if (set !== 3) throw new Error('ONYX_CHECK_RPC, ONYX_CHECK_CHAIN_ID and ONYX_CHECK_CONTRACT go together');
	return { rpc, chainId: Number(chainId), contract };
}

/** The payment settings from the environment, or null. */
export function payFromEnv(env = process.env) {
	const { ONYX_PAY_MANIFEST: manifest, ONYX_PAY_MAX_AMOUNT: maxAmount, ONYX_CHECK_RPC: rpc } = env;
	if (!manifest && !maxAmount) return null;
	if (!manifest || !maxAmount || !rpc) throw new Error('ONYX_PAY_MANIFEST, ONYX_PAY_MAX_AMOUNT and ONYX_CHECK_RPC go together');
	if (!/^[0-9]+$/.test(maxAmount)) throw new Error('ONYX_PAY_MAX_AMOUNT: a whole number in the token\'s smallest unit');
	return { manifest, maxAmount, rpc };
}
