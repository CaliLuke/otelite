// Overview landing page — five compact, lazy-loaded widgets that answer
// "what's broken / what's active right now?". Each widget links into the
// relevant detail view; the Overview itself stores nothing.
//
// Widgets:
//   1. Recent errors        → /api/traces?status=error
//   2. Active sessions      → /api/sessions
//   3. Model mix            → /api/genai/usage
//   4. p95 latency trend    → /api/genai/latency_series
//   5. Token burn           → /api/genai/cost_series
//
// All five render their loading state immediately; data fetches fire when
// each widget enters the viewport (lazyLoad). On a fresh page load every
// widget is above the fold so they all fetch in parallel — but failures /
// slow widgets don't block the others, and re-rendering after tab switch
// keeps them independent.

class OverviewView {
    constructor(apiClient) {
        this.api = apiClient;
        // Default time window: 1 day. Live DB sample: 1h returned ~10 traces,
        // 1d returned ~880 — 1d gives a useful working set.
        this.windowHours = 24;
    }

    async render() {
        const container = document.getElementById('overview-container');
        if (!container) return;

        container.innerHTML = `
            <div id="ov-anomaly-strip"></div>
            <div class="overview-grid">
                ${this._widgetSlot('errors', 'Recent errors', 'Errored traces in the last 24 h')}
                ${this._widgetSlot('sessions', 'Active sessions', 'Sessions seen in the last 24 h')}
                ${this._widgetSlot('models', 'Model mix', 'Distribution of GenAI requests by model')}
                ${this._widgetSlot('latency', 'p95 latency', 'GenAI request latency (last 24 h)')}
                ${this._widgetSlot('tokens', 'Token burn', 'Total tokens consumed (last 24 h)')}
            </div>
        `;

        // Lazy-load each widget once it's near the viewport.
        const lazy = window.lazyLoad || ((_el, cb) => Promise.resolve().then(cb));
        lazy(document.getElementById('ov-widget-errors'), () => this._loadErrors());
        lazy(document.getElementById('ov-widget-sessions'), () => this._loadSessions());
        lazy(document.getElementById('ov-widget-models'), () => this._loadModels());
        lazy(document.getElementById('ov-widget-latency'), () => this._loadLatency());
        lazy(document.getElementById('ov-widget-tokens'), () => this._loadTokens());

        // Fire the anomaly strip independently — it doesn't block the widgets.
        this._loadAnomalyStrip();
    }

    _widgetSlot(id, title, hint) {
        return `
            <div id="ov-widget-${id}" class="overview-widget" data-widget="${id}">
                <div class="overview-widget-header">
                    <h3>${this._escape(title)}</h3>
                    <span class="overview-widget-hint">${this._escape(hint)}</span>
                </div>
                <div class="overview-widget-body">
                    <div class="loading-state"><span class="spinner-sm"></span> Loading…</div>
                </div>
            </div>
        `;
    }

    _setBody(id, html) {
        const body = document.querySelector(`#ov-widget-${id} .overview-widget-body`);
        if (body) body.innerHTML = html;
    }

    _setError(id, msg) {
        this._setBody(id, `<div class="error-message">${this._escape(msg)}</div>`);
    }

    _windowParams() {
        const end = Date.now();
        const start = end - this.windowHours * 3600000;
        return {
            start_time: start * 1_000_000,
            end_time: end * 1_000_000,
        };
    }

    async _loadErrors() {
        try {
            const params = { limit: 5, status: 'error', ...this._windowParams() };
            const resp = await this.api.getTraces(params);
            const traces = resp.traces || [];
            if (traces.length === 0) {
                this._setBody('errors', '<div class="overview-empty">No errors in the last 24 h.</div>');
                return;
            }
            const total = resp.total ?? traces.length;
            const rows = traces.map(t => {
                const tid = (t.trace_id || '').substring(0, 8);
                const name = this._escape(t.root_span_name || t.service_name || '—');
                const ts = t.start_time ? this._fmtTime(t.start_time) : '';
                return `
                    <li class="overview-list-item" data-trace-id="${this._escape(t.trace_id)}">
                        <span class="ov-mono">${this._escape(tid)}</span>
                        <span class="ov-name">${name}</span>
                        <span class="ov-time">${this._escape(ts)}</span>
                    </li>
                `;
            }).join('');
            this._setBody('errors', `
                <div class="overview-stat-large">${total}</div>
                <ul class="overview-list">${rows}</ul>
                <button class="btn btn-secondary btn-sm overview-cta" data-target="traces-errors">View all in Traces →</button>
            `);
            this._wireRowClicks('errors', 'trace');
            this._wireCta('errors');
        } catch (err) {
            this._setError('errors', `Couldn't load errors: ${err.message}`);
        }
    }

    async _loadSessions() {
        try {
            const params = { limit: 5, ...this._windowParams() };
            const resp = await this.api.getSessions(params);
            const sessions = resp.sessions || [];
            if (sessions.length === 0) {
                this._setBody('sessions', '<div class="overview-empty">No sessions in the last 24 h.</div>');
                return;
            }
            const rows = sessions.map(s => {
                const sid = (s.session_id || '').substring(0, 12);
                const errs = s.error_count > 0
                    ? `<span class="cell-error">${s.error_count} err</span>`
                    : '';
                const models = s.models.length > 0 ? this._escape(s.models[0]) : '—';
                return `
                    <li class="overview-list-item" data-session-id="${this._escape(s.session_id)}">
                        <span class="ov-mono">${this._escape(sid)}…</span>
                        <span class="ov-name">${models} · ${s.interaction_count}×</span>
                        ${errs}
                    </li>
                `;
            }).join('');
            this._setBody('sessions', `
                <div class="overview-stat-large">${resp.total}</div>
                <ul class="overview-list">${rows}</ul>
                <button class="btn btn-secondary btn-sm overview-cta" data-target="sessions">View all sessions →</button>
            `);
            this._wireRowClicks('sessions', 'session');
            this._wireCta('sessions');
        } catch (err) {
            this._setError('sessions', `Couldn't load sessions: ${err.message}`);
        }
    }

    async _loadModels() {
        try {
            const params = this._windowParams();
            const resp = await this.api.getTokenUsage(params);
            const rows = (resp.rows || resp.usage || resp || []);
            const list = Array.isArray(rows) ? rows : [];
            if (list.length === 0) {
                this._setBody('models', '<div class="overview-empty">No GenAI activity yet.</div>');
                return;
            }
            const totals = list.map(r => ({
                model: r.model || r.gen_ai_request_model || 'unknown',
                count: r.request_count || r.requests || r.count || 0,
            })).filter(r => r.count > 0);
            totals.sort((a, b) => b.count - a.count);
            const max = totals[0]?.count || 1;
            const top = totals.slice(0, 5);
            const bars = top.map(r => {
                const pct = Math.max(2, Math.round((r.count / max) * 100));
                return `
                    <div class="ov-bar-row">
                        <span class="ov-bar-label" title="${this._escape(r.model)}">${this._escape(r.model)}</span>
                        <span class="ov-bar-track"><span class="ov-bar-fill" style="width:${pct}%"></span></span>
                        <span class="ov-bar-count">${r.count.toLocaleString()}</span>
                    </div>
                `;
            }).join('');
            this._setBody('models', `
                <div class="overview-bars">${bars}</div>
                <button class="btn btn-secondary btn-sm overview-cta" data-target="analytics">Open Analytics →</button>
            `);
            this._wireCta('models');
        } catch (err) {
            this._setError('models', `Couldn't load model mix: ${err.message}`);
        }
    }

    async _loadLatency() {
        try {
            const params = { bucket_secs: 3600, ...this._windowParams() };
            const resp = await this.api.getLatencySeries(params);
            const points = (resp.points || resp.buckets || resp || []);
            const series = Array.isArray(points) ? points : [];
            if (series.length === 0) {
                this._setBody('latency', '<div class="overview-empty">No latency data yet.</div>');
                return;
            }
            const values = series.map(p => p.p95_ms ?? p.p95 ?? p.value ?? 0).filter(v => v > 0);
            if (values.length === 0) {
                this._setBody('latency', '<div class="overview-empty">No latency data yet.</div>');
                return;
            }
            const peak = Math.max(...values);
            const last = values[values.length - 1];
            this._setBody('latency', `
                <div class="overview-stat-large">${(last / 1000).toFixed(2)}s</div>
                <div class="overview-stat-sub">peak ${(peak / 1000).toFixed(2)}s · ${values.length} buckets</div>
                ${this._sparkline(values)}
                <button class="btn btn-secondary btn-sm overview-cta" data-target="analytics">Open Analytics →</button>
            `);
            this._wireCta('latency');
        } catch (err) {
            this._setError('latency', `Couldn't load latency: ${err.message}`);
        }
    }

    async _loadTokens() {
        try {
            const params = { bucket_secs: 3600, ...this._windowParams() };
            const resp = await this.api.getCostSeries(params);
            const points = (resp.points || resp.buckets || resp || []);
            const series = Array.isArray(points) ? points : [];
            if (series.length === 0) {
                this._setBody('tokens', '<div class="overview-empty">No token usage yet.</div>');
                return;
            }
            const totals = series.map(p =>
                (p.input_tokens || 0) + (p.output_tokens || 0)
            );
            const sum = totals.reduce((a, b) => a + b, 0);
            this._setBody('tokens', `
                <div class="overview-stat-large">${sum.toLocaleString()}</div>
                <div class="overview-stat-sub">tokens across ${totals.length} hourly buckets</div>
                ${this._sparkline(totals)}
                <button class="btn btn-secondary btn-sm overview-cta" data-target="analytics">Open Analytics →</button>
            `);
            this._wireCta('tokens');
        } catch (err) {
            this._setError('tokens', `Couldn't load token burn: ${err.message}`);
        }
    }

    /**
     * Anomaly alert strip: surfaces the most actionable issues from the last
     * 24 h without requiring the user to open Analytics. Fires two parallel
     * fetches (top spans + tool usage) and builds a compact horizontal list of
     * alert chips. Silent on failure — never blocks the main widgets.
     */
    async _loadAnomalyStrip() {
        const strip = document.getElementById('ov-anomaly-strip');
        if (!strip) return;
        try {
            const params = this._windowParams();
            const [topSpans, toolUsage, latencyStats] = await Promise.all([
                this.api.getTopSpans({ ...params, limit: 5, sort_by: 'duration' }).catch(() => []),
                this.api.getToolUsage({ ...params, limit: 30 }).catch(() => []),
                this.api.getLatencyStats(params).catch(() => []),
            ]);

            const alerts = [];

            // 1. Any span >5 min is an outlier worth surfacing immediately.
            const slow = (topSpans || []).filter(s => (s.duration || 0) / 1e9 > 300);
            if (slow.length > 0) {
                const worst = slow[0];
                const durS = Math.round((worst.duration || 0) / 1e9);
                const tid = String(worst.trace_id || '').slice(0, 8);
                alerts.push({
                    level: 'warn',
                    icon: '⏱',
                    text: `Slow call: ${durS}s on ${worst.model || '?'} — ${tid}`,
                    action: worst.trace_id ? () => window.app.navigateToTrace(worst.trace_id) : null,
                    actionLabel: 'View trace',
                });
            }

            // 2. Any COLD-start span (expensive full context rebuild).
            const coldSpans = (topSpans || []).filter(s =>
                (s.cache_read_tokens || 0) === 0 && (s.cache_creation_tokens || 0) > 50_000
            );
            if (coldSpans.length > 0) {
                alerts.push({
                    level: 'warn',
                    icon: '❄',
                    text: `Cold context: ${coldSpans.length} request${coldSpans.length > 1 ? 's' : ''} rebuilt full context (no cache reads)`,
                    action: () => window.app.switchView('analytics'),
                    actionLabel: 'Cost →',
                });
            }

            // 3. Tools consuming outsized total wall-clock time (>5 min total).
            const slowTools = (toolUsage || [])
                .map(t => ({ ...t, totalMs: t.total_duration_ms || 0 }))
                .filter(t => t.totalMs > 300_000)
                .sort((a, b) => b.totalMs - a.totalMs)
                .slice(0, 2);
            for (const t of slowTools) {
                const mins = (t.totalMs / 60_000).toFixed(1);
                const name = String(t.tool_name || '?').replace('mcp__', '').replace(/__.*/, '…');
                alerts.push({
                    level: 'info',
                    icon: '🔧',
                    text: `${name}: ${mins} min total across ${t.count} calls`,
                    action: () => window.app.switchView('analytics'),
                    actionLabel: 'Behavior →',
                });
            }

            // 4. High output/input ratio — session is generating more than it reads.
            for (const s of (latencyStats || [])) {
                if ((s.output_input_ratio_p95 || 0) > 500) {
                    alerts.push({
                        level: 'info',
                        icon: '📝',
                        text: `${s.model}: p95 output/input ratio ${Math.round(s.output_input_ratio_p95)}× — responses are very long`,
                        action: () => window.app.switchView('analytics'),
                        actionLabel: 'Latency →',
                    });
                    break; // one message is enough
                }
            }

            if (alerts.length === 0) {
                strip.innerHTML = '';
                return;
            }

            const chips = alerts.map(a => {
                const actionHtml = a.action
                    ? `<button class="ov-anomaly-action">${this._escape(a.actionLabel)}</button>`
                    : '';
                return `<div class="ov-anomaly-chip ov-anomaly-${a.level}" data-idx="${alerts.indexOf(a)}">
                    <span class="ov-anomaly-icon">${a.icon}</span>
                    <span class="ov-anomaly-text">${this._escape(a.text)}</span>
                    ${actionHtml}
                </div>`;
            }).join('');

            strip.innerHTML = `<div class="ov-anomaly-strip-inner">${chips}</div>`;

            // Wire action buttons.
            strip.querySelectorAll('[data-idx]').forEach(chip => {
                const btn = chip.querySelector('.ov-anomaly-action');
                const idx = parseInt(chip.dataset.idx, 10);
                if (btn && alerts[idx]?.action) {
                    btn.addEventListener('click', (e) => {
                        e.stopPropagation();
                        alerts[idx].action();
                    });
                }
            });
        } catch (_e) {
            // Anomaly strip is best-effort — never show an error here.
        }
    }

    _sparkline(values) {
        if (!values || values.length === 0) return '';
        const w = 200, h = 32;
        const max = Math.max(...values, 1);
        const min = Math.min(...values, 0);
        const range = max - min || 1;
        const step = values.length > 1 ? w / (values.length - 1) : 0;
        const pts = values.map((v, i) => {
            const x = i * step;
            const y = h - ((v - min) / range) * h;
            return `${x.toFixed(1)},${y.toFixed(1)}`;
        }).join(' ');
        return `
            <svg class="overview-sparkline" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none">
                <polyline fill="none" stroke="currentColor" stroke-width="1.5" points="${pts}" />
            </svg>
        `;
    }

    _wireRowClicks(widgetId, kind) {
        const items = document.querySelectorAll(`#ov-widget-${widgetId} .overview-list-item`);
        items.forEach(li => {
            li.style.cursor = 'pointer';
            li.addEventListener('click', () => {
                if (kind === 'trace' && li.dataset.traceId) {
                    window.app.navigateToTrace(li.dataset.traceId);
                } else if (kind === 'session' && li.dataset.sessionId) {
                    if (window.app.views.traces &&
                        typeof window.app.views.traces.openSessionDiagnoseModal === 'function') {
                        if (!window.app.renderedViews.has('traces')) {
                            window.app.views.traces.render();
                            window.app.renderedViews.add('traces');
                        }
                        window.app.views.traces.openSessionDiagnoseModal(li.dataset.sessionId);
                    } else {
                        window.app.navigateToTracesBySession(li.dataset.sessionId);
                    }
                }
            });
        });
    }

    _wireCta(widgetId) {
        const btn = document.querySelector(`#ov-widget-${widgetId} .overview-cta`);
        if (!btn) return;
        btn.addEventListener('click', () => {
            const target = btn.dataset.target;
            if (target === 'traces-errors') {
                window.app.switchView('traces');
                const tv = window.app.views.traces;
                if (tv && tv.filters) {
                    tv.filters.status = 'error';
                    if (typeof tv.loadTraces === 'function') tv.loadTraces();
                }
            } else if (target === 'sessions' || target === 'analytics' || target === 'traces') {
                window.app.switchView(target);
            }
        });
    }

    _fmtTime(ns) {
        const d = new Date(ns / 1_000_000);
        const p = n => String(n).padStart(2, '0');
        return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
    }

    _escape(s) {
        const div = document.createElement('div');
        div.textContent = String(s ?? '');
        return div.innerHTML;
    }
}

window.OverviewView = OverviewView;
export { OverviewView };
