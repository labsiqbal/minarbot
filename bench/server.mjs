import { spawn } from 'node:child_process';
import { access, mkdir, realpath, stat } from 'node:fs/promises';
import { constants } from 'node:fs';
import { homedir } from 'node:os';
import { isAbsolute, join, relative } from 'node:path';

const names = {
    codex: ['codex'], claude: ['claude'], hermes: ['hermes-acp', 'hermes'], antigravity: ['agy', 'antigravity'],
    grok: ['grok'], pi: ['pi'], opencode: ['opencode'], gemini: ['gemini'], kiro: ['kiro-cli'],
};
async function findExecutables(path) {
    const dirs = [...new Set(path.split(':').filter(isAbsolute))];
    const executables = {};
    for (const [runtime, candidates] of Object.entries(names)) {
        const candidatePaths = candidates.flatMap(name => dirs.map(dir => join(dir, name)));
        for (const candidate of candidatePaths) {
            if (!(await stat(candidate).catch(() => null))?.isFile()) continue;
            if (!await access(candidate, constants.X_OK).then(() => true, () => false)) continue;
            executables[runtime] = candidate;
            break;
        }
    }
    return executables;
}

export async function startServer({ binary, scratch, fixture, provider, dist }) {
    const home = join(scratch, 'home'), tmp = join(scratch, 'tmp'), data = join(scratch, 'data');
    await Promise.all([home, tmp, data].map(path => mkdir(path, { recursive: true, mode: 0o700 })));
    const database = join(data, 'minarbot.sqlite');
    const args = [
        '--die-with-parent', '--unshare-pid', '--new-session', '--ro-bind', '/', '/', '--proc', '/proc', '--dev', '/dev',
        '--tmpfs', '/tmp', '--perms', '1777', '--tmpfs', '/dev/shm', '--bind', scratch, scratch,
    ];
    const env = { PATH: process.env.PATH ?? '/usr/bin:/bin', LANG: 'C.UTF-8', HOME: home, TMPDIR: tmp, MINARBOT_TEST_SERVER: 'rust' };
    // Keep CLI state disposable and expose only the device credential files through read-only mounts.
    for (const [name, file, source] of [
        ['.codex', 'auth.json', process.env.MINARBOT_BENCH_CODEX_HOME ?? join(homedir(), '.codex')],
        ['.claude', '.credentials.json', process.env.MINARBOT_BENCH_CLAUDE_HOME ?? join(homedir(), '.claude')],
    ]) {
        const dir = join(home, name);
        await mkdir(dir, { mode: 0o700 });
        if (!isAbsolute(source)) throw Error('Device sign-in directory must be an absolute path');
        const credential = join(source, file);
        if ((await stat(credential).catch(error => { if (error.code !== 'ENOENT') throw error; }))?.isFile()) {
            const target = await realpath(credential);
            const outside = relative(scratch, target);
            if (!outside.startsWith('../')) throw Error('Device credential must be outside the disposable files');
            args.push('--ro-bind', target, join(dir, file));
        }
    }
    const config = {
        database, port: 0, dist,
        desktop: { enabled: !!fixture, max: 1, leaseDir: join(scratch, 'desktop-leases'),
            ...(fixture && { testOnlyProxyTarget: { hostname: 'job-bench.test', port: fixture.port } }) },
        harness: { executables: await findExecutables(env.PATH) },
        ...(provider && { accounts: [provider] }),
    };
    const child = spawn('/usr/bin/bwrap', [...args, '--chdir', scratch, binary, '--test-config'], {
        env, stdio: ['pipe', 'pipe', 'ignore'],
    });
    const closed = new Promise(resolve => child.once('close', resolve));
    const close = async () => {
        if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
        const timer = setTimeout(() => {
            if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
        }, 5000);
        try { await closed; } finally { clearTimeout(timer); }
    };
    child.stdin.on('error', () => {});
    child.stdin.end(JSON.stringify(config));

    try {
        const origin = await new Promise((resolve, reject) => {
            const timer = setTimeout(() => reject(Error('Disposable server startup timed out')), 15000);
            let output = '';
            const fail = error => { clearTimeout(timer); reject(error); };
            child.once('error', fail);
            void closed.then(code => fail(Error(`Disposable server exited before readiness (${code ?? 'signal'})`)));
            child.stdout.setEncoding('utf8').on('data', chunk => {
                output = (output + chunk).slice(-8192);
                const match = /^Minarbot: (http:\/\/127\.0\.0\.1:[1-9]\d*)\r?$/m.exec(output);
                if (match) { clearTimeout(timer); resolve(match[1]); }
            });
        });
        const http = (path, body, timeout = 10000) => {
            if (!path.startsWith('/') || path.startsWith('//') || new URL(path, origin).origin !== origin)
                throw Error('Invalid local API path');
            return fetch(origin + path, {
                method: body === undefined ? 'GET' : 'POST',
                headers: { Origin: origin, 'Content-Type': 'application/json' },
                body: body === undefined ? undefined : JSON.stringify(body),
                redirect: 'error', signal: AbortSignal.timeout(timeout),
            });
        };
        const api = async (path, body, timeout) => {
            const response = await http(path, body, timeout);
            if (!response.ok) {
                await response.body?.cancel();
                throw Error(`HTTP ${response.status} at ${path.replace(/[0-9a-f]{8}-[0-9a-f-]{27}/g, '{id}')}`);
            }
            return response.json();
        };
        return { api, http, origin, database, close };
    } catch (error) {
        await close();
        throw error;
    }
}
