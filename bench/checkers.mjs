import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { open, readdir, writeFile, mkdtemp, rm, realpath } from 'node:fs/promises';
import { constants } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const exec = promisify(execFile);
export async function readOutput(workspace, name, artifacts = {}) {
    if (Object.hasOwn(artifacts, name))
        return artifacts[name];
    assert(/^[a-zA-Z0-9_.-]+$/.test(name) && name !== '.' && name !== '..', 'Invalid output name');
    const file = await open(join(workspace, name), constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    try {
        const info = await file.stat();
        assert(info.isFile(), 'Output must be a regular file');
        assert(info.size <= 256 * 1024, 'Output exceeds 256 KiB');
        return file.readFile('utf8');
    } finally {
        await file.close();
    }
}
// Submitted code runs read-only with no host home, credentials, network or benchmark files.
export async function sandbox(workspace, command, args, checker) {
    const scratch = await mkdtemp(join(tmpdir(), 'minarbot-check-'));
    try {
        if (checker)
            await writeFile(join(scratch, 'check.mjs'), checker);
        const mounts = ['/usr', '/lib', '/lib64', '/bin', '/opt'].flatMap(path => ['--ro-bind-try', path, path]);
        return await exec('/usr/bin/bwrap', ['--die-with-parent', '--unshare-all', ...mounts, '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/home', '--ro-bind', workspace, '/workspace', '--ro-bind', scratch, '/check', '--chdir', '/workspace', '--clearenv', '--setenv', 'PATH', '/usr/bin:/bin', '--setenv', 'HOME', '/tmp', command, ...args], { timeout: 12000, maxBuffer: 1024 * 1024 });
    }
    finally {
        await rm(scratch, { recursive: true, force: true });
    }
}
export function checkData(id, data) {
    switch (id) {
        case 'contacts-csv':
            assert.deepEqual(data.contacts, [{ name: 'Alice', email: 'alice@example.com' }, { name: 'Bob', email: 'bob@example.com' }, { name: 'Doe, Jane', email: 'jane@example.com' }]);
            assert.deepEqual(data.summary, { inputRows: 5, outputRows: 3, droppedRows: 2 });
            break;
        case 'sales-csv':
            assert.deepEqual(data, { currency: 'USD', paidOrders: 3, paidTotal: 35.3, byRegion: { West: 26.8, East: 8.5 } });
            break;
        case 'http-research':
            assert.deepEqual([...data].sort((a, b) => a.status - b.status), [
                { status: 200, title: 'OK', source: 'https://www.rfc-editor.org/rfc/rfc9110.html#section-15.3.1' },
                { status: 404, title: 'Not Found', source: 'https://www.rfc-editor.org/rfc/rfc9110.html#section-15.5.5' },
                { status: 503, title: 'Service Unavailable', source: 'https://www.rfc-editor.org/rfc/rfc9110.html#section-15.6.4' },
            ]);
            break;
        case 'csv-research':
            assert.deepEqual({ ...data, extension: data.extension?.toLowerCase() }, { mediaType: 'text/csv', extension: 'csv', rfc: 4180, source: 'https://www.iana.org/assignments/media-types/text/csv' });
            break;
        case 'team-review':
            assert.deepEqual(data.output, { subtotal: 100, tax: 10, total: 110, approved: true });
            assert(data.delegated && data.reviewerDone, 'Reviewer must finish a real delegated turn');
            break;
        case 'routine-setup':
            assert(data.some(row => row.name === 'Daily sales' && row.enabled === 0 && row.trigger === 'manual' && row.prompt === 'Read sales.csv and report paid revenue.' && row.report_to === 'direct'), 'Required saved, disabled manual routine missing');
            break;
        case 'browser-booking':
        case 'browser-ticket': {
            assert.deepEqual(data.submission, id === 'browser-booking' ? { choice: 'Team room', text: 'Friday planning', confirmed: true } : { choice: 'High', text: 'Printer offline', confirmed: true });
            for (const name of ['computer_open_url', 'computer_screenshot', 'computer_click', 'computer_type'])
                assert(data.tools.includes(name), `Missing Computer evidence: ${name}`);
            assert(data.steps.includes('review') && data.steps.includes('confirm'), 'Both browser steps required');
            break;
        }
        case 'handover-doc':
            for (const heading of ['Summary', 'Decisions', 'Actions', 'Risks'])
                assert(new RegExp(`^#{1,6}\\s+${heading}\\s*$`, 'im').test(data), `Missing ${heading}`);
            for (const fact of ['Atlas', '2026-12-01', '2400', 'email login', 'Maya', '2026-11-25', 'Rafi', '2026-11-27', 'vendor sandbox', 'local fixture'])
                assert(data.toLowerCase().includes(fact.toLowerCase()), `Missing ${fact}`);
            assert(/Maya[^\n]*2026-11-25/i.test(data) && /Rafi[^\n]*2026-11-27/i.test(data), 'Action owners must match dates');
            break;
        default: assert.fail(`Unknown data checker: ${id}`);
    }
}
const codeChecks = {
    'counter-app': `const m=await import('/workspace/counter.mjs'); assert.equal(m.increment(0),1); assert.equal(m.increment(9),10); assert.equal(m.reset(),0);`,
    'tip-app': `const m=await import('/workspace/tip.mjs'); assert(Math.abs(m.total(100,15)-115)<1e-9); assert.equal(m.total(0,15),0); assert.equal(m.total(20,0),20); assert.throws(()=>m.total(-1,10)); assert.throws(()=>m.total(1,-10));`,
    'cart-bug': `const m=await import('/workspace/cart.mjs'); assert.equal(m.total([]),0); assert.equal(m.total([{price:10.25,quantity:2},{price:1.1,quantity:3}]),23.8); assert.equal(m.total([{price:0.1,quantity:3}]),0.3); assert.equal(m.total([{price:9,quantity:0}]),0);`,
    'expiry-bug': `const m=await import('/workspace/auth.mjs'); assert.equal(m.isExpired(100,100),true); assert.equal(m.isExpired(100,99),false); assert.equal(m.isExpired(100,101),true); assert.equal(m.isExpired(null,101),false); assert.equal(m.isExpired(undefined,101),false); assert.equal(m.isExpired(0,0),true);`,
    'slug-feature': `const m=await import('/workspace/text.mjs'); assert.equal(m.greet('Ada'),'Hello, Ada!'); for(const [a,b] of [[' Hello, WORLD! ','hello-world'],['a---b','a-b'],['',''],['__Tea & Coffee__','tea-coffee']])assert.equal(m.slugify(a),b);`,
    'paginate-feature': `const m=await import('/workspace/list.mjs'); assert.equal(m.first([7]),7); assert.deepEqual(m.paginate([1,2,3,4,5],2,2),{items:[3,4],total:5,pages:3}); assert.deepEqual(m.paginate([],1,2),{items:[],total:0,pages:0}); assert.deepEqual(m.paginate([1],3,2),{items:[],total:1,pages:1}); for(const [p,s] of [[0,2],[1,0],[1.5,2],[1,2.5],[-1,2]])assert.throws(()=>m.paginate([],p,s),RangeError);`,
};
export const codeTaskIds = Object.keys(codeChecks);
export async function checkCode(id, workspace) {
    await sandbox(workspace, '/usr/bin/node', ['/check/check.mjs'], `import assert from 'node:assert/strict';\n${codeChecks[id]}`);
    const tests = (await readdir(workspace, { withFileTypes: true }))
        .filter(file => file.isFile() && /(?:^|\.)test\.(?:mjs|cjs|js|ts)$/.test(file.name)).map(file => file.name).sort();
    assert(tests.length, 'Native regression tests missing');
    await sandbox(workspace, '/usr/bin/node', ['--experimental-strip-types', '--test', ...tests.map(name => `/workspace/${name}`)]);
}
export async function renderCheck(workspace, html, script) {
    const scratch = await mkdtemp(join(tmpdir(), 'minarbot-render-'));
    try {
        // Load relative JavaScript and CSS alongside the HTML inside the networkless sandbox.
        for (const name of await readdir(workspace)) {
            if (/\.(?:mjs|js|css)$/.test(name))
                await writeFile(join(scratch, name), await readOutput(workspace, name));
        }
        await writeFile(join(scratch, 'index.html'), html + `<script>window.addEventListener('load',()=>{try{${script};const p=document.createElement('pre');p.id='bench-result';p.textContent='PASS';document.body.append(p)}catch{}})</script>`);
        const { stdout } = await sandbox(scratch, await realpath('/usr/bin/google-chrome'), ['--headless', '--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--user-data-dir=/tmp/chrome', '--allow-file-access-from-files', '--virtual-time-budget=1000', '--dump-dom', 'file:///workspace/index.html']);
        assert(stdout.includes('<pre id="bench-result">PASS</pre>'), 'Rendered behavior or required visible content failed');
    }
    finally {
        await rm(scratch, { recursive: true, force: true });
    }
}
export async function checkTask(task, context) {
    const { workspace, artifacts = {} } = context;
    const text = name => readOutput(workspace, name, artifacts);
    const json = async (name) => JSON.parse(await text(name));
    if (codeTaskIds.includes(task.id)) {
        await checkCode(task.id, workspace);
        if (task.id === 'counter-app')
            await renderCheck(workspace, await text('index.html'), `const a=(v,m)=>{if(!v)throw Error(m)};a(document.querySelector('h1')?.textContent==='Counter');const n=document.querySelector('#count');a(n?.textContent.trim()==='0');const b=[...document.querySelectorAll('button')];b.find(x=>x.textContent.trim()==='Add').click();b.find(x=>x.textContent.trim()==='Add').click();a(n.textContent.trim()==='2');b.find(x=>x.textContent.trim()==='Reset').click();a(n.textContent.trim()==='0')`);
        if (task.id === 'tip-app')
            await renderCheck(workspace, await text('index.html'), `const a=v=>{if(!v)throw Error()};document.querySelector('#bill').value='100';document.querySelector('#percent').value='15';[...document.querySelectorAll('button')].find(x=>x.textContent.trim()==='Calculate').click();a(document.querySelector('#total').textContent.includes('115.00'))`);
        return;
    }
    switch (task.id) {
        case 'contacts-csv': return checkData(task.id, { contacts: await json('contacts.json'), summary: await json('summary.json') });
        case 'sales-csv': return checkData(task.id, await json('sales-summary.json'));
        case 'http-research': return checkData(task.id, await json('http-facts.json'));
        case 'csv-research': return checkData(task.id, await json('csv-facts.json'));
        case 'handover-doc': return checkData(task.id, await text('handover.md'));
        case 'team-review': return checkData(task.id, { output: await json('review.json'), ...context.delegation });
        case 'routine-setup':
            assert.equal(context.routineRuns, 0, 'Routine must not run during setup');
            return checkData(task.id, context.routines);
        case 'browser-booking':
        case 'browser-ticket': return checkData(task.id, context.browser);
        case 'show-html': {
            await text('launch.html');
            assert(context.shown, 'HTML must be linked or published in the reply');
            const html = context.shownHtml;
            assert.equal(typeof html, 'string', 'Linked HTML must be reachable');
            await renderCheck(workspace, html, `const a=v=>{if(!v)throw Error()};a(document.body.innerText.includes('Atlas launch'));for(const [name,date] of [['Migration','2026-11-25'],['QA','2026-11-27'],['Launch','2026-12-01']])a([...document.querySelectorAll('tr')].some(r=>r.innerText.includes(name)&&r.innerText.includes(date)));a(document.querySelector('table')?.getBoundingClientRect().height>0)`);
            return;
        }
        default: assert.fail('Checker missing');
    }
}
