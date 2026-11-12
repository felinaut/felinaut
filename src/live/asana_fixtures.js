function (extractors) {
    'use strict';
    let assertions = 0;
    function check(ok, label) { assertions++; if (!ok) throw new Error('Fixture failed: ' + label); }
    const NOW = Date.parse('2026-11-12T12:00:00Z');
    const realNow = Date.now;
    Date.now = () => NOW;
    const ME = 'https://app.asana.com/api/1.0/users/me?opt_fields=workspaces';
    const workspace = {data: {workspaces: [{gid: '123'}, {gid: '456'}]}};
    const task = (n, extra) => Object.assign({gid: String(n), name: 'Task ' + n,
        permalink_url: 'https://app.asana.com/0/123/' + n, modified_at: '2026-11-12T10:00:00Z',
        due_on: null, due_at: null, memberships: []}, extra || {});
    const page = (rows, offset) => ({data: rows, next_page: offset === undefined ? null : {offset}});
    function session(mode) {
        let href = ME, saved = '', cached = {}, body = '', clock = NOW, hops = 0;
        const location = {get href() {return href;}, replace(url) {href = url; hops++;}};
        const document = {readyState: 'complete', body: {get innerText() {return body;}}};
        const evaluate = new Function('window', 'document', 'location', 'return ' + extractors[mode === 'due' ? 0 : 1]);
        return {
            load(value, url) {
                if (url !== undefined) href = url;
                body = typeof value === 'string' ? value : JSON.stringify(value);
                cached = {get name() {return saved;}, set name(v) {saved = v;}};
                return this.poll();
            },
            poll() {Date.now = () => clock; return evaluate(cached, document, location);},
            advance(ms) {clock += ms;},
            corrupt(value) {saved = value;},
            get href() {return href;}, get saved() {return saved;}, get hops() {return hops;}
        };
    }
    function error(result, label) {
        check(result !== null && !Array.isArray(result) && typeof result === 'object' &&
            Object.keys(result).join() === 'error' && /^Asana [A-Za-z ]+$/.test(result.error), label);
    }
    function started(mode) {
        const s = session(mode);
        check(s.load(workspace) === null && s.href.includes('workspace=123'), 'first workspace hop');
        return s;
    }
    try {
        for (const mode of ['due', 'mine']) {
            let s = started(mode);
            const first = Array.from({length: 100}, (_, i) => task(i + 100));
            check(s.load(page(first, 'opaque+/= next')) === null, mode + ' first page incomplete');
            check(s.href.includes('offset=opaque%2B%2F%3D%20next'), 'offset encoded');
            const before = s.hops;
            check(s.poll() === null && s.hops === before, 'departing document polled once');
            const result = s.load(page([task(1, {due_on: '2026-11-13', modified_at: '2026-11-12T11:00:00Z'}),
                task(2, {due_on: '2026-11-10', modified_at: '2026-11-12T12:00:00Z'})]));
            check(Array.isArray(result) && result.length === (mode === 'due' ? 2 : 50), mode + ' final list');
            check(result[0].title === 'Task 2' && result[1].title === 'Task 1', mode + ' global sort before truncation');
            check(s.saved === '', 'transient state cleared');
            check(s.poll() === result, 'terminal result stable');

            s = started(mode);
            check(s.load(page([task(1, {due_on: '2026-11-15'})], 'next')) === null, 'duplicate first page');
            const dedup = s.load(page([task(1, {name: 'New title', due_on: '2026-11-14', modified_at: '2026-11-12T11:00:00Z'}),
                task(2, {due_on: '2026-11-13', modified_at: '2026-11-12T12:00:00Z'})]));
            check(dedup.length === 2 && dedup[0].title === 'Task 2' && dedup[1].title === 'New title', mode + ' dedup newest and sort');
            s = started(mode);
            check(s.load(page([task(1, {name: '', due_on: '2026-11-14'}),
                task(2, {name: ' \t ', due_on: '2026-11-15'})], 'next')) === null, mode + ' unnamed tasks survive pagination');
            const untitled = s.load(page([task(3, {due_on: '2026-11-13', modified_at: '2026-11-12T12:00:00Z'})]));
            check(Array.isArray(untitled) && untitled.length === 3 && untitled[0].title === 'Task 3' &&
                untitled.slice(1).every(t => t.title === 'Untitled task'), mode + ' unnamed tasks do not reject named due task');
            s = started(mode);
            check(s.load(page([])).length === 0, mode + ' successful empty tasks');
            s = session(mode);
            check(s.load({data: {workspaces: []}}).length === 0 && s.hops === 0, mode + ' empty workspaces');
        }
        let s = started('due');
        const dates = s.load(page([task(1, {due_on: '2026-11-01'}), task(2), task(3, {due_at: '2026-11-19T12:00:00Z'}),
            task(4, {due_at: '2026-11-19T11:59:59Z'})]));
        check(dates.length === 2 && dates[0].title === 'Task 1' && dates[1].title === 'Task 4', 'overdue/missing/exclusive week boundary');

        for (const value of ['not json', 'null', '[]', '{}', {data: {}}, {data: {workspaces: null}}, {data: {workspaces: [{gid: '../evil'}]}}]) {
            error(session('mine').load(value), 'malformed workspace');
        }
        const invalidTasks = [null, {}, [null], [task(1, {gid: 1})], [task(1, {name: null})], [task(1, {name: 42})],
            [task(1, {permalink_url: 'https://evil.invalid/private'})], [task(1, {permalink_url: 'https://app.asana.com.evil.invalid/1'})],
            [task(1, {due_on: '2026-02-30'})], [task(1, {due_at: 'bad'})], [task(1, {modified_at: 5})],
            [task(1, {memberships: [{project: {name: 42}}]})], Array.from({length: 101}, (_, i) => task(i))];
        for (const rows of invalidTasks) error(started('due').load({data: rows, next_page: null}), 'malformed tasks');
        error(started('mine').load({data: []}), 'pagination metadata required');
        error(started('mine').load('truncated {'), 'malformed body');
        s = started('mine');
        const missing = s.load(page([task(1, {modified_at: null, memberships: [{project: null}]})]));
        check(missing.length === 1 && missing[0].updated === '', 'missing timestamps and null project supported');

        const url = 'https://app.asana.com/api/1.0/tasks?assignee=me&completed_since=now&limit=100&opt_fields=' +
            encodeURIComponent('name,permalink_url,modified_at,due_on,due_at,memberships.project.name') + '&workspace=123&offset=next';
        const path = url.replace('https://app.asana.com/api/1.0', '');
        for (const next of [{offset: 'next', uri: url, path}, {offset: 'next', path}, {offset: 'next', uri: url}]) {
            s = started('mine');
            check(s.load({data: [], next_page: next}) === null, 'valid absolute URI/API-relative path');
            check(s.href === url, 'pagination uses local canonical URL');
            check(s.load(page([])).length === 0, 'valid linked page completes');
        }
        for (const badPath of [url, path.replace('/tasks?', '/users/me?'), '/api/1.0' + path,
            path.replace('workspace=123', 'workspace=456'), path.replace('offset=next', 'offset=other'),
            path + '#fragment', path + '&workspace=123', path + '&extra=1', path.replace('offset=next', 'offset=%ZZ')]) {
            error(started('mine').load({data: [], next_page: {offset: 'next', uri: url, path: badPath}}), 'invalid API-relative path even with valid URI');
        }
        const invalidLinks = [
            url.replace('https:', 'http:'), url.replace('app.asana.com', 'evil.invalid'),
            url.replace('app.asana.com', 'app.asana.com.evil.invalid'), url.replace('app.asana.com', 'user@app.asana.com'),
            url.replace('app.asana.com', 'app.asana.com:443'), url.replace('/tasks?', '/users/me?'),
            url.replace('workspace=123', 'workspace=456'), url.replace('offset=next', 'offset=other'),
            url.replace('limit=100', 'limit=999'), url + '&workspace=123', url + '#fragment',
            url + '&extra=1', url.replace('assignee=me', 'assignee=other'), '//app.asana.com/api/1.0/tasks?offset=next',
            url.replace('offset=next', 'offset=%ZZ')
        ];
        invalidLinks.push(path, '/api/1.0' + path);
        for (const uri of invalidLinks) error(started('mine').load({data: [task(1)], next_page: {offset: 'next', uri}}), 'untrusted pagination URL');
        for (const next of [{}, {offset: ''}, {offset: 3}, {offset: 'x'.repeat(1025)}, {offset: 'next', path: '/wrong'}]) {
            error(started('mine').load({data: [], next_page: next}), 'invalid offset/path');
        }
        s = started('mine');
        check(s.load(page([task(1)], 'same')) === null, 'cycle first hop');
        error(s.load(page([task(2)], 'same')), 'cyclic offsets fail without partial result');
        s = started('mine');
        for (let i = 1; i < 8; i++) check(s.load(page([task(i)], 'page' + i)) === null, 'bounded page traversal');
        error(s.load(page([task(8)], 'page8')), 'page limit no partial success');
        s = started('mine');
        for (let i = 1; i < 8; i++) check(s.load(page([], 'page' + i)) === null, 'maximum successful traversal');
        check(s.load(page([task(9)])).length === 1 && s.hops === 8, 'eight pages may complete');
        s = started('mine');
        s.advance(18000);
        error(s.load(page([task(1)])), 'deadline');
        error(session('mine').load(' '.repeat(1024 * 1024 + 1)), 'response byte bound');
        s = started('mine');
        const large = page([], 'next'); large.padding = ' '.repeat(600000);
        check(s.load(large) === null, 'first large response');
        error(s.load(Object.assign(page([]), {padding: ' '.repeat(500000)})), 'cumulative byte bound');
        s = started('mine'); s.corrupt('untrusted state'); error(s.load(page([])), 'bad transient state');
        s = started('mine'); error(s.load(page([]), 'https://evil.invalid/api/1.0/tasks'), 'unexpected loaded location');

        for (const message of ['Forbidden', 'Rate limit enforced', 'Internal Server Error', 'Authentication service unavailable',
            'Not Authorized: private account', 'not authorized', 'Not Found', 'private server detail']) {
            for (const afterPage of [false, true]) {
                s = started('mine');
                if (afterPage) check(s.load(page([task(1)], 'next')) === null, 'error after valid page setup');
                const answer = s.load({errors: [{message}]});
                error(answer, 'non-auth error preserves cached list');
                check(JSON.stringify(answer).indexOf(message) === -1 && s.saved === '', 'no raw API details');
            }
        }
        for (const errors of [[], null, {}, [{message: 401}], [{message: 'Not Authorized'}, {message: 'Forbidden'}]]) {
            error(started('mine').load({errors}), 'malformed or mixed errors not auth');
        }
        for (const afterPage of [false, true]) {
            s = started('mine');
            if (afterPage) s.load(page([task(1)], 'next'));
            check(s.load({errors: [{message: 'Not Authorized'}]}) === 'signed-out' && s.saved === '', 'explicit Asana auth error');
        }
        check(session('mine').load({errors: [{message: 'Not Authorized'}]}) === 'signed-out', 'workspace auth failure');
        s = started('mine'); s.load(page([task(1)], 'next')); error(s.load('{bad'), 'malformed later page no partial success');
        return 'Asana offline fixtures passed (' + assertions + ' assertions)';
    } finally {Date.now = realNow;}
}
