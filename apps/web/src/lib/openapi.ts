/** The API reference page's reading of docs/api/openapi.json (pre-public loop PP6). Pure functions
 * over the spec, so the page and tests/pp6.mjs read it the same way. */
type Json = Record<string, unknown>;
export type Param = { name: string; in: string; required: boolean; description: string; schema: string };
export type Resp = { status: string; description: string; schema: string | null };
export type Op = {
	id: string;
	method: string;
	path: string;
	tag: string;
	summary: string;
	description: string;
	paid: boolean;
	params: Param[];
	body: string | null;
	responses: Resp[];
};
export type Code = { code: string; status: number; description: string };

function deref(spec: Json, v: unknown): Json {
	const o = (v ?? {}) as Json;
	const r = o.$ref;
	if (typeof r !== 'string') return o;
	let node: unknown = spec;
	for (const part of r.replace(/^#\//, '').split('/')) node = (node as Json)?.[part];
	if (!node) throw new Error(`openapi: unresolved ${r}`);
	return deref(spec, node);
}

/** A schema's short name for the page: its component name, its type, or "Error". */
function schemaName(spec: Json, s: unknown): string | null {
	const o = (s ?? null) as Json | null;
	if (!o) return null;
	if (typeof o.$ref === 'string') return o.$ref.split('/').pop() ?? null;
	if (Array.isArray(o.oneOf)) return o.oneOf.map((x) => schemaName(spec, x)).join(' or ');
	if (o.type === 'array') return `array of ${schemaName(spec, o.items) ?? 'values'}`;
	if (typeof o.pattern === 'string' && o.type === 'string') return `string ${o.pattern}`;
	return typeof o.type === 'string' ? o.type : 'object';
}

const ORDER = ['get', 'post', 'put', 'patch', 'delete'];

/** Every operation, in the spec's path order. */
export function operations(spec: Json): Op[] {
	const out: Op[] = [];
	for (const [path, item] of Object.entries((spec.paths ?? {}) as Record<string, Json>)) {
		for (const method of ORDER) {
			const op = item[method] as Json | undefined;
			if (!op) continue;
			const params = ((op.parameters ?? []) as unknown[]).map((p) => {
				const d = deref(spec, p);
				return { name: String(d.name), in: String(d.in), required: d.required === true, description: String(d.description ?? ''), schema: schemaName(spec, d.schema) ?? '' };
			});
			const responses = Object.entries((op.responses ?? {}) as Record<string, unknown>).map(([status, r]) => {
				const d = deref(spec, r);
				const content = (d.content as Json | undefined)?.['application/json'] as Json | undefined;
				return { status, description: String(d.description ?? ''), schema: schemaName(spec, content?.schema) };
			});
			const body = ((op.requestBody as Json | undefined)?.content as Json | undefined)?.['application/json'] as Json | undefined;
			out.push({
				id: String(op.operationId),
				method: method.toUpperCase(),
				path,
				tag: String(((op.tags ?? []) as string[])[0] ?? ''),
				summary: String(op.summary ?? ''),
				description: String(op.description ?? ''),
				paid: op['x-onyx-paid'] === true,
				params,
				body: body ? schemaName(spec, body.schema) : null,
				responses
			});
		}
	}
	return out;
}

/** Operations grouped by their first tag, in the spec's tag order. */
export function byTag(spec: Json): { tag: string; ops: Op[] }[] {
	const ops = operations(spec);
	const tags = ((spec.tags ?? []) as Json[]).map((t) => String(t.name));
	return tags.map((tag) => ({ tag, ops: ops.filter((o) => o.tag === tag) })).filter((g) => g.ops.length);
}

/** Every stable error code, by status then code. */
export function errorCodes(spec: Json): Code[] {
	return Object.entries((spec['x-onyx-error-codes'] ?? {}) as Record<string, Json>)
		.map(([code, v]) => ({ code, status: Number(v.status), description: String(v.description ?? '') }))
		.sort((a, b) => a.status - b.status || a.code.localeCompare(b.code));
}

/** Split text on `backticks` into plain and code runs (rendered without HTML injection). */
export function runs(text: string): { code: boolean; text: string }[] {
	return text.split('`').map((t, i) => ({ code: i % 2 === 1, text: t })).filter((r) => r.text);
}
