// All destinations remain discoverable in the footer and command palette.
// The header focuses on the four tasks a new visitor needs.
import { resolve } from '$app/paths';

export const NAV = [
	{ href: resolve('/how-it-works'), path: '/how-it-works', label: 'How it works' },
	{ href: resolve('/data'), path: '/data', label: 'Browse data' },
	{ href: resolve('/reserves'), path: '/reserves', label: 'Reserves' },
	{ href: resolve('/run'), path: '/run', label: 'Run' },
	{ href: resolve('/explorer'), path: '/explorer', label: 'Explorer' },
	{ href: resolve('/labels'), path: '/labels', label: 'Labels' },
	{ href: resolve('/api'), path: '/api', label: 'API' },
	{ href: resolve('/submit'), path: '/submit', label: 'Add data' },
	{ href: resolve('/markets'), path: '/markets', label: 'Markets' },
	{ href: resolve('/'), path: '/', label: 'Home' }
];

// MP13: Markets, tonight's demo, is a primary destination too.
export const PRIMARY_NAV = ['/data', '/markets', '/how-it-works', '/submit'].map((path) => NAV.find((n) => n.path === path)!);

export const FOOTER_NAV = ['/', '/submit', '/data', '/markets', '/how-it-works', '/run', '/reserves', '/explorer', '/labels', '/api'].map((path) => NAV.find((n) => n.path === path)!);
