// Global filter bar (#135)
//
// Shared by the GenAI Analytics and Sessions views. Holds the five filter
// dimensions (agent, model, provider, project, session), persists them in
// the URL hash query (e.g. `#/analytics?agent=claude&model=x`) so links
// round-trip, and feeds them into ApiClient.globalFilters.
//
// Each genai endpoint echoes the dimensions it actually applied in
// `filters_applied`; the bar greys out the dimensions no loaded endpoint
// applied, so users can see at a glance which filters a section honours.

const FILTER_DIMENSIONS = ['agent', 'model', 'provider', 'project', 'session'];
const AGENT_FAMILIES = ['claude', 'opencode', 'codex'];

function parseHashQuery() {
    const hash = window.location.hash || '';
    const qIndex = hash.indexOf('?');
    const out = {};
    if (qIndex < 0) return out;
    new URLSearchParams(hash.slice(qIndex + 1)).forEach((value, key) => {
        if (FILTER_DIMENSIONS.includes(key) && value) out[key] = value;
    });
    return out;
}

/**
 * Write the filter dimensions into the current view's hash query.
 * The view path (`#/analytics`, `#/sessions`) is preserved.
 */
function writeHashQuery(filters) {
    const hash = window.location.hash || '#/';
    const path = hash.split('?')[0];
    const params = new URLSearchParams();
    FILTER_DIMENSIONS.forEach(key => {
        if (filters[key]) params.set(key, filters[key]);
    });
    const qs = params.toString();
    const next = qs ? `${path}?${qs}` : path;
    const url = window.location.pathname + window.location.search + next;
    window.history.replaceState(null, '', url);
}

/**
 * Build the filter bar DOM.
 *
 * @param {HTMLElement} mount   - container element to fill
 * @param {object}      state   - current {agent, model, ...} values
 * @param {object}      opts
 *   opts.onChange(filters)     - called with the full state after any change
 *   opts.modelOptions          - string[] options for the model select
 *   opts.providerOptions       - string[] options for the provider select
 * @returns {{ bar: HTMLElement, grey: (applied: string[]) => void }}
 */
function renderFilterBar(mount, state, opts = {}) {
    mount.innerHTML = '';
    const bar = document.createElement('div');
    bar.className = 'filter-bar';
    bar.id = 'global-filter-bar';

    const makeSelect = (dim, label, values) => {
        const sel = document.createElement('select');
        sel.className = `filter-select fb-dim fb-dim-${dim}`;
        sel.title = label;
        const all = document.createElement('option');
        all.value = '';
        all.textContent = label;
        sel.appendChild(all);
        (values || []).forEach(v => {
            const o = document.createElement('option');
            o.value = v;
            o.textContent = v;
            sel.appendChild(o);
        });
        sel.value = state[dim] || '';
        sel.addEventListener('change', () => {
            state[dim] = sel.value || null;
            syncInputs();
            opts.onChange(state);
        });
        return sel;
    };

    const makeInput = (dim, label, placeholder) => {
        const inp = document.createElement('input');
        inp.type = 'text';
        inp.className = `filter-input fb-dim fb-dim-${dim}`;
        inp.placeholder = placeholder;
        inp.autocomplete = 'off';
        inp.value = state[dim] || '';
        inp.addEventListener('change', () => {
            state[dim] = inp.value.trim() || null;
            syncInputs();
            opts.onChange(state);
        });
        return inp;
    };

    bar.appendChild(makeSelect('agent', 'All agents', AGENT_FAMILIES));
    bar.appendChild(makeSelect('model', 'All models', opts.modelOptions));
    bar.appendChild(makeSelect('provider', 'All providers', opts.providerOptions));
    bar.appendChild(makeInput('project', 'project', 'project id'));
    bar.appendChild(makeInput('session', 'session', 'session id'));

    const clear = document.createElement('button');
    clear.className = 'btn-icon';
    clear.title = 'Clear filters (keeps the current time window)';
    clear.textContent = 'Clear';
    clear.addEventListener('click', () => {
        FILTER_DIMENSIONS.forEach(d => { state[d] = null; });
        syncInputs();
        opts.onChange(state);
    });
    bar.appendChild(clear);

    const syncInputs = () => {
        bar.querySelectorAll('.fb-dim').forEach(el => {
            const dim = el.className.match(/fb-dim-(\w+)/)[1];
            if (el.value !== (state[dim] || '')) el.value = state[dim] || '';
        });
    };

    mount.appendChild(bar);

    /**
     * Grey out dimensions that no loaded endpoint applied.
     * `applied` is the union of `filters_applied` seen so far in this view.
     */
    const grey = (applied) => {
        const appliedSet = new Set(applied);
        bar.querySelectorAll('.fb-dim').forEach(el => {
            const dim = el.className.match(/fb-dim-(\w+)/)[1];
            el.classList.toggle('fb-inactive', !appliedSet.has(dim));
        });
    };

    return { bar, grey };
}

// Exposed via window like the other view modules (app.js reads window.*View).
window.parseHashQuery = parseHashQuery;
window.writeHashQuery = writeHashQuery;
window.renderFilterBar = renderFilterBar;
window.FILTER_DIMENSIONS = FILTER_DIMENSIONS;
window.AGENT_FAMILIES = AGENT_FAMILIES;
