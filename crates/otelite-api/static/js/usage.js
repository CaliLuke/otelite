// GenAI token usage view

import { api } from './api.js';

function formatTs(date) {
    const p = n => String(n).padStart(2, '0');
    return `${date.getFullYear()}-${p(date.getMonth()+1)}-${p(date.getDate())} ` +
           `${p(date.getHours())}:${p(date.getMinutes())}:${p(date.getSeconds())}`;
}

// Claude pricing in USD per 1M tokens. Mirrors CLAUDE_PRICING in logs.js.
const CLAUDE_PRICING = {
    'opus':   { input: 15, output: 75, cache_5m: 18.75, cache_1h: 30, cache_read: 1.5 },
    'sonnet': { input: 3,  output: 15, cache_5m: 3.75,  cache_1h: 6,  cache_read: 0.3 },
    'haiku':  { input: 1,  output: 5,  cache_5m: 1.25,  cache_1h: 2,  cache_read: 0.1 },
};

function pricingForModel(model) {
    if (!model) return null;
    const m = String(model).toLowerCase();
    if (m.includes('opus')) return CLAUDE_PRICING.opus;
    if (m.includes('sonnet')) return CLAUDE_PRICING.sonnet;
    if (m.includes('haiku')) return CLAUDE_PRICING.haiku;
    return null;
}

function computeCost(model, usage) {
    const p = pricingForModel(model);
    if (!p || !usage) return null;
    const c5 = usage.cache_creation?.ephemeral_5m_input_tokens ?? 0;
    const c1 = usage.cache_creation?.ephemeral_1h_input_tokens ?? 0;
    const cc = (usage.cache_creation_input_tokens ?? 0) - c5 - c1;
    const cr = usage.cache_read_input_tokens ?? 0;
    const input = usage.input_tokens ?? 0;
    const output = usage.output_tokens ?? 0;
    return (
        input * p.input +
        output * p.output +
        c5 * p.cache_5m +
        c1 * p.cache_1h +
        Math.max(cc, 0) * p.cache_5m +
        cr * p.cache_read
    ) / 1_000_000;
}

class UsageView {
    constructor(apiClient) {
        this.api = apiClient;
        this.refreshInterval = null;
        this.trStart = null;
        this.trEnd = null;
        this.trWindowHours = null;
    }

    async render() {
        const container = document.getElementById('usage-container');
        if (!container) return;

        container.innerHTML = `
            <div class="view-header">
                <h2>GenAI Usage</h2>
            </div>
            <div class="filters">
                <div class="time-range-bar">
                    <button class="btn-icon" id="tr-prev-usage" title="Previous window">&#8592;</button>
                    <input type="text" id="tr-start-usage" class="filter-input tr-datetime" placeholder="YYYY-MM-DD HH:MM" autocomplete="off">
                    <span class="tr-sep">–</span>
                    <input type="text" id="tr-end-usage" class="filter-input tr-datetime" placeholder="YYYY-MM-DD HH:MM" autocomplete="off">
                    <button class="btn-icon" id="tr-next-usage" title="Next window">&#8594;</button>
                    <button class="btn-icon" id="tr-now-usage" title="Jump to now">Now</button>
                    <select id="tr-preset-usage" class="filter-select tr-preset">
                        <option value="">All time</option>
                        <option value="1">1 hr</option>
                        <option value="6">6 hr</option>
                        <option value="24">24 hr</option>
                        <option value="168">7 days</option>
                    </select>
                </div>
            </div>
            <div id="usage-data-container"></div>
        `;

        this._attachTimeRangeListeners();
        await this._loadAndRender();

        if (!this.refreshInterval) {
            this.refreshInterval = setInterval(() => this._loadAndRender(), 30000);
        }
    }

    _attachTimeRangeListeners() {
        document.getElementById('tr-preset-usage').addEventListener('change', (e) => {
            const hours = e.target.value ? parseFloat(e.target.value) : null;
            if (hours !== null) {
                const now = new Date();
                this.trEnd = now;
                this.trStart = new Date(now.getTime() - hours * 3600000);
                this.trWindowHours = hours;
                this._syncDateInputs();
            } else {
                this.trStart = null;
                this.trEnd = null;
                this.trWindowHours = null;
                this._syncDateInputs();
            }
            this._loadAndRender();
        });

        document.getElementById('tr-start-usage').addEventListener('change', () => this._onDateInputChange());
        document.getElementById('tr-end-usage').addEventListener('change', () => this._onDateInputChange());

        document.getElementById('tr-prev-usage').addEventListener('click', () => {
            const windowMs = (this.trWindowHours || 1) * 3600000;
            const end = (this.trEnd || new Date()).getTime() - windowMs;
            const start = (this.trStart ? this.trStart.getTime() : end - windowMs) - windowMs;
            this.trEnd = new Date(end);
            this.trStart = new Date(start);
            this._syncDateInputs();
            document.getElementById('tr-preset-usage').value = '';
            this._loadAndRender();
        });

        document.getElementById('tr-next-usage').addEventListener('click', () => {
            const now = Date.now();
            const windowMs = (this.trWindowHours || 1) * 3600000;
            let end = (this.trEnd || new Date()).getTime() + windowMs;
            if (end > now) end = now;
            this.trEnd = new Date(end);
            this.trStart = new Date(end - windowMs);
            this._syncDateInputs();
            document.getElementById('tr-preset-usage').value = '';
            this._loadAndRender();
        });

        document.getElementById('tr-now-usage').addEventListener('click', () => {
            const now = new Date();
            const windowMs = (this.trWindowHours || 1) * 3600000;
            this.trEnd = now;
            this.trStart = new Date(now.getTime() - windowMs);
            this._syncDateInputs();
            document.getElementById('tr-preset-usage').value = '';
            this._loadAndRender();
        });
    }

    _syncDateInputs() {
        const startEl = document.getElementById('tr-start-usage');
        const endEl = document.getElementById('tr-end-usage');
        if (startEl) startEl.value = this.trStart ? this._toDatetimeLocal(this.trStart) : '';
        if (endEl) endEl.value = this.trEnd ? this._toDatetimeLocal(this.trEnd) : '';
    }

    _toDatetimeLocal(date) {
        const pad = n => String(n).padStart(2, '0');
        return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
    }

    _parseDatetimeInput(str) {
        if (!str) return null;
        const normalized = str.trim().replace('T', ' ');
        const m = normalized.match(/^(\d{4}-\d{2}-\d{2})(?:\s+(\d{2}:\d{2}))?$/);
        if (!m) return null;
        return new Date(`${m[1]}T${m[2] || '00:00'}`);
    }

    _onDateInputChange() {
        const startEl = document.getElementById('tr-start-usage');
        const endEl = document.getElementById('tr-end-usage');
        this.trStart = this._parseDatetimeInput(startEl ? startEl.value : '');
        this.trEnd = this._parseDatetimeInput(endEl ? endEl.value : '');
        if (this.trStart && this.trEnd) {
            this.trWindowHours = (this.trEnd.getTime() - this.trStart.getTime()) / 3600000;
        }
        const presetEl = document.getElementById('tr-preset-usage');
        if (presetEl) presetEl.value = '';
        this._loadAndRender();
    }

    _chooseBucket() {
        const hours = this.trWindowHours;
        if (hours == null) return 86400;
        if (hours <= 1) return 60;
        if (hours <= 6) return 300;
        if (hours <= 24) return 900;
        if (hours <= 168) return 3600;
        return 86400;
    }

    async _loadAndRender() {
        const dataContainer = document.getElementById('usage-data-container');
        if (!dataContainer) return;

        try {
            const params = {};
            if (this.trStart !== null) {
                params.start_time = this.trStart.getTime() * 1_000_000;
                params.end_time = (this.trEnd || new Date()).getTime() * 1_000_000;
            }
            const bucket = this._chooseBucket();
            const [summary, costSeries, topSpans, finishReasons] = await Promise.all([
                this.api.getTokenUsage(params),
                this.api.getCostSeries({ ...params, bucket }),
                this.api.getTopSpans({ ...params, limit: 20 }),
                this.api.getFinishReasons(params),
            ]);
            dataContainer.innerHTML = this._buildHtml(summary, costSeries, topSpans, finishReasons, bucket);
        } catch (err) {
            dataContainer.innerHTML = `<div class="empty-state"><p>Failed to load usage data</p><p class="empty-state-hint">${err.message}</p></div>`;
        }
    }

    _buildHtml(data, costSeries, topSpans, finishReasons, bucket) {
        const { summary, by_model, by_system } = data;

        if (summary.total_requests === 0) {
            return `<div class="empty-state">
                <p>No GenAI data yet</p>
                <p class="empty-state-hint">
                    Instrument your LLM application with the OpenAI or Anthropic OTel SDK and point it at
                    <strong>http://localhost:4318</strong>. Token usage will appear here once spans with
                    <code>gen_ai.system</code> attributes arrive.
                </p>
            </div>`;
        }

        const fmt = n => Number(n).toLocaleString();

        const cacheRead = summary.total_cache_read_tokens ?? 0;
        const cacheCreate = summary.total_cache_creation_tokens ?? 0;
        const totalInput = summary.total_input_tokens ?? 0;
        const cacheDenom = cacheRead + cacheCreate + totalInput;
        const cachePct = cacheDenom > 0 ? (cacheRead / cacheDenom) * 100 : 0;

        const reasons = Array.isArray(finishReasons) ? finishReasons : [];
        const truncCount = reasons
            .filter(r => String(r.reason || '').toLowerCase() === 'max_tokens')
            .reduce((acc, r) => acc + (r.count || 0), 0);
        const totalCount = reasons.reduce((acc, r) => acc + (r.count || 0), 0);
        const truncPct = totalCount > 0 ? (truncCount / totalCount) * 100 : 0;

        const summaryCards = `
            <div class="usage-summary-cards">
                <div class="usage-card">
                    <div class="usage-card-label">Input tokens</div>
                    <div class="usage-card-value">${fmt(totalInput)}</div>
                </div>
                <div class="usage-card">
                    <div class="usage-card-label">Output tokens</div>
                    <div class="usage-card-value">${fmt(summary.total_output_tokens ?? 0)}</div>
                </div>
                <div class="usage-card">
                    <div class="usage-card-label">Requests</div>
                    <div class="usage-card-value">${fmt(summary.total_requests)}</div>
                </div>
                <div class="usage-gauge-card">
                    <div class="usage-card-label">Cache hit rate</div>
                    <div class="usage-card-value">${cachePct.toFixed(1)}%</div>
                    <div class="gauge-bar"><div class="gauge-fill" style="width:${cachePct.toFixed(2)}%"></div></div>
                    <div class="gauge-hint">${fmt(cacheRead)} / ${fmt(cacheDenom)} tokens served from cache</div>
                </div>
                <div class="usage-gauge-card">
                    <div class="usage-card-label">Truncation rate</div>
                    <div class="usage-card-value">${truncPct.toFixed(1)}%</div>
                    <div class="gauge-bar"><div class="gauge-fill ${truncPct > 0 ? 'gauge-fill-warning' : ''}" style="width:${truncPct.toFixed(2)}%"></div></div>
                    <div class="gauge-hint">${fmt(truncCount)} / ${fmt(totalCount)} responses hit max_tokens</div>
                </div>
            </div>`;

        const costChart = this._buildCostChart(costSeries || [], bucket);
        const topSpansSection = this._buildTopSpans(topSpans || []);
        const finishReasonsSection = this._buildFinishReasons(reasons);

        const modelRows = by_model.map(m => `
            <tr>
                <td>${this._esc(m.model)}</td>
                <td>${fmt(m.requests)}</td>
                <td>${fmt(m.input_tokens)}</td>
                <td>${fmt(m.output_tokens)}</td>
                <td>${fmt(m.input_tokens + m.output_tokens)}</td>
            </tr>`).join('');

        const modelTable = `
            <h3>By model</h3>
            <table class="data-table">
                <thead><tr>
                    <th>Model</th><th>Requests</th><th>Input tokens</th><th>Output tokens</th><th>Total tokens</th>
                </tr></thead>
                <tbody>${modelRows}</tbody>
            </table>`;

        const systemRows = by_system.map(s => `
            <tr>
                <td>${this._esc(s.system)}</td>
                <td>${fmt(s.requests)}</td>
                <td>${fmt(s.input_tokens + s.output_tokens)}</td>
            </tr>`).join('');

        const systemTable = `
            <h3>By provider</h3>
            <table class="data-table">
                <thead><tr>
                    <th>Provider</th><th>Requests</th><th>Total tokens</th>
                </tr></thead>
                <tbody>${systemRows}</tbody>
            </table>`;

        return summaryCards + costChart + topSpansSection + finishReasonsSection + modelTable + systemTable;
    }

    _buildCostChart(costSeries, bucketSecs) {
        if (!costSeries.length) {
            return `<h3>Cost over time</h3><div class="empty-state-hint">No cost data in this window.</div>`;
        }

        // Group rows by timestamp bucket, computing cost per row and summing.
        const bucketMap = new Map();
        for (const row of costSeries) {
            const ts = row.timestamp;
            const cost = computeCost(row.model, {
                input_tokens: row.input_tokens ?? 0,
                output_tokens: row.output_tokens ?? 0,
                cache_creation_input_tokens: row.cache_creation_tokens ?? 0,
                cache_read_input_tokens: row.cache_read_tokens ?? 0,
            }) ?? 0;
            const existing = bucketMap.get(ts) || { timestamp: ts, cost: 0, models: {} };
            existing.cost += cost;
            existing.models[row.model] = (existing.models[row.model] || 0) + cost;
            bucketMap.set(ts, existing);
        }
        const buckets = Array.from(bucketMap.values()).sort((a, b) => a.timestamp - b.timestamp);
        const total = buckets.reduce((a, b) => a + b.cost, 0);
        const maxCost = buckets.reduce((a, b) => Math.max(a, b.cost), 0);

        const width = 100;
        const barGap = 0.5;
        const barWidth = buckets.length > 0 ? Math.max((width - barGap * (buckets.length - 1)) / buckets.length, 0.1) : 0;
        const chartHeight = 100;

        const bars = buckets.map((b, i) => {
            const h = maxCost > 0 ? (b.cost / maxCost) * chartHeight : 0;
            const x = i * (barWidth + barGap);
            const y = chartHeight - h;
            const breakdown = Object.entries(b.models)
                .filter(([, v]) => v > 0)
                .map(([m, v]) => `${m}: $${v.toFixed(4)}`)
                .join('\n');
            const tsDate = new Date(b.timestamp / 1_000_000);
            const title = `${formatTs(tsDate)}\n$${b.cost.toFixed(4)}${breakdown ? `\n${breakdown}` : ''}`;
            return `<rect class="cost-chart-bar" x="${x.toFixed(3)}" y="${y.toFixed(3)}" width="${barWidth.toFixed(3)}" height="${h.toFixed(3)}"><title>${this._esc(title)}</title></rect>`;
        }).join('');

        const labelFor = i => {
            const d = new Date(buckets[i].timestamp / 1_000_000);
            const pad = n => String(n).padStart(2, '0');
            if (bucketSecs >= 86400) return `${d.getFullYear()}-${pad(d.getMonth()+1)}-${pad(d.getDate())}`;
            return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
        };

        const axisLabels = [];
        if (buckets.length > 0) {
            axisLabels.push(`<text class="cost-chart-axis" x="0" y="${chartHeight + 8}">${this._esc(labelFor(0))}</text>`);
            if (buckets.length > 2) {
                const mid = Math.floor(buckets.length / 2);
                const midX = mid * (barWidth + barGap) + barWidth / 2;
                axisLabels.push(`<text class="cost-chart-axis" x="${midX.toFixed(3)}" y="${chartHeight + 8}" text-anchor="middle">${this._esc(labelFor(mid))}</text>`);
            }
            if (buckets.length > 1) {
                axisLabels.push(`<text class="cost-chart-axis" x="${width}" y="${chartHeight + 8}" text-anchor="end">${this._esc(labelFor(buckets.length - 1))}</text>`);
            }
        }

        return `
            <h3>Cost over time — total $${total.toFixed(4)} across ${buckets.length} bucket${buckets.length === 1 ? '' : 's'}</h3>
            <div class="cost-chart">
                <svg class="cost-chart-svg" viewBox="0 -4 ${width} ${chartHeight + 14}" preserveAspectRatio="none">
                    ${bars}
                    ${axisLabels.join('')}
                </svg>
            </div>`;
    }

    _buildTopSpans(topSpans) {
        if (!topSpans.length) {
            return `<h3>Top 20 most expensive calls</h3><div class="empty-state-hint">No GenAI spans in this window.</div>`;
        }

        const fmt = n => Number(n).toLocaleString();

        const rows = topSpans.map(row => {
            const cost = computeCost(row.model, {
                input_tokens: row.input_tokens ?? 0,
                output_tokens: row.output_tokens ?? 0,
                cache_creation_input_tokens: row.cache_creation_tokens ?? 0,
                cache_read_input_tokens: row.cache_read_tokens ?? 0,
            });
            const costStr = cost === null ? '—' : `$${cost.toFixed(4)}`;
            const costClass = cost !== null && cost >= 0.01 ? 'top-spans-cost-high' : '';

            const tsDate = new Date((row.start_time ?? 0) / 1_000_000);
            const timeStr = formatTs(tsDate);

            const sessionCell = row.session_id
                ? `<a href="#" onclick="window.app.navigateToLogsBySession('${this._esc(row.session_id)}'); return false;" title="${this._esc(row.session_id)}">${this._esc(String(row.session_id).slice(0, 8))}</a>`
                : '—';
            const promptCell = row.prompt_id
                ? `<a href="#" onclick="window.app.navigateToLogsByPrompt('${this._esc(row.prompt_id)}'); return false;" title="${this._esc(row.prompt_id)}">${this._esc(String(row.prompt_id).slice(0, 8))}</a>`
                : '—';
            const traceCell = row.trace_id
                ? `<a href="#" onclick="window.app.navigateToTrace('${this._esc(row.trace_id)}'); return false;" title="${this._esc(row.trace_id)}">${this._esc(String(row.trace_id).slice(0, 8))}</a>`
                : '—';

            return `
                <tr>
                    <td>${this._esc(timeStr)}</td>
                    <td>${this._esc(row.model || '—')}</td>
                    <td>${sessionCell}</td>
                    <td>${promptCell}</td>
                    <td>${fmt(row.input_tokens ?? 0)}</td>
                    <td>${fmt(row.output_tokens ?? 0)}</td>
                    <td>${fmt(row.cache_read_tokens ?? 0)}</td>
                    <td class="${costClass}">${costStr}</td>
                    <td>${traceCell}</td>
                </tr>`;
        }).join('');

        return `
            <h3>Top 20 most expensive calls</h3>
            <table class="data-table">
                <thead><tr>
                    <th>Time</th><th>Model</th><th>Session</th><th>Prompt</th>
                    <th>Input</th><th>Output</th><th>Cache read</th><th>Cost (est.)</th><th>Trace</th>
                </tr></thead>
                <tbody>${rows}</tbody>
            </table>`;
    }

    _buildFinishReasons(reasons) {
        if (!reasons.length) {
            return `<h3>Finish reasons</h3><div class="empty-state-hint">No finish-reason data in this window.</div>`;
        }
        const total = reasons.reduce((acc, r) => acc + (r.count || 0), 0);
        const sorted = [...reasons].sort((a, b) => (b.count || 0) - (a.count || 0));
        const rows = sorted.map(r => {
            const count = r.count || 0;
            const pct = total > 0 ? (count / total) * 100 : 0;
            const reason = String(r.reason || 'unknown');
            const warning = reason.toLowerCase() === 'max_tokens';
            return `
                <div class="finish-reason-row">
                    <div class="finish-reason-name">${this._esc(reason)}</div>
                    <div class="finish-reason-bar"><div class="finish-reason-fill ${warning ? 'warning' : ''}" style="width:${pct.toFixed(2)}%"></div></div>
                    <div class="finish-reason-count">${Number(count).toLocaleString()} (${pct.toFixed(1)}%)</div>
                </div>`;
        }).join('');

        return `
            <h3>Finish reasons</h3>
            <div class="finish-reasons-list">${rows}</div>`;
    }

    _esc(str) {
        return String(str)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }
}

window.UsageView = UsageView;
