// All destinations remain discoverable in the footer and command palette.
// The header focuses on the tasks a new visitor needs: four pages and the app's download.
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
	{ href: resolve('/download'), path: '/download', label: 'Download' },
	{ href: resolve('/markets'), path: '/markets', label: 'Markets' },
	{ href: resolve('/'), path: '/', label: 'Home' }
];

// MP13: Markets, tonight's demo, is a primary destination too. CL10: Download (the desktop app, as
// bitcoin.org's menu links its download) ends the header row.
export const PRIMARY_NAV = ['/data', '/markets', '/how-it-works', '/submit', '/download'].map((path) => NAV.find((n) => n.path === path)!);

export const FOOTER_NAV = ['/', '/submit', '/download', '/data', '/markets', '/how-it-works', '/run', '/reserves', '/explorer', '/labels', '/api'].map((path) => NAV.find((n) => n.path === path)!);
