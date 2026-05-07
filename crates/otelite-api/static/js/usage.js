// GenAI token usage view
//
// Costs are computed server-side (see crates/otelite-core/src/pricing.rs). Rows
// returned by the backend carry `cost`, `cost_source` and optional
// `cost_reason` fields — this view only renders them.

function formatTs(date) {
    const p = n => String(n).padStart(2, '0');
    return `${date.getFullYear()}-${p(date.getMonth()+1)}-${p(date.getDate())} ` +
           `${p(date.getHours())}:${p(date.getMinutes())}:${p(date.getSeconds())}`;
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
            ${this._renderTipsPanel()}
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
        this._attachTipsPanelListener();
        await this._loadAndRender();

        if (!this.refreshInterval) {
            this.refreshInterval = setInterval(() => this._loadAndRender(), 30000);
        }
    }

    _renderTipsPanel() {
        const dismissed = localStorage.getItem('otelite_tips_dismissed_usage') === 'true';
        const openAttr = dismissed ? '' : ' open';
        return `
            <details class="tips-panel" id="tips-panel-usage"${openAttr}>
                <summary>Tips</summary>
                <div class="tips-panel-body">
                    <h4>Widget tips</h4>
                    <ul>
                        <li>Cost is estimated from a Claude 4.x pricing table — unknown models show "—"</li>
                        <li>Cost-over-time bucket auto-scales with your time window (1m for ≤1h up to 1d for ≥7d)</li>
                        <li>Click session / prompt / trace cells in the Top expensive calls table to drill in</li>
                        <li>Truncation gauge turns red if any response ended with <code>finish_reason=max_tokens</code></li>
                        <li>Tool rows go amber if success rate &lt; 90%</li>
                    </ul>
                    <h4>Debugging recipes</h4>
                    <ul>
                        <li>What did this prompt cost? — Logs → click any <code>prompt.id</code> → summary banner</li>
                        <li>All activity for a session — click <code>session.id</code> anywhere</li>
                        <li>Any truncation? — Usage → finish_reasons → <code>max_tokens</code> bar</li>
                        <li>Top expensive calls — Usage → top calls table</li>
                        <li>Which tool is failing? — Usage → tool usage table → success rate column</li>
                        <li>Is Opus slower than Sonnet? — Usage → latency-by-model → compare P50 / P95</li>
                        <li>How much is cache saving? — Usage → cache hit rate gauge</li>
                    </ul>
                </div>
            </details>
        `;
    }

    _attachTipsPanelListener() {
        const panel = document.getElementById('tips-panel-usage');
        if (!panel) return;
        panel.addEventListener('toggle', () => {
            if (!panel.open) {
                localStorage.setItem('otelite_tips_dismissed_usage', 'true');
            } else {
                localStorage.removeItem('otelite_tips_dismissed_usage');
            }
        });
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

    _prefillDateInputsFromData(costSeries, bucketSecs) {
        if (this.trStart !== null || this.trEnd !== null) return; // preset already populated via _syncDateInputs
        const startEl = document.getElementById('tr-start-usage');
        const endEl = document.getElementById('tr-end-usage');
        if (!startEl || !endEl) return;
        if (!Array.isArray(costSeries) || costSeries.length === 0) return;
        const timestamps = costSeries
            .map(r => r.timestamp)
            .filter(t => typeof t === 'number');
        if (timestamps.length === 0) return;
        const minMs = Math.min(...timestamps) / 1_000_000;
        const bucketMs = (bucketSecs || 3600) * 1000;
        // Last bucket's END is (last-start + bucket). Cap at now so we don't
        // show an end in the future for bucket sizes that cross now.
        const maxMs = Math.min(Math.max(...timestamps) / 1_000_000 + bucketMs, Date.now());
        startEl.value = this._toDatetimeLocal(new Date(minMs));
        endEl.value = this._toDatetimeLocal(new Date(maxMs));
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
            const [summary, costSeries, topSpans, finishReasons, latencyStats, errorRate, toolUsage, retryStats, retrievalStats, pricingMeta] = await Promise.all([
                this.api.getTokenUsage(params),
                this.api.getCostSeries({ ...params, bucket }),
                this.api.getTopSpans({ ...params, limit: 20 }),
                this.api.getFinishReasons(params),
                this.api.getLatencyStats(params),
                this.api.getErrorRate(params),
                this.api.getToolUsage(params),
                this.api.getRetryStats(params),
                this.api.getRetrievalStats(params).catch(() => null),
                this.api.getPricingMetadata().catch(() => null),
            ]);
            dataContainer.innerHTML = this._buildHtml(summary, costSeries, topSpans, finishReasons, bucket, latencyStats, errorRate, toolUsage, retryStats, retrievalStats, pricingMeta);
            // When no explicit range is selected ("All time"), show the user
            // what effective range the query covered by prefilling the date
            // inputs from the returned cost-series data. Prefill is visual
            // only — we don't mutate trStart/trEnd so the preset stays
            // "All time" until the user actively edits.
            this._prefillDateInputsFromData(costSeries, bucket);
        } catch (err) {
            dataContainer.innerHTML = `<div class="empty-state"><p>Failed to load usage data</p><p class="empty-state-hint">${err.message}</p></div>`;
        }
    }

    _buildHtml(data, costSeries, topSpans, finishReasons, bucket, latencyStats, errorRate, toolUsage, retryStats, retrievalStats, pricingMeta) {
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
                ${this._buildRetryGauge(retryStats)}
            </div>`;

        const costChart = this._buildCostChart(costSeries || [], bucket);
        const topSpansSection = this._buildTopSpans(topSpans || []);
        const finishReasonsSection = this._buildFinishReasons(reasons);
        const latencySection = this._buildLatencyTable(latencyStats || []);
        const errorRateSection = this._buildErrorRate(errorRate || []);
        const toolUsageSection = this._buildToolUsage(toolUsage || []);
        const retrievalSection = this._buildRetrievalStats(retrievalStats);

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

        const pricingNotice = this._renderPricingNotice(pricingMeta);

        return pricingNotice + summaryCards + costChart + topSpansSection + finishReasonsSection
            + latencySection + errorRateSection + toolUsageSection + retrievalSection
            + modelTable + systemTable;
    }

    _buildRetrievalStats(stats) {
        if (!stats || !stats.total_retrievals) return '';
        const fmt = n => Number(n).toLocaleString();
        const avgDocs = stats.avg_documents_per_query != null
            ? Number(stats.avg_documents_per_query).toFixed(2)
            : '—';
        const avgScore = stats.avg_top_document_score != null
            ? Number(stats.avg_top_document_score).toFixed(3)
            : null;

        const summaryLine = `
            <div class="retrieval-summary">
                <span><strong>${fmt(stats.total_retrievals)}</strong> retrievals</span>
                <span>·</span>
                <span><strong>${avgDocs}</strong> avg docs / query</span>
                ${avgScore !== null ? `<span>·</span><span><strong>${avgScore}</strong> avg top-1 score</span>` : ''}
            </div>`;

        const topQueries = Array.isArray(stats.top_queries) ? stats.top_queries : [];
        const topTable = topQueries.length > 0 ? `
            <table class="data-table">
                <thead><tr>
                    <th>Query</th><th>Retrievals</th><th>Avg docs</th><th>Avg top score</th>
                </tr></thead>
                <tbody>${topQueries.map(q => {
                    const full = String(q.query ?? '');
                    const truncated = full.length > 80 ? full.slice(0, 80) + '…' : full;
                    const avgDocsQ = q.avg_documents != null ? Number(q.avg_documents).toFixed(2) : '—';
                    const avgScoreQ = q.avg_top_score != null ? Number(q.avg_top_score).toFixed(3) : '—';
                    return `
                        <tr>
                            <td title="${this._esc(full)}">${this._esc(truncated)}</td>
                            <td>${fmt(q.count || 0)}</td>
                            <td>${this._esc(avgDocsQ)}</td>
                            <td>${this._esc(avgScoreQ)}</td>
                        </tr>`;
                }).join('')}</tbody>
            </table>` : '';

        return `
            <h3>Retrieval (RAG) activity</h3>
            ${summaryLine}
            ${topTable}
        `;
    }

    _formatDuration(ms) {
        if (ms == null) return '—';
        return ms < 10000 ? `${Number(ms).toLocaleString()} ms` : `${(ms / 1000).toFixed(1)} s`;
    }

    _buildRetryGauge(retryStats) {
        if (!retryStats || !retryStats.total_llm_calls) return '';
        const rate = retryStats.retry_rate || 0;
        const pct = rate * 100;
        const fmt = n => Number(n).toLocaleString();
        return `
                <div class="usage-gauge-card">
                    <div class="usage-card-label">Retry rate</div>
                    <div class="usage-card-value">${pct.toFixed(1)}%</div>
                    <div class="gauge-bar"><div class="gauge-fill ${pct > 0 ? 'gauge-fill-warning' : ''}" style="width:${pct.toFixed(2)}%"></div></div>
                    <div class="gauge-hint">${fmt(retryStats.retried_calls || 0)} of ${fmt(retryStats.total_llm_calls)} calls retried (${fmt(retryStats.extra_attempts || 0)} extra attempts)</div>
                </div>`;
    }

    _buildLatencyTable(latencyStats) {
        if (!latencyStats.length) {
            return `<h3>Latency by model</h3><div class="empty-state-hint">No latency data in this window.</div>`;
        }
        const fmt = n => Number(n).toLocaleString();
        const rows = latencyStats.map(s => {
            const ttftP50 = s.ttft_count > 0 ? this._formatDuration(s.ttft_p50_ms) : '—';
            const ttftP95 = s.ttft_count > 0 ? this._formatDuration(s.ttft_p95_ms) : '—';
            return `
                <tr>
                    <td>${this._esc(s.model || '—')}</td>
                    <td>${fmt(s.count || 0)}</td>
                    <td>${this._esc(this._formatDuration(s.avg_ms))}</td>
                    <td>${this._esc(this._formatDuration(s.p50_ms))}</td>
                    <td>${this._esc(this._formatDuration(s.p95_ms))}</td>
                    <td>${this._esc(this._formatDuration(s.p99_ms))}</td>
                    <td>${this._esc(ttftP50)}</td>
                    <td>${this._esc(ttftP95)}</td>
                </tr>`;
        }).join('');
        return `
            <h3>Latency by model</h3>
            <table class="data-table latency-table">
                <thead><tr>
                    <th>Model</th><th>Calls</th><th>Avg</th><th>P50</th><th>P95</th><th>P99</th><th>TTFT P50</th><th>TTFT P95</th>
                </tr></thead>
                <tbody>${rows}</tbody>
            </table>`;
    }

    _buildErrorRate(errorRate) {
        if (!errorRate.length || errorRate.every(r => (r.errors || 0) === 0)) {
            return '';
        }
        const sorted = [...errorRate].sort((a, b) => (b.error_rate || 0) - (a.error_rate || 0));
        const rows = sorted.map(r => {
            const rate = r.error_rate || 0;
            const pct = rate * 100;
            const warning = rate > 0.1;
            return `
                <div class="finish-reason-row">
                    <div class="finish-reason-name">${this._esc(r.model || '—')}</div>
                    <div class="finish-reason-bar"><div class="finish-reason-fill ${warning ? 'warning' : ''}" style="width:${pct.toFixed(2)}%"></div></div>
                    <div class="finish-reason-count">${r.errors || 0}/${r.total || 0} (${pct.toFixed(1)}%)</div>
                </div>`;
        }).join('');
        return `
            <h3>Error rate by model</h3>
            <div class="finish-reasons-list error-rate-list">${rows}</div>`;
    }

    _buildToolUsage(toolUsage) {
        if (!toolUsage.length) {
            return `<h3>Tool usage</h3><div class="empty-state-hint">No tool-use spans in this window.</div>`;
        }
        const fmt = n => Number(n).toLocaleString();
        const rows = toolUsage.map(t => {
            const count = t.count || 0;
            const succ = t.success_count || 0;
            const rate = count > 0 ? (succ / count) * 100 : 0;
            const warn = rate < 90;
            return `
                <tr class="${warn ? 'tool-usage-warn' : ''}">
                    <td>${this._esc(t.tool_name || '—')}</td>
                    <td>${fmt(count)}</td>
                    <td>${rate.toFixed(1)}%</td>
                    <td>${fmt(t.error_count || 0)}</td>
                    <td>${this._esc(this._formatDuration(t.avg_duration_ms))}</td>
                </tr>`;
        }).join('');
        return `
            <h3>Tool usage</h3>
            <table class="data-table tool-usage-table">
                <thead><tr>
                    <th>Tool</th><th>Calls</th><th>Success rate</th><th>Errors</th><th>Avg duration</th>
                </tr></thead>
                <tbody>${rows}</tbody>
            </table>`;
    }

    _buildCostChart(costSeries, bucketSecs) {
        if (!costSeries.length) {
            return `<h3>Cost over time</h3><div class="empty-state-hint">No cost data in this window.</div>`;
        }

        // Server-enriched: each row has `cost` (null if unknown). Group by
        // timestamp bucket and sum, tracking per-model breakdown for tooltips.
        const bucketMap = new Map();
        for (const row of costSeries) {
            const ts = row.timestamp;
            const cost = row.cost ?? 0;
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

        // Axis labels render as HTML below the SVG so the SVG's
        // preserveAspectRatio="none" (needed to stretch bars across the
        // container width) doesn't distort the text.
        let axisHtml = '';
        if (buckets.length > 0) {
            const left = this._esc(labelFor(0));
            const mid = buckets.length > 2
                ? this._esc(labelFor(Math.floor(buckets.length / 2)))
                : '';
            const right = buckets.length > 1
                ? this._esc(labelFor(buckets.length - 1))
                : '';
            axisHtml = `
                <div class="cost-chart-axis-labels">
                    <span class="cost-chart-axis-left">${left}</span>
                    <span class="cost-chart-axis-mid">${mid}</span>
                    <span class="cost-chart-axis-right">${right}</span>
                </div>`;
        }

        return `
            <h3>Cost over time — total $${total.toFixed(4)} across ${buckets.length} bucket${buckets.length === 1 ? '' : 's'}</h3>
            <div class="cost-chart">
                <svg class="cost-chart-svg" viewBox="0 0 ${width} ${chartHeight}" preserveAspectRatio="none">
                    ${bars}
                </svg>
                ${axisHtml}
            </div>`;
    }

    _buildTopSpans(topSpans) {
        if (!topSpans.length) {
            return `<h3>Top 20 most expensive calls</h3><div class="empty-state-hint">No GenAI spans in this window.</div>`;
        }

        const fmt = n => Number(n).toLocaleString();

        // Hide optional columns entirely when no row populates them. Claude
        // Code emits prompt.id only on api_request_body logs (not on spans),
        // so a column of em-dashes adds noise without signal.
        const anyPrompt = topSpans.some(r => r.prompt_id);
        const anySession = topSpans.some(r => r.session_id);

        const rows = topSpans.map(row => {
            const cost = row.cost ?? null;
            const costStr = cost === null
                ? `<span title="${this._esc(row.cost_reason || 'no pricing match')}">—</span>`
                : `$${cost.toFixed(4)}`;
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
                    ${anySession ? `<td>${sessionCell}</td>` : ''}
                    ${anyPrompt ? `<td>${promptCell}</td>` : ''}
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
                    <th>Time</th><th>Model</th>
                    ${anySession ? '<th>Session</th>' : ''}
                    ${anyPrompt ? '<th>Prompt</th>' : ''}
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

    _renderPricingNotice(meta) {
        if (!meta) return '';
        const source = meta.source;
        const sourceLabel = source === 'litellm'
            ? `LiteLLM (${meta.entry_count.toLocaleString()} models)`
            : `hardcoded Claude fallback — last verified ${meta.fallback_last_verified}`;
        const freshness = source === 'litellm' && meta.last_fetched_unix_ms
            ? ` · fetched ${this._relativeTime(meta.last_fetched_unix_ms)}`
            : '';
        const staleWarning = source !== 'litellm' && meta.last_failed_unix_ms
            ? ` · <span class="pricing-disclaimer-warn">last LiteLLM fetch failed ${this._relativeTime(meta.last_failed_unix_ms)}</span>`
            : '';
        return `
            <div class="pricing-disclaimer" role="note">
                <strong>Pricing note:</strong> ${this._esc(meta.disclaimer)}
                <br>
                <span>Source: ${this._esc(sourceLabel)}${freshness}${staleWarning}</span>
                · <a href="${this._esc(meta.source_url)}" target="_blank" rel="noopener">${this._esc(meta.license)}</a>
            </div>`;
    }

    _relativeTime(unixMs) {
        const diffSec = (Date.now() - unixMs) / 1000;
        if (diffSec < 60) return 'just now';
        if (diffSec < 3600) return `${Math.round(diffSec / 60)} min ago`;
        if (diffSec < 86400) return `${Math.round(diffSec / 3600)} h ago`;
        return `${Math.round(diffSec / 86400)} d ago`;
    }
}

window.UsageView = UsageView;
