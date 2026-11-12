function (list) {
    'use strict';
    // One fresh hidden view per section. No cookies, tokens or persistent storage are copied.
    const KEY = 'felifox-asana-v1:';
    const MAX_PAGES = 8, MAX_BYTES = 2 * 1024 * 1024, MAX_MS = 18000;
    const FIELDS = 'name,permalink_url,modified_at,due_on,due_at,memberships.project.name';
    const ROOT = 'https://app.asana.com/api/1.0/';
    const ME = ROOT + 'users/me?opt_fields=workspaces';
    const now = Date.now();
    const object = v => v !== null && typeof v === 'object' && !Array.isArray(v);
    const id = v => typeof v === 'string' && /^[0-9]{1,40}$/.test(v);
    const fail = message => {
        window.name = '';
        return (window.__felifoxAsanaResult = {error: 'Asana ' + message});
    };
    const tasksUrl = (workspace, offset) => ROOT + 'tasks?assignee=me&completed_since=now&limit=100&opt_fields=' +
        encodeURIComponent(FIELDS) + '&workspace=' + workspace + (offset === null ? '' : '&offset=' + encodeURIComponent(offset));
    // Do not trust next_page URLs. Parse only the exact API origin/path and known query keys,
    // then navigate to our own canonical URL (not the supplied URL).
    const validUrl = (url, workspace, offset, apiRelative = false) => {
        if (typeof url !== 'string' || url.length > 4096) return false;
        // next_page.path is relative to the API root; uri and location are absolute.
        const pattern = apiRelative ? /^\/tasks\?([^#]+)$/ : /^https:\/\/app\.asana\.com\/api\/1\.0\/tasks\?([^#]+)$/;
        const match = pattern.exec(url);
        if (!match) return false;
        const expected = {assignee: 'me', completed_since: 'now', limit: '100', opt_fields: FIELDS, workspace, offset};
        const values = Object.create(null);
        try {
            for (const part of match[1].split('&')) {
                const pair = part.split('=');
                if (pair.length !== 2) return false;
                const key = decodeURIComponent(pair[0]), value = decodeURIComponent(pair[1]);
                if (!Object.prototype.hasOwnProperty.call(expected, key) || Object.prototype.hasOwnProperty.call(values, key)) return false;
                values[key] = value;
            }
        } catch (_) { return false; }
        return Object.keys(expected).every(k => expected[k] === null ? !(k in values) : values[k] === expected[k]);
    };
    const timestamp = value => value === null || value === undefined ||
        (typeof value === 'string' && value.length <= 40 && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:\d\d)$/.test(value) && Number.isFinite(Date.parse(value)));
    const date = value => value === null || value === undefined ||
        (typeof value === 'string' && /^\d{4}-\d\d-\d\d$/.test(value) && Number.isFinite(Date.parse(value)) && new Date(value).toISOString().slice(0, 10) === value);
    try {
        if (Object.prototype.hasOwnProperty.call(window, '__felifoxAsanaResult')) return window.__felifoxAsanaResult;
        if (document.readyState !== 'complete') return null;
        if (list !== 'due' && list !== 'mine') return fail('invalid list');
        let state;
        if (window.name) {
            if (!window.name.startsWith(KEY) || window.name.length * 2 > MAX_BYTES) return fail('invalid pagination state');
            state = JSON.parse(window.name.slice(KEY.length));
            if (!object(state) || state.list !== list || !Number.isFinite(state.started) ||
                state.started > now || now - state.started >= MAX_MS || !id(state.workspace) ||
                !Array.isArray(state.offsets) || state.offsets.length > MAX_PAGES ||
                state.offsets.some(v => v !== null && (typeof v !== 'string' || !v.length || v.length > 1024)) ||
                new Set(state.offsets).size !== state.offsets.length || !Array.isArray(state.rows) ||
                state.rows.length > MAX_PAGES * 100 || !Number.isInteger(state.bytes) || state.bytes < 0 || state.bytes > MAX_BYTES ||
                !validUrl(location.href, state.workspace, state.offsets[state.offsets.length - 1])) return fail('invalid pagination state');
        } else {
            if (location.href !== ME) return fail('unexpected API page');
            state = {list, started: now, workspace: null, offsets: [], rows: [], bytes: 0};
        }
        const text = document.body && document.body.innerText;
        if (typeof text !== 'string' || text.length * 2 + state.bytes > MAX_BYTES) return fail('response size limit exceeded');
        state.bytes += text.length * 2;
        let body;
        try { body = JSON.parse(text); } catch (_) { return fail('invalid API response'); }
        if (!object(body)) return fail('invalid API response');
        if (Object.prototype.hasOwnProperty.call(body, 'errors')) {
            // https://developers.asana.com/docs/errors documents this exact 401 body.
            // Asana's error envelope has human-readable messages, not an auth code.
            // Only the explicit unauthenticated message is evidence of sign-out. In particular,
            // "Forbidden", rate limits and arbitrary messages containing "auth" are not.
            if (Array.isArray(body.errors) && body.errors.length > 0 && body.errors.every(e => object(e) && e.message === 'Not Authorized')) {
                window.name = '';
                return (window.__felifoxAsanaResult = 'signed-out');
            }
            return fail('API request failed');
        }
        const hop = url => {
            if (Date.now() - state.started >= MAX_MS) return fail('refresh time limit exceeded');
            const saved = KEY + JSON.stringify(state);
            if (saved.length * 2 > MAX_BYTES) return fail('pagination state size limit exceeded');
            window.name = saved;
            // Repeated host polling of the departing document must not consume another page.
            window.__felifoxAsanaResult = null;
            location.replace(url);
            return null;
        };
        if (state.workspace === null) {
            if (!object(body.data) || !Array.isArray(body.data.workspaces) ||
                body.data.workspaces.some(w => !object(w) || !id(w.gid))) return fail('invalid workspace response');
            if (!body.data.workspaces.length) {
                window.name = '';
                return (window.__felifoxAsanaResult = []);
            }
            state.workspace = body.data.workspaces[0].gid;
            state.offsets.push(null);
            return hop(tasksUrl(state.workspace, null));
        }
        const validRow = row => object(row) && id(row.gid) && typeof row.name === 'string' && row.name.length <= 4096 &&
            typeof row.permalink_url === 'string' && row.permalink_url.length <= 4096 && /^https:\/\/app\.asana\.com\/[A-Za-z0-9/_-]+(?:[?#][^\s\\]*)?$/.test(row.permalink_url) &&
            timestamp(row.modified_at) && timestamp(row.due_at) && date(row.due_on) &&
            (row.memberships === undefined || row.memberships === null || (Array.isArray(row.memberships) && row.memberships.length <= 100 &&
                row.memberships.every(m => object(m) && (m.project === null || m.project === undefined ||
                    (object(m.project) && (m.project.name === undefined || (typeof m.project.name === 'string' && m.project.name.length <= 4096)))))));
        if (!Array.isArray(body.data) || body.data.length > 100 || !body.data.every(validRow) || !state.rows.every(validRow)) return fail('invalid task response');
        // Keep only fields needed by the final projection, never the entire API response.
        state.rows.push(...body.data.map(t => ({gid: t.gid, name: t.name, permalink_url: t.permalink_url,
            modified_at: t.modified_at, due_at: t.due_at, due_on: t.due_on,
            memberships: (t.memberships || []).slice(0, 1).map(m => ({project: m.project ? {name: m.project.name} : null}))})));
        if (!Object.prototype.hasOwnProperty.call(body, 'next_page')) return fail('missing pagination metadata');
        if (body.next_page !== null) {
            const next = body.next_page;
            if (!object(next) || typeof next.offset !== 'string' || !next.offset.length || next.offset.length > 1024 ||
                state.offsets.includes(next.offset) ||
                (next.uri !== undefined && !validUrl(next.uri, state.workspace, next.offset)) ||
                (next.path !== undefined && !validUrl(next.path, state.workspace, next.offset, true))) return fail('invalid pagination link');
            if (state.offsets.length >= MAX_PAGES) return fail('pagination limit exceeded');
            state.offsets.push(next.offset);
            return hop(tasksUrl(state.workspace, next.offset));
        }
        if (Date.now() - state.started >= MAX_MS) return fail('refresh time limit exceeded');
        // Deduplicate before sorting/filtering/truncating; prefer the newest version of a task.
        const unique = new Map();
        for (const row of state.rows) {
            const previous = unique.get(row.permalink_url);
            if (!previous || (Date.parse(row.modified_at) || 0) > (Date.parse(previous.modified_at) || 0)) unique.set(row.permalink_url, row);
        }
        const due = t => Date.parse(t.due_at || t.due_on);
        const rows = [...unique.values()].filter(t => list !== 'due' || (Number.isFinite(due(t)) && due(t) < state.started + 7 * 864e5));
        rows.sort((a, b) => (list === 'due' ? due(a) - due(b) : (Date.parse(b.modified_at) || 0) - (Date.parse(a.modified_at) || 0)) ||
            a.name.localeCompare(b.name) || a.permalink_url.localeCompare(b.permalink_url));
        const result = rows.slice(0, 50).map(t => ({title: t.name.trim() ? t.name : 'Untitled task', url: t.permalink_url, updated: t.modified_at || '',
            detail: [t.memberships[0]?.project?.name, (t.due_at || t.due_on) && 'due ' +
                new Date(t.due_at || t.due_on + 'T12:00').toLocaleDateString(undefined, {month: 'short', day: 'numeric'})].filter(Boolean).join(' · ')}));
        window.name = '';
        return (window.__felifoxAsanaResult = result);
    } catch (_) { return fail('invalid API response'); }
}
