// The onyx slab's stone (OnyxSlab.svelte): a small WebGL shader (warped bands, jade veins, a
// soft glow). W4: it runs in a worker on an OffscreenCanvas where the browser allows it, so
// compiling the shader never blocks the page (on a software renderer, e.g. a machine without a
// GPU, compiling it held the main thread 0.5-13 s at 4x CPU throttling); the same code runs on
// the page itself where it does not.
export const VERT = 'attribute vec2 p;void main(){gl_Position=vec4(p,0.,1.);}';
export const FRAG = `precision highp float;
uniform vec2 r;uniform float t;
float h(vec2 p){return fract(sin(dot(p,vec2(127.1,311.7)))*43758.5453);}
float n(vec2 p){vec2 i=floor(p),f=fract(p);f=f*f*(3.-2.*f);
return mix(mix(h(i),h(i+vec2(1,0)),f.x),mix(h(i+vec2(0,1)),h(i+vec2(1,1)),f.x),f.y);}
float fbm(vec2 p){float v=0.,a=.5;for(int i=0;i<5;i++){v+=a*n(p);p=p*2.03+vec2(1.7,9.2);a*=.5;}return v;}
void main(){
 vec2 uv=gl_FragCoord.xy/r.y;
 vec2 q=vec2(fbm(uv*1.1+t*.018),fbm(uv*1.1+vec2(5.2,1.3)-t*.013));
 float w=fbm(uv*1.4+2.6*q);
 float bands=sin((uv.y*2.6+uv.x*1.1+w*6.5)*3.14159);
 float b=bands*.5+.5;
 vec3 col=vec3(.028,.029,.028);
 col+=vec3(.13,.12,.11)*smoothstep(.5,1.,b)*.6;
 col+=vec3(.92,.89,.82)*pow(b,16.)*.16;
 col+=vec3(1.,.98,.94)*pow(b,70.)*.12;
 col+=vec3(.06,.055,.05)*w;
 vec2 g=gl_FragCoord.xy/r-vec2(.8,.7);
 col+=vec3(.92,.89,.82)*.07*exp(-dot(g*vec2(1.6,1.),g*vec2(1.6,1.))*5.);
 vec2 v=gl_FragCoord.xy/r-.5;col*=1.-dot(v,v)*.9;
 gl_FragColor=vec4(col,1.);
}`;

type Canvas = HTMLCanvasElement | OffscreenCanvas;
/** What the page tells the stone: the canvas's CSS size, whether it may move, whether it is on screen. */
export type StoneState = {
	width: number;
	height: number;
	motionOff: boolean;
	visible: boolean;
	/** Test hook (tests/w4.mjs): drift even on a software renderer, so a headless browser can
	 *  check that pausing stops it. Set from localStorage `onyx-stone-test` = `animate`. */
	animate?: boolean;
};
/** What the stone is doing, for the browser tests. */
export type StoneInfo = { frames: number; scheduled: boolean; motionOff: boolean; visible: boolean; software: boolean };
type Stone = { set: (s: Partial<StoneState>) => void; stop: () => void; info: () => StoneInfo };

const raf: (f: (t: number) => void) => number =
	typeof requestAnimationFrame === 'function' ? (f) => requestAnimationFrame(f) : (f) => setTimeout(() => f(performance.now()), 16) as unknown as number;
const caf: (id: number) => void = typeof cancelAnimationFrame === 'function' ? (id) => cancelAnimationFrame(id) : (id) => clearTimeout(id);

/** Draw the stone on `canvas` and keep it drifting; `set` passes on what changed on the page. */
export function stone(canvas: Canvas, init: StoneState): Stone {
	const st = { ...init };
	const gl = canvas.getContext('webgl', { antialias: false, premultipliedAlpha: false }) as WebGLRenderingContext | null;
	// Without WebGL: one still gradient (2D canvas).
	const fallback = () => {
		const g = canvas.getContext('2d') as CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D | null;
		if (!g) return;
		canvas.width = Math.max(1, st.width);
		canvas.height = Math.max(1, st.height);
		const grad = g.createLinearGradient(0, 0, canvas.width, canvas.height);
		grad.addColorStop(0, '#151514');
		grad.addColorStop(1, '#050908');
		g.fillStyle = grad;
		g.fillRect(0, 0, canvas.width, canvas.height);
	};
	if (!gl) {
		fallback();
		return {
			set: (s) => (Object.assign(st, s), s.width !== undefined || s.height !== undefined ? fallback() : undefined),
			stop: () => {},
			info: () => ({ frames: 0, scheduled: false, motionOff: st.motionOff, visible: st.visible, software: true })
		};
	}
	const sh = (type: number, src: string) => {
		const s = gl.createShader(type)!;
		gl.shaderSource(s, src);
		gl.compileShader(s);
		return s;
	};
	const prog = gl.createProgram()!;
	gl.attachShader(prog, sh(gl.VERTEX_SHADER, VERT));
	gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, FRAG));
	gl.linkProgram(prog);
	if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
		// A context that cannot link the shader: the 2D canvas cannot be had on this canvas
		// any more, so the slab keeps its CSS ground colour.
		return { set: () => {}, stop: () => {}, info: () => ({ frames: 0, scheduled: false, motionOff: st.motionOff, visible: st.visible, software: true }) };
	}
	gl.useProgram(prog);
	gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
	gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
	const loc = gl.getAttribLocation(prog, 'p');
	gl.enableVertexAttribArray(loc);
	gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
	const uR = gl.getUniformLocation(prog, 'r');
	const uT = gl.getUniformLocation(prog, 't');
	// Software renderers (no GPU) get one still frame: the stone looks the
	// same, and the page stays responsive.
	const dbg = gl.getExtension('WEBGL_debug_renderer_info');
	const renderer = dbg ? String(gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL)) : '';
	const software = /swiftshader|llvmpipe|software|basic render/i.test(renderer);
	const still = () => (software && !st.animate) || st.motionOff;
	let frameId = 0;
	let scheduled = false;
	let frames = 0;
	const next = () => {
		frameId = raf(frame);
		scheduled = true;
	};
	const cancel = () => {
		caf(frameId);
		scheduled = false;
	};
	const t0 = performance.now();
	const size = () => {
		// The stone is soft, so half resolution looks the same at a quarter of the work.
		const dpr = 0.5;
		canvas.width = Math.max(1, Math.round(st.width * dpr));
		canvas.height = Math.max(1, Math.round(st.height * dpr));
		gl.viewport(0, 0, canvas.width, canvas.height);
		gl.uniform2f(uR, canvas.width, canvas.height);
	};
	// About 24 frames a second is plenty for drifting stone.
	let last = -1e9;
	const frame = (now: number) => {
		scheduled = false;
		if (now - last >= 41) {
			last = now;
			frames++;
			gl.uniform1f(uT, 40 + (now - t0) / 1000);
			gl.drawArrays(gl.TRIANGLES, 0, 3);
		}
		if (!still() && st.visible) next();
	};
	size();
	frame(t0);
	let stopped = false;
	return {
		set(s) {
			if (stopped) return;
			const resized = (s.width !== undefined && s.width !== st.width) || (s.height !== undefined && s.height !== st.height);
			Object.assign(st, s);
			cancel();
			if (resized) {
				size();
				// A new size clears the canvas: draw now.
				last = -1e9;
				frame(performance.now());
			} else if (!still() && st.visible) next();
		},
		stop() {
			stopped = true;
			cancel();
		},
		info: () => ({ frames, scheduled, motionOff: st.motionOff, visible: st.visible, software })
	};
}
