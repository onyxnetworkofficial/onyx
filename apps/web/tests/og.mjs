// Renders static/og.jpg (1200x630 share preview, via PNG then JPEG q82 with Pillow) from the built home page.
// Run after `pnpm build`: node tests/og.mjs
import { chromium } from 'playwright-core';
import { serve } from './serve.mjs';
const srv = await serve(0);
const b = await chromium.launch({ channel: 'chrome' });
const p = await b.newPage({ viewport: { width: 1200, height: 630 }, deviceScaleFactor: 1 });
await p.goto(`http://127.0.0.1:${srv.address().port}/`);
await p.waitForTimeout(2500);
await p.addStyleTag({ content: '.top{display:none!important}' });
await p.screenshot({ path: new URL('../static/og.png', import.meta.url).pathname });
await b.close();
srv.close();
console.log('wrote static/og.png; convert: python3 -c "from PIL import Image; Image.open(\'static/og.png\').convert(\'RGB\').save(\'static/og.jpg\', quality=82, optimize=True, progressive=True)" && rm static/og.png');
