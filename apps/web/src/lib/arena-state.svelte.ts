// The Markets dashboard's state (src/routes/markets): the markets, simulated traders, the viewer's
// practice account, flash rounds, parlays, quests and the leaderboard. Everything runs in this
// browser with practice credits: the prices move because simulated traders trade against the
// LMSR market maker (arena.ts), and a market resolves when the presenter asks, through a replay
// of the Onyx observer check. Nothing is sent anywhere; the account is kept in localStorage.
import { level, parlayPayout, prices, quote, refundFor, rng, seed } from '#lib/arena.ts';

export type Category = 'Economy' | 'Bitcoin' | 'Ethereum' | 'Companies' | 'AI & compute' | 'Weather';
export const CATEGORIES: Category[] = ['Economy', 'Bitcoin', 'Ethereum', 'Companies', 'AI & compute', 'Weather'];

export type Market = {
	id: string;
	kind: 'binary' | 'range';
	cat: Category;
	title: string;
	outcomes: string[];
	/** The value the observers agree on, per winning outcome (what the receipt shows). */
	evidence: string[];
	source: string;
	dataset: string;
	closes: string;
	b: number;
	q: number[];
	/** Price history per outcome, oldest first (simulated). */
	hist: number[][];
	vol: number;
	traders: number;
	/** The hidden probability the simulated traders lean toward. */
	lean: number[];
	status: 'open' | 'resolving' | 'resolved';
	winner: number | null;
};

type Def = Omit<Market, 'q' | 'hist' | 'vol' | 'traders' | 'lean' | 'status' | 'winner'> & { open: number[]; vol: number };

const DEFS: Def[] = [
	{ id: 'debt-406', kind: 'binary', cat: 'Economy', title: 'U.S. total public debt above $40.60 trillion on 30 Nov 2026?', outcomes: ['Yes', 'No'], evidence: ['$40,612,880,301,447.18', '$40,571,004,512,903.66'], source: 'U.S. Treasury, Debt to the Penny API', dataset: 'us.treasury.total_public_debt_outstanding', closes: '29 Nov 2026', b: 900, open: [0.62, 0.38], vol: 48210 },
	{ id: 'debt-range', kind: 'range', cat: 'Economy', title: 'U.S. total public debt on 31 Dec 2026', outcomes: ['Below $40.50T', '$40.50T–$40.60T', '$40.60T–$40.70T', '$40.70T–$40.80T', '$40.80T or more'], evidence: ['$40,486,112,004,915.02', '$40,557,301,846,220.40', '$40,651,918,370,662.13', '$40,740,207,155,018.77', '$40,833,560,918,004.51'], source: 'U.S. Treasury, Debt to the Penny API', dataset: 'us.treasury.total_public_debt_outstanding', closes: '30 Dec 2026', b: 1100, open: [0.08, 0.27, 0.36, 0.21, 0.08], vol: 91340 },
	{ id: 'btc-fees', kind: 'binary', cat: 'Bitcoin', title: 'Average Bitcoin fees above 0.05 BTC per block on 15 Nov 2026?', outcomes: ['Yes', 'No'], evidence: ['0.0617 BTC per block', '0.0389 BTC per block'], source: 'Bitcoin blocks, recomputed by an open recipe', dataset: 'btc.chain-stats', closes: '14 Nov 2026', b: 600, open: [0.41, 0.59], vol: 22870 },
	{ id: 'btc-blocks', kind: 'range', cat: 'Bitcoin', title: 'Bitcoin blocks mined 1–7 Nov 2026', outcomes: ['Under 990', '990–1,019', '1,020–1,049', '1,050 or more'], evidence: ['982 blocks', '1,006 blocks', '1,031 blocks', '1,058 blocks'], source: 'Bitcoin blocks, recomputed by an open recipe', dataset: 'btc.blocks', closes: '31 Oct 2026', b: 700, open: [0.16, 0.39, 0.33, 0.12], vol: 30410 },
	{ id: 'pool-share', kind: 'binary', cat: 'Bitcoin', title: 'Largest mining pool finds over 30% of blocks this week?', outcomes: ['Yes', 'No'], evidence: ['31.8% of blocks', '28.4% of blocks'], source: 'Coinbase tags in Bitcoin blocks', dataset: 'btc.pools', closes: '1 Nov 2026', b: 500, open: [0.55, 0.45], vol: 15920 },
	{ id: 'eth-gas', kind: 'binary', cat: 'Ethereum', title: 'Ethereum median gas price under 1 gwei on 10 Nov 2026?', outcomes: ['Yes', 'No'], evidence: ['0.71 gwei', '1.36 gwei'], source: 'Ethereum blocks, recomputed by an open recipe', dataset: 'eth.chain-stats', closes: '9 Nov 2026', b: 600, open: [0.68, 0.32], vol: 19650 },
	{ id: 'etf-btc', kind: 'binary', cat: 'Companies', title: 'U.S. spot Bitcoin ETFs hold over 1.40 million BTC on 1 Dec 2026?', outcomes: ['Yes', 'No'], evidence: ['1,412,906 BTC', '1,381,440 BTC'], source: 'Fund issuers’ daily holdings files', dataset: 'sec.etf-btc', closes: '30 Nov 2026', b: 800, open: [0.47, 0.53], vol: 37280 },
	{ id: 'sec-filer', kind: 'binary', cat: 'Companies', title: 'Largest corporate Bitcoin holder reports more BTC in its next 10-Q?', outcomes: ['Yes', 'No'], evidence: ['More units than the last filing', 'Same or fewer units'], source: 'SEC EDGAR filings', dataset: 'sec.crypto-units', closes: 'at the next 10-Q', b: 700, open: [0.83, 0.17], vol: 26110 },
	{ id: 'gpu-h100', kind: 'binary', cat: 'AI & compute', title: 'H100 GPU rental index below $2.00 per hour on 31 Dec 2026?', outcomes: ['Yes', 'No'], evidence: ['$1.87 per GPU-hour', '$2.14 per GPU-hour'], source: 'A published GPU rental price index', dataset: 'compute.gpu-rental (proposed)', closes: '30 Dec 2026', b: 500, open: [0.36, 0.64], vol: 12740 },
	{ id: 'ai-tokens', kind: 'binary', cat: 'AI & compute', title: 'One model passes 1 billion tokens in a day in a public usage sample?', outcomes: ['Yes', 'No'], evidence: ['1.21 billion tokens', '0.88 billion tokens'], source: 'A model aggregator’s public usage sample', dataset: 'ai.model-usage (proposed)', closes: '30 Dec 2026', b: 500, open: [0.58, 0.42], vol: 17330 },
	{ id: 'nyc-snow', kind: 'binary', cat: 'Weather', title: 'Measurable snow in New York’s Central Park before 1 Dec 2026?', outcomes: ['Yes', 'No'], evidence: ['0.4 in on 26 Nov 2026', '0.0 in all month'], source: 'NOAA daily station records', dataset: 'weather.noaa-daily (proposed)', closes: '30 Nov 2026', b: 400, open: [0.22, 0.78], vol: 9850 },
	{ id: 'cpi', kind: 'range', cat: 'Economy', title: 'U.S. CPI, 12-month change, November 2026 release', outcomes: ['Under 2.5%', '2.5%–2.8%', '2.9%–3.2%', '3.3% or more'], evidence: ['2.4%', '2.7%', '3.0%', '3.4%'], source: 'U.S. Bureau of Labor Statistics', dataset: 'us.bls.cpi (proposed)', closes: 'on CPI release day', b: 800, open: [0.12, 0.41, 0.35, 0.12], vol: 41760 }
];

/** Datasets a new market can be listed on. `latest` is the newest value: the debt is the real
 * 2026-10-22 value the Onyx observers agreed on (PP9); the others are sample values. */
export type Dataset = { id: string; name: string; cat: Category; unit: string; source: string; latest: number; sample: boolean; step: number };
export const DATASETS: Dataset[] = [
	{ id: 'us.treasury.total_public_debt_outstanding', name: 'U.S. total public debt', cat: 'Economy', unit: 'trillion USD', source: 'U.S. Treasury, Debt to the Penny API', latest: 40.44, sample: false, step: 0.01 },
	{ id: 'btc.chain-stats', name: 'Average Bitcoin fees per block', cat: 'Bitcoin', unit: 'BTC', source: 'Bitcoin blocks, recomputed by an open recipe', latest: 0.045, sample: true, step: 0.001 },
	{ id: 'btc.pools', name: 'Largest mining pool’s share of blocks', cat: 'Bitcoin', unit: '%', source: 'Coinbase tags in Bitcoin blocks', latest: 29.5, sample: true, step: 0.1 },
	{ id: 'eth.chain-stats', name: 'Ethereum median gas price', cat: 'Ethereum', unit: 'gwei', source: 'Ethereum blocks, recomputed by an open recipe', latest: 0.9, sample: true, step: 0.01 },
	{ id: 'sec.etf-btc', name: 'U.S. spot Bitcoin ETF holdings', cat: 'Companies', unit: 'million BTC', source: 'Fund issuers’ daily holdings files', latest: 1.38, sample: true, step: 0.01 },
	{ id: 'compute.gpu-rental', name: 'H100 GPU rental index', cat: 'AI & compute', unit: 'USD per hour', source: 'A published GPU rental price index', latest: 2.05, sample: true, step: 0.01 },
	{ id: 'weather.noaa-daily', name: 'New York Central Park daily high', cat: 'Weather', unit: '°F', source: 'NOAA daily station records', latest: 58, sample: true, step: 1 }
];
export type NewMarket = { dataset: string; above: boolean; threshold: number; date: string; chance: number };

const HANDLES = Array.from({ length: 40 }, (_, i) => `trader·${((i * 2654435761) >>> 0).toString(16).slice(0, 4)}`);
const HIST = 90;
const START = 1000;
const KEY = 'onyx-markets-v2';

export type Position = { mid: string; i: number; shares: number; cost: number };
export type FeedItem = { id: number; who: string; mid: string; outcome: string; spend: number; price: number; you: boolean; sell: boolean };
export type Toast = { id: number; text: string; tone: 'good' | 'bad' | 'info' };
export type Leg = { mid: string; i: number };
export type ParlayTicket = { legs: (Leg & { price: number; title: string; outcome: string; hit: boolean | null })[]; stake: number; payout: number; state: 'live' | 'won' | 'lost' };
export type Quest = { id: string; text: string; goal: number; reward: number };
export const QUESTS: Quest[] = [
	{ id: 'trade', text: 'Make 3 trades', goal: 3, reward: 50 },
	{ id: 'flash', text: 'Win a flash round', goal: 1, reward: 75 },
	{ id: 'range', text: 'Trade a range market', goal: 1, reward: 40 },
	{ id: 'parlay', text: 'Place a parlay', goal: 1, reward: 60 }
];
export const BADGES: Record<string, string> = {
	first: 'First trade',
	win: 'First win',
	streak3: '3 wins in a row',
	parlay: 'Parlay hit',
	ten: '10 trades',
	cashout: 'Cashed out in profit',
	lv5: 'Level 5',
	maker: 'Listed a market'
};

type Saved = {
	balance: number;
	xp: number;
	streak: number;
	best: number;
	wins: number;
	losses: number;
	trades: number;
	positions: Position[];
	quests: Record<string, number>;
	claimed: string[];
	badges: string[];
	limit: number;
	spentToday: number;
	day: string;
	markets: Record<string, { q: number[]; status: Market['status']; winner: number | null; vol: number; traders: number }>;
	custom?: Def[];
};

function freshMarket(d: Def, k: number): Market {
	const r = rng(7919 * (k + 1));
	const q = seed(d.open, d.b);
	// A plausible simulated past: the opening price wanders toward today's.
	const hist = d.open.map(() => [] as number[]);
	const walk = d.open.map((p) => Math.min(0.95, Math.max(0.03, p + (r() - 0.5) * 0.18)));
	for (let t = 0; t < HIST; t++) {
		const f = t / (HIST - 1);
		const raw = walk.map((w, j) => w * (1 - f) + d.open[j] * f + (r() - 0.5) * 0.03 * (1 - f));
		const s = raw.reduce((n, x) => n + Math.max(0.01, x), 0);
		raw.forEach((x, j) => hist[j].push(Math.max(0.01, x) / s));
	}
	const lean = d.open.map((p) => Math.max(0.02, p + (r() - 0.5) * 0.04));
	const ls = lean.reduce((n, x) => n + x, 0);
	return { ...d, q, hist, vol: d.vol, traders: 120 + Math.floor(r() * 900), lean: lean.map((x) => x / ls), status: 'open', winner: null };
}

const today = () => new Date().toISOString().slice(0, 10);

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
/** One date style across the practice markets: "29 Nov 2026" (UTC). */
export function dayLabel(ms: number): string {
	const d = new Date(ms);
	return `${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]} ${d.getUTCFullYear()}`;
}

class Arena {
	markets = $state<Market[]>(DEFS.map(freshMarket));
	/** Markets the viewer listed (Create), kept with the account. */
	custom = $state<Def[]>([]);
	balance = $state(START);
	xp = $state(0);
	streak = $state(0);
	best = $state(0);
	wins = $state(0);
	losses = $state(0);
	trades = $state(0);
	positions = $state<Position[]>([]);
	quests = $state<Record<string, number>>({});
	claimed = $state<string[]>([]);
	badges = $state<string[]>([]);
	limit = $state(0);
	spentToday = $state(0);
	day = today();
	feed = $state<FeedItem[]>([]);
	toasts = $state<Toast[]>([]);
	live = $state(true);
	sound = $state(false);
	/** Bumped on every win, so the page can throw confetti. */
	celebrate = $state(0);
	/** The last price move per market (d: +1 up, -1 down; n changes each time), for the flash on its card. */
	moved = $state<Record<string, { d: number; n: number }>>({});
	parlays = $state<ParlayTicket[]>([]);
	board = $state(HANDLES.slice(0, 9).map((who, k) => ({ who, pnl: Math.round((rng(k + 3)() - 0.35) * 2400) })));

	// Flash rounds: a simulated index, 30-second rounds (20 open, 10 locked).
	flashSeries = $state<number[]>([]);
	flashStart = $state(0);
	flashT = $state(0);
	flashRound = $state(1);
	flashPick = $state<{ dir: 'up' | 'down'; stake: number } | null>(null);
	flashPools = $state({ up: 0, down: 0 });
	flashLast = $state<{ dir: 'up' | 'down'; won: boolean | null; paid: number; from: number; to: number } | null>(null);
	flashHistory = $state<('up' | 'down')[]>([]);

	private n = 0;
	private timer: ReturnType<typeof setInterval> | null = null;
	private flashTimer: ReturnType<typeof setInterval> | null = null;
	private saveTimer: ReturnType<typeof setTimeout> | null = null;
	private audio: AudioContext | null = null;
	private r = rng(Date.now() & 0xffff);

	level = $derived(level(this.xp));
	held = $derived(this.positions.reduce((n, p) => n + this.value(p), 0));
	worth = $derived(this.balance + this.held);
	pnl = $derived(this.worth - START - this.claimed.reduce((n, id) => n + (QUESTS.find((x) => x.id === id)?.reward ?? 0), 0));

	market(id: string) {
		return this.markets.find((m) => m.id === id)!;
	}
	price(m: Market, i: number) {
		return prices(m.q, m.b)[i];
	}
	value(p: Position) {
		const m = this.market(p.mid);
		if (!m || m.status !== 'open') return 0;
		return refundFor(m.q, m.b, p.i, p.shares);
	}

	private loaded = false;
	start() {
		// Once per visit: the page may mount again after a trip to another page.
		if (!this.loaded) this.load();
		this.loaded = true;
		if (!this.flashSeries.length) {
			let v = 24.0;
			const s = [];
			for (let k = 0; k < 60; k++) {
				v = Math.max(5, v + (this.r() - 0.5) * 0.6);
				s.push(v);
			}
			this.flashSeries = s;
			this.flashStart = s[s.length - 1];
			this.flashPools = { up: 400 + Math.round(this.r() * 400), down: 400 + Math.round(this.r() * 400) };
		}
		// The feed opens with a short backlog of simulated trades at today's prices, so the panel is
		// not two lines beside a full grid while the first ticks arrive (one every 1.3 s).
		if (!this.feed.length) this.seedFeed(8);
		this.timer ??= setInterval(() => this.live && this.tick(), 1300);
		this.flashTimer ??= setInterval(() => this.live && this.flashTick(), 500);
	}
	stop() {
		if (this.timer) clearInterval(this.timer);
		if (this.flashTimer) clearInterval(this.flashTimer);
		this.timer = this.flashTimer = null;
		this.save(true);
	}

	/** One simulated trader trades; every open market's history steps forward. */
	tick() {
		const open = this.markets.filter((m) => m.status === 'open');
		if (open.length) {
			const m = open[Math.floor(this.r() * open.length)];
			const p = prices(m.q, m.b);
			// Lean: buy the outcome most under its hidden probability, with noise.
			const gaps = p.map((x, j) => m.lean[j] - x + (this.r() - 0.5) * 0.08);
			const i = gaps.indexOf(Math.max(...gaps));
			// Small trades against a deep market maker: a few points of drift, not jumps.
			const spend = Math.round(3 + this.r() ** 3 * Math.min(140, m.b / 8));
			const before = p[i];
			m.q[i] += quote(m.q, m.b, i, spend).shares;
			m.vol += spend;
			if (this.r() < 0.3) m.traders += 1;
			this.moved = { ...this.moved, [m.id]: { d: m.kind === 'binary' && i === 1 ? -1 : 1, n: ++this.n } };
			this.pushFeed({ who: HANDLES[Math.floor(this.r() * HANDLES.length)], mid: m.id, outcome: m.outcomes[i], spend, price: before, you: false, sell: false });
			if (this.r() < 0.04) {
				const l = m.lean.map((x) => x * (0.93 + this.r() * 0.14));
				const ls = l.reduce((n, x) => n + x, 0);
				m.lean = l.map((x) => x / ls);
			}
		}
		for (const m of this.markets) {
			if (m.status !== 'open') continue;
			const p = prices(m.q, m.b);
			m.hist.forEach((h, j) => {
				h.push(p[j]);
				if (h.length > HIST) h.shift();
			});
		}
		const k = Math.floor(this.r() * this.board.length);
		this.board[k].pnl += Math.round((this.r() - 0.48) * 60);
	}

	/** Simulated trades for the feed only: prices, volume and positions do not move. */
	private seedFeed(n: number) {
		const open = this.markets.filter((m) => m.status === 'open');
		if (!open.length) return;
		for (let k = 0; k < n; k++) {
			const m = open[Math.floor(this.r() * open.length)];
			const p = prices(m.q, m.b);
			const gaps = p.map((x, j) => m.lean[j] - x + (this.r() - 0.5) * 0.08);
			const i = gaps.indexOf(Math.max(...gaps));
			const spend = Math.round(3 + this.r() ** 3 * Math.min(140, m.b / 8));
			this.pushFeed({ who: HANDLES[Math.floor(this.r() * HANDLES.length)], mid: m.id, outcome: m.outcomes[i], spend, price: p[i], you: false, sell: false });
		}
	}

	private pushFeed(f: Omit<FeedItem, 'id'>) {
		this.feed = [{ ...f, id: ++this.n }, ...this.feed].slice(0, 14);
	}
	toast(text: string, tone: Toast['tone'] = 'info') {
		const id = ++this.n;
		this.toasts = [...this.toasts, { id, text, tone }].slice(-3);
		setTimeout(() => (this.toasts = this.toasts.filter((t) => t.id !== id)), 3600);
	}
	beep(kind: 'fill' | 'win' | 'lose') {
		if (!this.sound) return;
		try {
			this.audio ??= new AudioContext();
			const a = this.audio;
			const notes = kind === 'win' ? [660, 880, 1320] : kind === 'fill' ? [520, 780] : [300, 220];
			notes.forEach((f, k) => {
				const o = a.createOscillator();
				const g = a.createGain();
				o.frequency.value = f;
				o.type = 'triangle';
				g.gain.setValueAtTime(0.0001, a.currentTime + k * 0.09);
				g.gain.exponentialRampToValueAtTime(0.12, a.currentTime + k * 0.09 + 0.02);
				g.gain.exponentialRampToValueAtTime(0.0001, a.currentTime + k * 0.09 + 0.18);
				o.connect(g).connect(a.destination);
				o.start(a.currentTime + k * 0.09);
				o.stop(a.currentTime + k * 0.09 + 0.2);
			});
		} catch {
			/* no audio: silent */
		}
	}

	/** Why a spend is refused, or null when it can go through. */
	refuse(spend: number): string | null {
		if (!(spend > 0)) return 'Enter an amount above 0.';
		if (spend > this.balance + 1e-9) return 'Not enough practice credits.';
		this.rollDay();
		if (this.limit > 0 && this.spentToday + spend > this.limit + 1e-9) return `Your daily practice limit is ${this.limit} credits. Change it under Portfolio.`;
		return null;
	}
	private rollDay() {
		if (this.day !== today()) {
			this.day = today();
			this.spentToday = 0;
		}
	}

	buy(mid: string, i: number, spend: number): string | null {
		const m = this.market(mid);
		if (m.status !== 'open') return 'This market is closed.';
		const no = this.refuse(spend);
		if (no) return no;
		const before = this.price(m, i);
		const qt = quote(m.q, m.b, i, spend);
		m.q[i] += qt.shares;
		m.vol += spend;
		m.hist.forEach((h, j) => h.push(this.price(m, j)));
		this.balance -= spend;
		this.spentToday += spend;
		const p = this.positions.find((x) => x.mid === mid && x.i === i);
		if (p) {
			p.shares += qt.shares;
			p.cost += spend;
		} else this.positions.push({ mid, i, shares: qt.shares, cost: spend });
		this.trades += 1;
		this.xp += Math.round(10 + spend / 5);
		this.moved = { ...this.moved, [mid]: { d: m.kind === 'binary' && i === 1 ? -1 : 1, n: ++this.n } };
		this.pushFeed({ who: 'You', mid, outcome: m.outcomes[i], spend, price: before, you: true, sell: false });
		this.progress('trade');
		if (m.kind === 'range') this.progress('range');
		this.badge('first');
		if (this.trades >= 10) this.badge('ten');
		this.toast(`Bought ${qt.shares.toFixed(1)} ${m.outcomes[i]} shares · pays ${qt.payout.toFixed(2)} if right`, 'good');
		this.beep('fill');
		this.save();
		return null;
	}

	cashOut(mid: string, i: number) {
		const m = this.market(mid);
		const k = this.positions.findIndex((x) => x.mid === mid && x.i === i);
		if (k < 0 || m.status !== 'open') return;
		const p = this.positions[k];
		const back = refundFor(m.q, m.b, i, p.shares);
		m.q[i] -= p.shares;
		this.balance += back;
		this.positions.splice(k, 1);
		this.trades += 1;
		const gain = back - p.cost;
		if (gain > 0) this.badge('cashout');
		this.pushFeed({ who: 'You', mid, outcome: m.outcomes[i], spend: back, price: this.price(m, i), you: true, sell: true });
		this.toast(`Cashed out ${back.toFixed(2)} credits (${gain >= 0 ? '+' : ''}${gain.toFixed(2)})`, gain >= 0 ? 'good' : 'bad');
		if (gain > 0) this.beep('fill');
		this.save();
	}

	/** Pay a resolved market: each winning share pays 1 credit. */
	settle(mid: string, winner: number) {
		const m = this.market(mid);
		m.status = 'resolved';
		m.winner = winner;
		let paid = 0;
		let stake = 0;
		let lost = 0;
		for (const p of this.positions.filter((x) => x.mid === mid)) {
			if (p.i === winner) {
				paid += p.shares;
				stake += p.cost;
			} else lost += p.cost;
		}
		this.positions = this.positions.filter((x) => x.mid !== mid);
		if (paid > 0) {
			this.balance += paid;
			this.win(Math.round(paid - stake));
			this.toast(`${m.outcomes[winner]} won · you collect ${paid.toFixed(2)} credits`, 'good');
		} else if (lost > 0) {
			this.lose();
			this.toast(`${m.outcomes[winner]} won · your position did not`, 'bad');
		}
		this.save();
	}
	reopen(mid: string) {
		const k = this.markets.findIndex((m) => m.id === mid);
		const d = [...DEFS, ...this.custom].find((x) => x.id === mid)!;
		this.markets[k] = freshMarket(d, k + this.n);
		this.save();
	}

	private win(gain: number) {
		this.wins += 1;
		this.streak += 1;
		this.best = Math.max(this.best, this.streak);
		this.xp += 50 + Math.max(0, gain);
		this.badge('win');
		if (this.streak >= 3) this.badge('streak3');
		this.celebrate += 1;
		this.beep('win');
	}
	private lose() {
		this.losses += 1;
		this.streak = 0;
		this.beep('lose');
	}
	private progress(id: string) {
		this.quests = { ...this.quests, [id]: (this.quests[id] ?? 0) + 1 };
	}
	claim(id: string) {
		const q = QUESTS.find((x) => x.id === id)!;
		if (this.claimed.includes(id) || (this.quests[id] ?? 0) < q.goal) return;
		this.claimed = [...this.claimed, id];
		this.balance += q.reward;
		this.xp += q.reward;
		this.celebrate += 1;
		this.toast(`Quest done: +${q.reward} credits`, 'good');
		this.beep('win');
		this.save();
	}
	private badge(id: string) {
		if (this.badges.includes(id)) return;
		this.badges = [...this.badges, id];
		this.toast(`Badge unlocked: ${BADGES[id]}`, 'info');
	}

	// Flash rounds.
	flashTick() {
		const s = this.flashSeries;
		const last = s[s.length - 1];
		const next = Math.max(5, last + (this.r() - 0.5) * 0.5);
		this.flashSeries = [...s.slice(-59), next];
		this.flashT += 0.5;
		if (this.flashT < 20 && this.r() < 0.5) {
			const side = this.r() < 0.5 ? 'up' : 'down';
			this.flashPools = { ...this.flashPools, [side]: this.flashPools[side] + Math.round(5 + this.r() * 60) };
		}
		if (this.flashT >= 30) this.flashResolve();
	}
	flashMultiplier(dir: 'up' | 'down', extra = 0) {
		const pool = this.flashPools[dir] + extra;
		const total = this.flashPools.up + this.flashPools.down + extra;
		return (total * 0.98) / pool;
	}
	flashEnter(dir: 'up' | 'down', stake: number): string | null {
		if (this.flashT >= 20) return 'Picks are locked for this round. The next round opens in a few seconds.';
		if (this.flashPick) return 'You already have a pick in this round.';
		const no = this.refuse(stake);
		if (no) return no;
		this.balance -= stake;
		this.spentToday += stake;
		this.flashPick = { dir, stake };
		this.flashPools = { ...this.flashPools, [dir]: this.flashPools[dir] + stake };
		this.trades += 1;
		this.xp += 10;
		this.progress('trade');
		this.badge('first');
		this.beep('fill');
		this.save();
		return null;
	}
	private flashResolve() {
		const to = this.flashSeries[this.flashSeries.length - 1];
		const from = this.flashStart;
		const dir: 'up' | 'down' = to >= from ? 'up' : 'down';
		let won: boolean | null = null;
		let paid = 0;
		if (this.flashPick) {
			won = this.flashPick.dir === dir;
			if (won) {
				paid = (this.flashPick.stake * (this.flashPools.up + this.flashPools.down) * 0.98) / this.flashPools[dir];
				this.balance += paid;
				this.progress('flash');
				this.win(Math.round(paid - this.flashPick.stake));
				this.toast(`Flash round ${dir.toUpperCase()} · you won ${paid.toFixed(2)} credits`, 'good');
			} else {
				this.lose();
				this.toast(`Flash round went ${dir.toUpperCase()} · no win this time`, 'bad');
			}
		}
		this.flashLast = { dir: this.flashPick?.dir ?? dir, won, paid, from, to };
		this.flashHistory = [dir, ...this.flashHistory].slice(0, 12);
		this.flashPick = null;
		this.flashRound += 1;
		this.flashT = 0;
		this.flashStart = to;
		this.flashPools = { up: 300 + Math.round(this.r() * 500), down: 300 + Math.round(this.r() * 500) };
		this.save();
	}

	// Parlays: every leg must win; each leg resolves by its price as its probability.
	placeParlay(legs: Leg[], stake: number): string | null {
		if (legs.length < 2) return 'Pick at least 2 legs.';
		const no = this.refuse(stake);
		if (no) return no;
		const priced = legs.map((l) => {
			const m = this.market(l.mid);
			return { ...l, price: this.price(m, l.i), title: m.title, outcome: m.outcomes[l.i], hit: null as boolean | null };
		});
		this.balance -= stake;
		this.spentToday += stake;
		this.parlays = [{ legs: priced, stake, payout: parlayPayout(stake, priced.map((l) => l.price)), state: 'live' as const }, ...this.parlays].slice(0, 6);
		this.trades += 1;
		this.xp += 25;
		this.progress('trade');
		this.progress('parlay');
		this.badge('first');
		this.beep('fill');
		this.save();
		return null;
	}
	/** Reveal one more leg of the newest live parlay; returns false when nothing is left. */
	revealLeg(k: number): boolean {
		const t = this.parlays[k];
		if (!t || t.state !== 'live') return false;
		const leg = t.legs.find((l) => l.hit === null);
		if (!leg) return false;
		leg.hit = this.r() < leg.price;
		if (!leg.hit) {
			t.state = 'lost';
			this.lose();
			this.toast('Parlay missed on one leg', 'bad');
		} else if (t.legs.every((l) => l.hit)) {
			t.state = 'won';
			this.balance += t.payout;
			this.badge('parlay');
			this.win(Math.round(t.payout - t.stake));
			this.toast(`Parlay hit · +${t.payout.toFixed(2)} credits`, 'good');
		} else this.beep('fill');
		this.save();
		return t.state === 'live';
	}

	/** List a new Yes/No market on a dataset; returns its id, or an error. */
	addMarket(n: NewMarket): { id: string } | { error: string } {
		const ds = DATASETS.find((d) => d.id === n.dataset);
		if (!ds) return { error: 'Pick a dataset.' };
		if (!Number.isFinite(n.threshold) || n.threshold <= 0) return { error: 'Enter a threshold above 0.' };
		const day = Date.parse(`${n.date}T00:00:00Z`);
		if (!Number.isFinite(day) || day <= Date.now()) return { error: 'Pick a date in the future.' };
		if (!(n.chance >= 5 && n.chance <= 95)) return { error: 'The opening chance must be between 5% and 95%.' };
		const when = dayLabel(day);
		const close = dayLabel(day - 86_400_000);
		const v = `${n.threshold.toLocaleString('en-US', { maximumFractionDigits: 6 })} ${ds.unit}`;
		const near = (k: number) => `${(n.threshold * k).toLocaleString('en-US', { maximumFractionDigits: 4 })} ${ds.unit}`;
		const def: Def = {
			id: `new-${Date.now().toString(36)}`, kind: 'binary', cat: ds.cat,
			title: `${ds.name} ${n.above ? 'above' : 'below'} ${v} on ${when}?`, outcomes: ['Yes', 'No'],
			evidence: n.above ? [near(1.02), near(0.98)] : [near(0.98), near(1.02)],
			source: ds.source, dataset: ds.id, closes: close, b: 500, open: [n.chance / 100, 1 - n.chance / 100], vol: 0
		};
		this.custom = [...this.custom, def];
		const m = freshMarket(def, this.markets.length + this.n);
		m.traders = 1;
		this.markets = [m, ...this.markets];
		this.xp += 40;
		this.badge('maker');
		this.toast('Market listed. Traders can buy it now.', 'good');
		this.save();
		return { id: def.id };
	}

	reset() {
		this.custom = [];
		this.markets = DEFS.map(freshMarket);
		Object.assign(this, { balance: START, xp: 0, streak: 0, best: 0, wins: 0, losses: 0, trades: 0, spentToday: 0 });
		this.positions = [];
		this.quests = {};
		this.claimed = [];
		this.badges = [];
		this.parlays = [];
		this.feed = [];
		this.flashPick = null;
		this.flashLast = null;
		this.save(true);
		this.toast('Practice account reset to 1,000 credits');
	}

	save(now = false) {
		if (this.level.level >= 5) this.badge('lv5');
		const write = () => {
			const s: Saved = {
				balance: this.balance, xp: this.xp, streak: this.streak, best: this.best, wins: this.wins, losses: this.losses, trades: this.trades,
				positions: this.positions, quests: this.quests, claimed: this.claimed, badges: this.badges, limit: this.limit, spentToday: this.spentToday, day: this.day,
				markets: Object.fromEntries(this.markets.map((m) => [m.id, { q: m.q, status: m.status, winner: m.winner, vol: m.vol, traders: m.traders }])),
				custom: this.custom
			};
			try {
				localStorage.setItem(KEY, JSON.stringify(s));
			} catch {
				/* storage blocked: the account lasts for this visit */
			}
		};
		if (this.saveTimer) clearTimeout(this.saveTimer);
		if (now) write();
		else this.saveTimer = setTimeout(write, 400);
	}
	private load() {
		let s: Saved | null = null;
		try {
			s = JSON.parse(localStorage.getItem(KEY) ?? 'null');
		} catch {
			s = null;
		}
		if (!s || typeof s.balance !== 'number') return;
		Object.assign(this, { balance: s.balance, xp: s.xp, streak: s.streak, best: s.best, wins: s.wins, losses: s.losses, trades: s.trades, limit: s.limit ?? 0, spentToday: s.spentToday ?? 0, day: s.day ?? today() });
		this.custom = Array.isArray(s.custom) ? s.custom : [];
		if (this.custom.length) this.markets = [...this.custom.map((d, k) => freshMarket(d, 50 + k)), ...this.markets];
		this.positions = (s.positions ?? []).filter((p) => this.markets.some((m) => m.id === p.mid));
		this.quests = s.quests ?? {};
		this.claimed = s.claimed ?? [];
		this.badges = s.badges ?? [];
		for (const m of this.markets) {
			const x = s.markets?.[m.id];
			if (!x || x.q.length !== m.q.length) continue;
			m.q = x.q;
			m.status = x.status === 'resolving' ? 'open' : x.status;
			m.winner = x.winner;
			m.vol = x.vol;
			m.traders = x.traders;
			const p = prices(m.q, m.b);
			m.hist.forEach((h, j) => (h[h.length - 1] = p[j]));
		}
		this.rollDay();
	}
}

export const arena = new Arena();
