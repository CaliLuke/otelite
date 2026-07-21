// GenAI analytics view
//
// Replaces the old "Usage" tab. The page is organised as 4 collapsed
// <details> accordion sections grouped by question:
//   Cost · Latency · Reliability · Behavior
// On initial load only a single cheap getTokenUsage summary call (plus the
// static pricing metadata) is made — every chart inside a section is fetched
// lazily on first expand and cached thereafter.
//
// Costs are computed server-side (see crates/otelite-core/src/pricing.rs).

function formatTs(date) {
    const p = n => String(n).padStart(2, '0');
    return `${date.getFullYear()}-${p(date.getMonth()+1)}-${p(date.getDate())} ` +
           `${p(date.getHours())}:${p(date.getMinutes())}:${p(date.getSeconds())}`;
}

class AnalyticsView {
    constructor(apiClient) {
        this.api = apiClient;
        this.refreshInterval = null;
        const now = new Date();
        this.trWindowHours = 24;
        this.trEnd = now;
        this.trStart = new Date(now.getTime() - this.trWindowHours * 3600000);
        this.modelFilter = null;
        this.topNSort = 'cost';
        // Loader registry — keyed by section id ('cost', 'latency', ...)
        this.sectionLoaders = {};
        // Sections that have rendered their content for the current params.
        this.loadedSections = new Set();
        // Track open state across re-renders
        this.openSections = new Set();
        this.lastSummary = null;
    }

    async render() {
        const container = document.getElementById('analytics-container');
        if (!container) return;

        container.innerHTML = `
            ${this._renderTipsPanel()}
            <div class="view-header">
                <h2>GenAI Analytics</h2>
            </div>
            <div class="filters">
                <div class="time-range-bar">
                    <button class="btn-icon" id="tr-prev-analytics" title="Previous window">&#8592;</button>
                    <input type="text" id="tr-start-analytics" class="filter-input tr-datetime" placeholder="YYYY-MM-DD HH:MM" autocomplete="off">
                    <span class="tr-sep">–</span>
                    <input type="text" id="tr-end-analytics" class="filter-input tr-datetime" placeholder="YYYY-MM-DD HH:MM" autocomplete="off">
                    <button class="btn-icon" id="tr-next-analytics" title="Next window">&#8594;</button>
                    <button class="btn-icon" id="tr-now-analytics" title="Jump to now">Now</button>
                    <select id="tr-preset-analytics" class="filter-select tr-preset">
                        <option value="">All time</option>
                        <option value="1">1 hr</option>
                        <option value="6">6 hr</option>
                        <option value="24" selected>24 hr</option>
                        <option value="168">7 days</option>
                    </select>
                </div>
                <select id="analytics-model-filter" class="filter-select">
                    <option value="">All models</option>
                </select>
            </div>
            <div id="analytics-pricing-notice"></div>
            <div id="analytics-summary-cards"></div>
            <div id="analytics-empty-state"></div>
            <div id="analytics-sections">
                ${this._renderSectionShell('cost', 'Cost', 'Tokens spent · pricing · most expensive calls')}
                ${this._renderSectionShell('latency', 'Latency', 'Response time · throughput · context size')}
                ${this._renderSectionShell('reliability', 'Reliability', 'Errors · retries · truncation · drift')}
                ${this._renderSectionShell('behavior', 'Behavior', 'Tool use · retrieval · request volume')}
            </div>
        `;

        this._attachTimeRangeListeners();
        this._attachTipsPanelListener();
        this._syncDateInputs();
        document.getElementById('analytics-model-filter').addEventListener('change', (e) => {
            this.modelFilter = e.target.value || null;
            this._refresh();
        });

        this._registerSectionLoaders();
        this._attachSectionToggleHandlers();

        await this._loadSummary();

        if (!this.refreshInterval) {
            this.refreshInterval = setInterval(() => this._refresh(), 30000);
        }
    }

    _renderSectionShell(id, title, hint) {
        const open = this.openSections.has(id);
        return `
            <details class="analytics-section" id="analytics-section-${id}"${open ? ' open' : ''}>
                <summary class="analytics-section-summary">
                    <span class="analytics-section-title">${title}</span>
                    <span class="analytics-section-hint">${hint}</span>
                    <span class="analytics-section-stat" id="analytics-section-stat-${id}">—</span>
                </summary>
                <div class="analytics-section-body" id="analytics-section-body-${id}">
                    <div class="empty-state-hint">Loading…</div>
                </div>
            </details>`;
    }

    _renderTipsPanel() {
        const dismissed = localStorage.getItem('otelite_tips_dismissed_analytics') === 'true';
        const openAttr = dismissed ? '' : ' open';
        return `
            <details class="tips-panel" id="tips-panel-analytics"${openAttr}>
                <summary>Tips</summary>
                <div class="tips-panel-body">
                    <h4>Layout</h4>
                    <ul>
                        <li>Sections are collapsed by default — expand only what you need; charts fetch lazily on first open</li>
                        <li>Top-spans table is under <strong>Cost</strong> — use the sort dropdown to switch between most expensive / slowest / truncated / etc.</li>
                    </ul>
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
                        <li>Any truncation? — Reliability → finish_reasons → <code>max_tokens</code> bar</li>
                        <li>Top expensive calls — Cost → top calls table</li>
                        <li>Which tool is failing? — Behavior → tool usage table → success rate column</li>
                        <li>Is Opus slower than Sonnet? — Latency → latency-by-model</li>
                        <li>How much is cache saving? — Cost → cache hit rate</li>
                    </ul>
                </div>
            </details>
        `;
    }

    _attachTipsPanelListener() {
        const panel = document.getElementById('tips-panel-analytics');
        if (!panel) return;
        panel.addEventListener('toggle', () => {
            if (!panel.open) {
                localStorage.setItem('otelite_tips_dismissed_analytics', 'true');
            } else {
                localStorage.removeItem('otelite_tips_dismissed_analytics');
            }
        });
    }

    _attachTimeRangeListeners() {
        document.getElementById('tr-preset-analytics').addEventListener('change', (e) => {
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
            this._refresh();
        });

        document.getElementById('tr-start-analytics').addEventListener('change', () => this._onDateInputChange());
        document.getElementById('tr-end-analytics').addEventListener('change', () => this._onDateInputChange());

        document.getElementById('tr-prev-analytics').addEventListener('click', () => {
            const windowMs = (this.trWindowHours || 1) * 3600000;
            const end = (this.trEnd || new Date()).getTime() - windowMs;
            const start = (this.trStart ? this.trStart.getTime() : end - windowMs) - windowMs;
            this.trEnd = new Date(end);
            this.trStart = new Date(start);
            this._syncDateInputs();
            document.getElementById('tr-preset-analytics').value = '';
            this._refresh();
        });

        document.getElementById('tr-next-analytics').addEventListener('click', () => {
            const now = Date.now();
            const windowMs = (this.trWindowHours || 1) * 3600000;
            let end = (this.trEnd || new Date()).getTime() + windowMs;
            if (end > now) end = now;
            this.trEnd = new Date(end);
            this.trStart = new Date(end - windowMs);
            this._syncDateInputs();
            document.getElementById('tr-preset-analytics').value = '';
            this._refresh();
        });

        document.getElementById('tr-now-analytics').addEventListener('click', () => {
            const now = new Date();
            const windowMs = (this.trWindowHours || 1) * 3600000;
            this.trEnd = now;
            this.trStart = new Date(now.getTime() - windowMs);
            this._syncDateInputs();
            document.getElementById('tr-preset-analytics').value = '';
            this._refresh();
        });
    }

    _syncDateInputs() {
        const startEl = document.getElementById('tr-start-analytics');
        const endEl = document.getElementById('tr-end-analytics');
        if (startEl) startEl.value = this.trStart ? this._toDatetimeLocal(this.trStart) : '';
        if (endEl) endEl.value = this.trEnd ? this._toDatetimeLocal(this.trEnd) : '';
    }

    _prefillDateInputsFromData(costSeries, bucketSecs) {
        if (this.trStart !== null || this.trEnd !== null) return;
        const startEl = document.getElementById('tr-start-analytics');
        const endEl = document.getElementById('tr-end-analytics');
        if (!startEl || !endEl) return;
        if (!Array.isArray(costSeries) || costSeries.length === 0) return;
        const timestamps = costSeries
            .map(r => r.timestamp)
            .filter(t => typeof t === 'number');
        if (timestamps.length === 0) return;
        const minMs = Math.min(...timestamps) / 1_000_000;
        const bucketMs = (bucketSecs || 3600) * 1000;
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
        const startEl = document.getElementById('tr-start-analytics');
        const endEl = document.getElementById('tr-end-analytics');
        this.trStart = this._parseDatetimeInput(startEl ? startEl.value : '');
        this.trEnd = this._parseDatetimeInput(endEl ? endEl.value : '');
        if (this.trStart && this.trEnd) {
            this.trWindowHours = (this.trEnd.getTime() - this.trStart.getTime()) / 3600000;
        }
        const presetEl = document.getElementById('tr-preset-analytics');
        if (presetEl) presetEl.value = '';
        this._refresh();
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

    _baseParams() {
        const params = {};
        if (this.trStart !== null) {
            params.start_time = this.trStart.getTime() * 1_000_000;
            params.end_time = (this.trEnd || new Date()).getTime() * 1_000_000;
        }
        if (this.modelFilter) params.model = this.modelFilter;
        return params;
    }

    /**
     * Re-fetch summary and any currently-expanded section. Called when the
     * time window or model filter changes, or on the 30s auto-refresh.
     */
    async _refresh() {
        this.loadedSections.clear();
        await this._loadSummary();
        // Re-fire loaders for any open sections
        for (const id of Object.keys(this.sectionLoaders)) {
            const details = document.getElementById(`analytics-section-${id}`);
            if (details && details.open) {
                this.sectionLoaders[id]();
            }
        }
    }

    /**
     * Single eager call: getTokenUsage. Populates the header summary cards,
     * the per-section tiny stat in each <summary>, the model dropdown, and
     * the pricing-notice slot (a separate cheap fetch for static metadata).
     */
    async _loadSummary() {
        const summaryContainer = document.getElementById('analytics-summary-cards');
        const emptyEl = document.getElementById('analytics-empty-state');
        const sectionsEl = document.getElementById('analytics-sections');
        const noticeEl = document.getElementById('analytics-pricing-notice');
        if (!summaryContainer) return;

        try {
            const params = this._baseParams();
            const [summary, pricingMeta] = await Promise.all([
                this.api.getTokenUsage(params),
                this.api.getPricingMetadata().catch(() => null),
            ]);
            this.lastSummary = summary;

            if (noticeEl) {
                noticeEl.innerHTML = this._renderPricingNotice(pricingMeta);
            }

            if (!summary || !summary.summary || summary.summary.total_requests === 0) {
                summaryContainer.innerHTML = '';
                if (sectionsEl) sectionsEl.style.display = 'none';
                if (emptyEl) {
                    emptyEl.innerHTML = `<div class="empty-state">
                        <p>No GenAI data yet</p>
                        <p class="empty-state-hint">
                            Instrument your LLM application with the OpenAI or Anthropic OTel SDK and point it at
                            <strong>http://localhost:4318</strong>. Token usage will appear here once spans with
                            <code>gen_ai.system</code> attributes arrive.
                        </p>
                    </div>`;
                }
                this._populateModelDropdown([]);
                return;
            }

            if (sectionsEl) sectionsEl.style.display = '';
            if (emptyEl) emptyEl.innerHTML = '';

            summaryContainer.innerHTML = this._buildHeaderCards(summary);
            this._populateModelDropdown(summary.by_model || []);
            this._updateSectionStats(summary);
        } catch (err) {
            summaryContainer.innerHTML = `<div class="empty-state"><p>Failed to load analytics summary</p><p class="empty-state-hint">${this._esc(err.message)}</p></div>`;
        }
    }

    _buildHeaderCards(data) {
        const { summary } = data;
        const fmt = n => Number(n).toLocaleString();
        const totalInput = summary.total_input_tokens ?? 0;
        const totalOutput = summary.total_output_tokens ?? 0;
        return `
            <div class="usage-summary-cards">
                <div class="usage-card">
                    <div class="usage-card-label">Requests</div>
                    <div class="usage-card-value">${fmt(summary.total_requests ?? 0)}</div>
                </div>
                <div class="usage-card">
                    <div class="usage-card-label">Input tokens</div>
                    <div class="usage-card-value">${fmt(totalInput)}</div>
                </div>
                <div class="usage-card">
                    <div class="usage-card-label">Output tokens</div>
                    <div class="usage-card-value">${fmt(totalOutput)}</div>
                </div>
                <div class="usage-card">
                    <div class="usage-card-label">Models</div>
                    <div class="usage-card-value">${fmt((data.by_model || []).length)}</div>
                </div>
            </div>`;
    }

    _updateSectionStats(data) {
        const { summary, by_model } = data;
        const fmt = n => Number(n).toLocaleString();
        const requests = summary.total_requests ?? 0;
        const totalTokens = (summary.total_input_tokens ?? 0) + (summary.total_output_tokens ?? 0);
        const modelCount = (by_model || []).length;

        const set = (id, html) => {
            const el = document.getElementById(`analytics-section-stat-${id}`);
            if (el) el.innerHTML = html;
        };
        set('cost', `${fmt(totalTokens)} tokens · ${fmt(requests)} req`);
        set('latency', `${fmt(requests)} req · ${fmt(modelCount)} model${modelCount === 1 ? '' : 's'}`);
        set('reliability', `${fmt(requests)} req`);
        set('behavior', `${fmt(requests)} req`);
    }

    _populateModelDropdown(byModel) {
        const sel = document.getElementById('analytics-model-filter');
        if (!sel) return;
        const current = this.modelFilter || '';
        const models = byModel.map(r => r.model).filter(Boolean).sort();
        sel.innerHTML = '<option value="">All models</option>' +
            models.map(m => `<option value="${this._esc(m)}"${m === current ? ' selected' : ''}>${this._esc(m)}</option>`).join('');
    }

    // ── Section lazy-loaders ─────────────────────────────────────────────────

    _registerSectionLoaders() {
        this.sectionLoaders = {
            cost: () => this._loadCostSection(),
            latency: () => this._loadLatencySection(),
            reliability: () => this._loadReliabilitySection(),
            behavior: () => this._loadBehaviorSection(),
        };
    }

    _attachSectionToggleHandlers() {
        for (const id of Object.keys(this.sectionLoaders)) {
            const details = document.getElementById(`analytics-section-${id}`);
            if (!details) continue;
            details.addEventListener('toggle', () => {
                if (details.open) {
                    this.openSections.add(id);
                    if (!this.loadedSections.has(id)) {
                        this.sectionLoaders[id]();
                    }
                } else {
                    this.openSections.delete(id);
                }
            });
        }
    }

    _setSectionBody(id, html) {
        const body = document.getElementById(`analytics-section-body-${id}`);
        if (body) body.innerHTML = html;
    }

    _setSectionLoading(id) {
        this._setSectionBody(id, `<div class="empty-state-hint">Loading…</div>`);
    }

    _setSectionError(id, err) {
        this._setSectionBody(id, `<div class="empty-state-hint">Failed to load: ${this._esc(err.message || String(err))}</div>`);
    }

    async _loadCostSection() {
        this._setSectionLoading('cost');
        try {
            const params = this._baseParams();
            const bucket = this._chooseBucket();
            const [costSeries, topSpans, cacheHitRate, retryStats, errorRate] = await Promise.all([
                this.api.getCostSeries({ ...params, bucket }),
                this.api.getTopSpans({ ...params, limit: 20 }),
                this.api.getCacheHitRate(params).catch(() => null),
                this.api.getRetryStats(params).catch(() => null),
                this.api.getErrorRate(params).catch(() => []),
            ]);

            const summary = this.lastSummary || { summary: {} };
            const cacheRead = summary.summary?.total_cache_read_tokens ?? 0;
            const cacheCreate = summary.summary?.total_cache_creation_tokens ?? 0;
            const totalInput = summary.summary?.total_input_tokens ?? 0;
            const cacheDenom = cacheRead + cacheCreate + totalInput;
            const cachePct = cacheDenom > 0 ? (cacheRead / cacheDenom) * 100 : 0;

            const fmt = n => Number(n).toLocaleString();
            const cacheCard = `
                <div class="usage-summary-cards">
                    <div class="usage-gauge-card">
                        <div class="usage-card-label">Cache hit rate</div>
                        <div class="usage-card-value">${cachePct.toFixed(1)}%</div>
                        <div class="gauge-bar"><div class="gauge-fill" style="width:${cachePct.toFixed(2)}%"></div></div>
                        <div class="gauge-hint">${fmt(cacheRead)} / ${fmt(cacheDenom)} tokens served from cache</div>
                    </div>
                    ${this._buildRetryGauge(retryStats)}
                </div>`;

            const html = [
                cacheCard,
                this._buildCostChart(costSeries || [], bucket),
                this._buildTopNSection(topSpans || [], errorRate || []),
                this._buildCacheHitRate(cacheHitRate || []),
                this._buildByModelByProvider(summary),
            ].filter(Boolean).join('');

            this._setSectionBody('cost', html);
            this._attachTopNDropdownHandler(params);
            this._prefillDateInputsFromData(costSeries, bucket);
            this.loadedSections.add('cost');
        } catch (err) {
            this._setSectionError('cost', err);
        }
    }

    _buildByModelByProvider(data) {
        if (!data || !data.by_model) return '';
        const fmt = n => Number(n).toLocaleString();
        const modelRows = (data.by_model || []).map(m => `
            <tr>
                <td>${this._esc(m.model)}</td>
                <td>${fmt(m.requests)}</td>
                <td>${fmt(m.input_tokens)}</td>
                <td>${fmt(m.output_tokens)}</td>
                <td>${fmt(m.input_tokens + m.output_tokens)}</td>
            </tr>`).join('');
        const systemRows = (data.by_system || []).map(s => `
            <tr>
                <td>${this._esc(s.system)}</td>
                <td>${fmt(s.requests)}</td>
                <td>${fmt(s.input_tokens + s.output_tokens)}</td>
            </tr>`).join('');
        return `
            <h3>By model</h3>
            <table class="data-table">
                <thead><tr>
                    <th>Model</th><th>Requests</th><th>Input tokens</th><th>Output tokens</th><th>Total tokens</th>
                </tr></thead>
                <tbody>${modelRows}</tbody>
            </table>
            <h3>By provider</h3>
            <table class="data-table">
                <thead><tr>
                    <th>Provider</th><th>Requests</th><th>Total tokens</th>
                </tr></thead>
                <tbody>${systemRows}</tbody>
            </table>`;
    }

    async _loadLatencySection() {
        this._setSectionLoading('latency');
        try {
            const params = this._baseParams();
            const bucket = this._chooseBucket();
            const [latencyStats, latencySeries, latencyByContext, conversationDepth] = await Promise.all([
                this.api.getLatencyStats(params),
                this.api.getLatencySeries(params).catch(() => null),
                this.api.getLatencyByContext(params).catch(() => null),
                this.api.getConversationDepth(params).catch(() => null),
            ]);

            const convCard = this._buildConversationDepthCard(conversationDepth);
            const cards = convCard ? `<div class="usage-summary-cards">${convCard}</div>` : '';

            const html = [
                cards,
                this._buildLatencyTable(latencyStats || []),
                this._buildLatencySeriesChart(latencySeries || [], bucket),
                this._buildLatencyByContext(latencyByContext || []),
            ].filter(Boolean).join('');

            this._setSectionBody('latency', html);
            this.loadedSections.add('latency');
        } catch (err) {
            this._setSectionError('latency', err);
        }
    }

    async _loadReliabilitySection() {
        this._setSectionLoading('reliability');
        try {
            const params = this._baseParams();
            const [finishReasons, errorRate, errorTypes, truncationRate, modelDrift] = await Promise.all([
                this.api.getFinishReasons(params),
                this.api.getErrorRate(params),
                this.api.getErrorTypes(params).catch(() => null),
                this.api.getTruncationRate(params).catch(() => null),
                this.api.getModelDrift(params).catch(() => null),
            ]);

            const reasons = Array.isArray(finishReasons) ? finishReasons : [];
            const truncCount = reasons
                .filter(r => String(r.reason || '').toLowerCase() === 'max_tokens')
                .reduce((acc, r) => acc + (r.count || 0), 0);
            const totalCount = reasons.reduce((acc, r) => acc + (r.count || 0), 0);
            const truncPct = totalCount > 0 ? (truncCount / totalCount) * 100 : 0;
            const fmt = n => Number(n).toLocaleString();

            const truncCard = totalCount > 0 ? `
                <div class="usage-summary-cards">
                    <div class="usage-gauge-card">
                        <div class="usage-card-label">Truncation rate</div>
                        <div class="usage-card-value">${truncPct.toFixed(1)}%</div>
                        <div class="gauge-bar"><div class="gauge-fill ${truncPct > 0 ? 'gauge-fill-warning' : ''}" style="width:${truncPct.toFixed(2)}%"></div></div>
                        <div class="gauge-hint">${fmt(truncCount)} / ${fmt(totalCount)} responses hit max_tokens</div>
                    </div>
                </div>` : '';

            const html = [
                truncCard,
                this._buildFinishReasons(reasons),
                this._buildTruncationRate(truncationRate || []),
                this._buildErrorRate(errorRate || []),
                this._buildErrorTypes(errorTypes || []),
                this._buildModelDrift(modelDrift || []),
            ].filter(Boolean).join('');

            this._setSectionBody('reliability', html);
            this.loadedSections.add('reliability');
        } catch (err) {
            this._setSectionError('reliability', err);
        }
    }

    async _loadBehaviorSection() {
        this._setSectionLoading('behavior');
        try {
            const params = this._baseParams();
            const [toolUsage, retrievalStats, requestParamProfile, callsSeries] = await Promise.all([
                this.api.getToolUsage(params),
                this.api.getRetrievalStats(params).catch(() => null),
                this.api.getRequestParamProfile(params).catch(() => null),
                this.api.getCallsSeries(params).catch(() => null),
            ]);

            const html = [
                this._buildCallsChart(callsSeries || []),
                this._buildToolUsage(toolUsage || []),
                this._buildRetrievalStats(retrievalStats),
                this._buildRequestParamProfile(requestParamProfile),
            ].filter(Boolean).join('');

            this._setSectionBody('behavior', html);
            this.loadedSections.add('behavior');
        } catch (err) {
            this._setSectionError('behavior', err);
        }
    }

    // ── Renderers (preserved from the old usage view) ────────────────────────

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

    _formatTokensK(n) {
        if (n == null) return '—';
        if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
        return String(n);
    }

    _buildLatencyTable(latencyStats) {
        if (!latencyStats.length) {
            return `<h3>Latency by model</h3><div class="empty-state-hint">No latency data in this window.</div>`;
        }
        const fmt = n => Number(n).toLocaleString();
        const rows = latencyStats.map(s => {
            const ttftP50 = s.ttft_count > 0 ? this._formatDuration(s.ttft_p50_ms) : '—';
            const ttftP95 = s.ttft_count > 0 ? this._formatDuration(s.ttft_p95_ms) : '—';

            const tpsP50 = s.derived_tokens_per_sec_p50 != null ? Math.round(s.derived_tokens_per_sec_p50) : null;
            const tpsP95 = s.derived_tokens_per_sec_p95 != null ? Math.round(s.derived_tokens_per_sec_p95) : null;
            const tpsP99 = s.derived_tokens_per_sec_p99 != null ? Math.round(s.derived_tokens_per_sec_p99) : null;
            const tpsCell = (tpsP50 != null && tpsP95 != null && tpsP99 != null)
                ? `${tpsP50} / ${tpsP95} / ${tpsP99} tok/s`
                : '—';

            const ctxP50 = this._formatTokensK(s.input_tokens_p50);
            const ctxP95 = this._formatTokensK(s.input_tokens_p95);
            const ctxP99 = this._formatTokensK(s.input_tokens_p99);
            const ctxCell = (s.input_tokens_p50 != null) ? `${ctxP50} / ${ctxP95} / ${ctxP99}` : '—';

            const ratioP50 = s.output_input_ratio_p50 != null ? `${Number(s.output_input_ratio_p50).toFixed(2)}×` : null;
            const ratioP95 = s.output_input_ratio_p95 != null ? `${Number(s.output_input_ratio_p95).toFixed(2)}×` : null;
            const ratioCell = (ratioP50 != null && ratioP95 != null) ? `${ratioP50} / ${ratioP95}` : '—';

            // Flag rows where output volume is extremely high — this is often
            // the primary cause of slow sessions (generation takes minutes).
            const ratioHigh = (s.output_input_ratio_p95 || 0) > 200;
            return `
                <tr${ratioHigh ? ' class="latency-ratio-warn"' : ''}>
                    <td>${this._esc(s.model || '—')}</td>
                    <td class="num">${fmt(s.count || 0)}</td>
                    <td class="num">${this._esc(this._formatDuration(s.avg_ms))}</td>
                    <td class="num">${this._esc(this._formatDuration(s.p50_ms))}</td>
                    <td class="num">${this._esc(this._formatDuration(s.p95_ms))}</td>
                    <td class="num">${this._esc(this._formatDuration(s.p99_ms))}</td>
                    <td class="num">${this._esc(ttftP50)}</td>
                    <td class="num">${this._esc(ttftP95)}</td>
                    <td class="num">${this._esc(tpsCell)}</td>
                    <td class="num">${this._esc(ctxCell)}</td>
                    <td class="num${ratioHigh ? ' latency-ratio-high' : ''}">${this._esc(ratioCell)}</td>
                </tr>`;
        }).join('');
        return `
            <h3>Latency by model</h3>
            <table class="data-table latency-table">
                <thead><tr>
                    <th>Model</th><th>Calls</th><th>Avg</th><th>P50</th><th>P95</th><th>P99</th><th>TTFT P50</th><th>TTFT P95</th>
                    <th title="Derived metric: span duration includes network and queue time, not pure generation throughput">Tok/s derived (p50/p95/p99)</th>
                    <th>Context (p50/p95/p99)</th>
                    <th>Out/In ratio (p50/p95)</th>
                </tr></thead>
                <tbody>${rows}</tbody>
            </table>`;
    }

    _buildLatencySeriesChart(points, bucketSecs) {
        if (!Array.isArray(points) || !points.length) {
            return `<h3>Latency over time</h3><div class="empty-state-hint">No latency data in this window.</div>`;
        }

        const bucketMap = new Map();
        for (const p of points) {
            const ts = p.timestamp;
            const n = p.count || 1;
            const existing = bucketMap.get(ts) || { timestamp: ts, count: 0, sum_avg: 0, max_p95: 0, details: [] };
            existing.count += n;
            existing.sum_avg += (p.avg_ms || 0) * n;
            existing.max_p95 = Math.max(existing.max_p95, p.p95_ms || 0);
            existing.details.push(p);
            bucketMap.set(ts, existing);
        }
        const buckets = Array.from(bucketMap.values())
            .sort((a, b) => a.timestamp - b.timestamp)
            .map(b => ({ ...b, avg_ms: b.count > 0 ? b.sum_avg / b.count : 0 }));

        const maxVal = buckets.reduce((m, b) => Math.max(m, b.max_p95), 0);
        if (maxVal === 0) return `<h3>Latency over time</h3><div class="empty-state-hint">No latency data in this window.</div>`;

        const width = 100, barGap = 0.5, chartHeight = 100;
        const barWidth = Math.max((width - barGap * (buckets.length - 1)) / buckets.length, 0.1);

        const bars = buckets.map((b, i) => {
            const x = i * (barWidth + barGap);
            const p95H = (b.max_p95 / maxVal) * chartHeight;
            const avgH = Math.min((b.avg_ms / maxVal) * chartHeight, p95H);
            const tsDate = new Date(b.timestamp / 1_000_000);
            const modelLines = b.details.map(d =>
                `  ${d.model || d.name || '(all)'}: avg ${Math.round(d.avg_ms)}ms · p95 ${d.p95_ms}ms · ${d.count} calls`
            ).join('\n');
            const tip = `${formatTs(tsDate)}\navg ${Math.round(b.avg_ms)}ms  p95 ${b.max_p95}ms\n${b.count} calls\n${modelLines}`;
            const p95Rect = `<rect class="latency-chart-bar-p95" x="${x.toFixed(3)}" y="${(chartHeight - p95H).toFixed(3)}" width="${barWidth.toFixed(3)}" height="${p95H.toFixed(3)}"><title>${this._esc(tip)}</title></rect>`;
            const avgRect = avgH > 0
                ? `<rect class="latency-chart-bar-avg" x="${x.toFixed(3)}" y="${(chartHeight - avgH).toFixed(3)}" width="${barWidth.toFixed(3)}" height="${avgH.toFixed(3)}"><title>${this._esc(tip)}</title></rect>`
                : '';
            return p95Rect + avgRect;
        }).join('');

        const labelFor = i => {
            const d = new Date(buckets[i].timestamp / 1_000_000);
            const pad = n => String(n).padStart(2, '0');
            if ((bucketSecs || 3600) >= 86400) return `${d.getFullYear()}-${pad(d.getMonth()+1)}-${pad(d.getDate())}`;
            return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
        };
        let axisHtml = '';
        if (buckets.length > 0) {
            const left = this._esc(labelFor(0));
            const mid = buckets.length > 2 ? this._esc(labelFor(Math.floor(buckets.length / 2))) : '';
            const right = buckets.length > 1 ? this._esc(labelFor(buckets.length - 1)) : '';
            axisHtml = `<div class="cost-chart-axis-labels">
                <span class="cost-chart-axis-left">${left}</span>
                <span class="cost-chart-axis-mid">${mid}</span>
                <span class="cost-chart-axis-right">${right}</span>
            </div>`;
        }
        const peakP95 = buckets.reduce((m, b) => Math.max(m, b.max_p95), 0);
        return `
            <h3>Latency over time — peak p95 ${peakP95.toLocaleString()} ms</h3>
            <p class="table-hint">Solid bar = avg; faded extension = p95. Hover for per-model breakdown.</p>
            <div class="cost-chart">
                <svg class="cost-chart-svg" viewBox="0 0 ${width} ${chartHeight}" preserveAspectRatio="none">
                    ${bars}
                </svg>
                ${axisHtml}
            </div>`;
    }

    _buildLatencyByContext(bins) {
        if (!bins || !bins.length) return '';
        const fmt = n => Number(n).toLocaleString();
        const rows = bins.map(b => {
            const ttft = b.avg_ttft_ms != null ? this._formatDuration(b.avg_ttft_ms) : '—';
            return `
                <tr>
                    <td>${this._esc(b.bin)}</td>
                    <td>${this._esc(b.model || '—')}</td>
                    <td>${fmt(b.count || 0)}</td>
                    <td>${this._esc(this._formatDuration(b.avg_ms))}</td>
                    <td>${this._esc(this._formatDuration(b.p95_ms))}</td>
                    <td>${this._esc(this._formatDuration(b.max_ms))}</td>
                    <td>${this._esc(ttft)}</td>
                </tr>`;
        }).join('');
        return `
            <h3>Latency by context size</h3>
            <p class="table-hint">Response time broken down by prompt token count × model. Helps identify whether larger contexts cause slowdowns.</p>
            <table class="data-table">
                <thead><tr>
                    <th>Context bin (input tokens)</th><th>Model</th><th>Calls</th>
                    <th>Avg</th><th>P95</th><th>Max</th><th>TTFT avg</th>
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

    _buildErrorTypes(rows) {
        if (!rows || !rows.length) return '';
        const sorted = [...rows].sort((a, b) => (b.count || 0) - (a.count || 0));
        const bucketColors = {
            rate_limit: '#e74c3c',
            timeout: '#e67e22',
            context_length: '#f39c12',
            content_filter: '#9b59b6',
            auth: '#c0392b',
            server_error: '#e74c3c',
            unknown: '#95a5a6',
        };
        const tableRows = sorted.map(r => {
            const color = bucketColors[r.bucket] || '#95a5a6';
            return `
                <tr>
                    <td><span class="bucket-chip" style="background:${color};color:#fff;padding:2px 6px;border-radius:3px;font-size:0.85em">${this._esc(r.bucket)}</span></td>
                    <td title="${this._esc(r.error_type)}">${this._esc(r.error_type.length > 40 ? r.error_type.slice(0, 40) + '…' : r.error_type)}</td>
                    <td>${this._esc(r.model || '—')}</td>
                    <td>${r.count || 0}</td>
                </tr>`;
        }).join('');
        return `
            <h3>Error type breakdown</h3>
            <table class="data-table">
                <thead><tr>
                    <th>Bucket</th><th>Error Type</th><th>Model</th><th>Count</th>
                </tr></thead>
                <tbody>${tableRows}</tbody>
            </table>`;
    }

    _buildModelDrift(rows) {
        if (!rows || !rows.length) return '';
        const drifted = rows.filter(r => r.differs);
        if (!drifted.length) {
            return `<h3>Model drift</h3><p class="empty-state-hint">No model drift detected — request and response models match for all calls.</p>`;
        }
        const tableRows = drifted.map(r => `
            <tr class="drift-warning">
                <td>${this._esc(r.request_model || '—')}</td>
                <td>⚠ ${this._esc(r.response_model || '—')}</td>
                <td>${r.count || 0}</td>
            </tr>`).join('');
        return `
            <h3>Model drift — provider rerouted to a different model</h3>
            <table class="data-table">
                <thead><tr>
                    <th>Requested</th><th>Served</th><th>Count</th>
                </tr></thead>
                <tbody>${tableRows}</tbody>
            </table>`;
    }

    _buildCostChart(costSeries, bucketSecs) {
        if (!costSeries.length) {
            return `<h3>Cost over time</h3><div class="empty-state-hint">No cost data in this window.</div>`;
        }

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

    // ── Top-N section: dropdown-driven (replaces the old 8-tab switcher) ─────

    _buildTopNSection(topSpans, errorRate) {
        const sortOptions = [
            { id: 'cost',      label: 'Most expensive' },
            { id: 'slow',      label: 'Slowest' },
            { id: 'truncated', label: 'Truncated' },
            { id: 'sessions',  label: 'Sessions' },
            { id: 'convs',     label: 'Conversations' },
            { id: 'verbose',   label: 'Most verbose' },
            { id: 'cache',     label: 'Cache efficiency' },
            { id: 'errors',    label: 'Error runs' },
        ];

        const opts = sortOptions.map(o =>
            `<option value="${o.id}"${o.id === this.topNSort ? ' selected' : ''}>${o.label}</option>`
        ).join('');

        let initialContent = '';
        if (this.topNSort === 'cost') {
            initialContent = this._renderSpanTable(topSpans || [], { extraCol: 'cost', emptyMsg: 'No expensive calls in this window.' });
        } else if (this.topNSort === 'errors') {
            initialContent = this._renderErrorRunsTable(errorRate || []);
        } else {
            initialContent = `<div class="empty-state-hint">Loading…</div>`;
        }

        // Cache the initial cost data so the dropdown handler can return to it
        // without an extra fetch.
        this._topNCostCache = topSpans || [];
        this._topNErrorCache = errorRate || [];

        return `
            <div class="top-n-section">
                <h3>Top 20 calls
                    <select id="top-n-sort" class="filter-select" style="margin-left:0.75rem;font-size:0.85em">${opts}</select>
                </h3>
                <div id="top-n-content">${initialContent}</div>
            </div>`;
    }

    _attachTopNDropdownHandler(params) {
        const sel = document.getElementById('top-n-sort');
        if (!sel) return;
        sel.addEventListener('change', async (e) => {
            this.topNSort = e.target.value;
            const content = document.getElementById('top-n-content');
            if (!content) return;

            if (this.topNSort === 'cost') {
                content.innerHTML = this._renderSpanTable(this._topNCostCache, { extraCol: 'cost', emptyMsg: 'No expensive calls in this window.' });
                return;
            }
            if (this.topNSort === 'errors') {
                content.innerHTML = this._renderErrorRunsTable(this._topNErrorCache);
                return;
            }

            content.innerHTML = `<div class="empty-state-hint">Loading…</div>`;
            const fetchers = {
                slow:      p => this.api.getTopSpans({...p, sort_by: 'duration'}),
                truncated: p => this.api.getTopSpans({...p, truncated_only: true}),
                sessions:  p => this.api.getTopSessions(p),
                convs:     p => this.api.getTopConversations(p),
                verbose:   p => this.api.getTopSpans({...p, sort_by: 'output_input_ratio'}),
                cache:     p => this.api.getTopSpans({...p, sort_by: 'cache_efficiency'}),
            };
            try {
                const data = await fetchers[this.topNSort]({ ...params, limit: 20 });
                let html;
                if (this.topNSort === 'sessions') {
                    html = this._renderGroupTable(data || [], 'session_id', 'Session ID');
                } else if (this.topNSort === 'convs') {
                    html = this._renderGroupTable(data || [], 'conversation_id', 'Conversation ID');
                } else {
                    const extraCol = {slow: 'duration', truncated: 'finish_reason', verbose: 'ratio', cache: 'cache_rate'}[this.topNSort] || 'cost';
                    html = this._renderSpanTable(data || [], { extraCol, emptyMsg: 'No matching spans in this window.' });
                }
                content.innerHTML = html;
            } catch (err) {
                content.innerHTML = `<div class="empty-state-hint">Failed to load: ${this._esc(err.message)}</div>`;
            }
        });
    }

    /**
     * Compute a cache-state label for a span row.
     * COLD  — cache_read=0 and cache_creation>50K (full context rebuild)
     * WARMING — cache_read present but <50% of token budget
     * HOT   — cache_read>80% of (cache_read + cache_creation + input_tokens)
     */
    _cacheStateLabel(row) {
        const read   = row.cache_read_tokens     || 0;
        const create = row.cache_creation_tokens || 0;
        const input  = row.input_tokens          || 0;
        const total  = read + create + input;
        if (total === 0) return null;
        if (read === 0 && create > 50_000) return 'cold';
        const hitPct = read / total;
        if (hitPct >= 0.8) return 'hot';
        if (hitPct >= 0.3) return 'warming';
        if (read === 0) return null;   // small request, no cache signal
        return 'warming';
    }

    _renderSpanTable(spans, { extraCol, emptyMsg }) {
        if (!spans.length) return `<div class="empty-state-hint">${emptyMsg}</div>`;
        const fmt = n => Number(n).toLocaleString();
        const anySession = spans.some(r => r.session_id);
        // Show cache column whenever we have cache token data on any row
        const anyCacheData = spans.some(r => (r.cache_creation_tokens || 0) + (r.cache_read_tokens || 0) > 0);

        const extraHeader = {
            cost:         '<th>Cost</th>',
            duration:     '<th>Duration</th>',
            finish_reason:'<th>Finish reason</th>',
            ratio:        '<th>Out/In ratio</th>',
            cache_rate:   '<th>Cache hit%</th>',
        }[extraCol] || '';

        const rows = spans.map(row => {
            const cost = row.cost ?? null;
            const costStr = cost === null
                ? `<span title="${this._esc(row.cost_reason || 'no pricing match')}">—</span>`
                : `$${cost.toFixed(4)}`;
            const costClass = cost !== null && cost >= 0.01 ? 'top-spans-cost-high' : '';

            const timeStr = formatTs(new Date((row.start_time ?? 0) / 1_000_000));
            const sessionCell = row.session_id
                ? `<a href="#" onclick="window.app.navigateToLogsBySession('${this._esc(row.session_id)}'); return false;" title="${this._esc(row.session_id)}">${this._esc(String(row.session_id).slice(0, 8))}</a>`
                : '—';
            const traceCell = row.trace_id
                ? `<a href="#" onclick="window.app.navigateToTrace('${this._esc(row.trace_id)}'); return false;" title="${this._esc(row.trace_id)}">${this._esc(String(row.trace_id).slice(0, 8))}</a>`
                : '—';

            // Cache state badge
            const cacheState = anyCacheData ? this._cacheStateLabel(row) : null;
            const cacheLabels = {
                cold:    ['COLD',    'cache-state-cold',    'Full context rebuild — no cache reads, high creation cost'],
                warming: ['WARMING', 'cache-state-warming', 'Partial cache hit — context still filling'],
                hot:     ['HOT',     'cache-state-hot',     '>80% of tokens served from cache'],
            };
            const cacheBadge = cacheState
                ? (() => {
                    const [label, cls, tip] = cacheLabels[cacheState];
                    const read   = fmt(row.cache_read_tokens     || 0);
                    const create = fmt(row.cache_creation_tokens || 0);
                    return `<td><span class="cache-state-badge ${cls}" title="${tip}&#10;read: ${read} · created: ${create}">${label}</span></td>`;
                })()
                : (anyCacheData ? '<td>—</td>' : '');

            let extraCell = '';
            if (extraCol === 'cost') {
                extraCell = `<td class="${costClass}">${costStr}</td>`;
            } else if (extraCol === 'duration') {
                const ms = Math.round((row.duration ?? 0) / 1_000_000);
                extraCell = `<td>${ms.toLocaleString()}ms</td>`;
            } else if (extraCol === 'finish_reason') {
                extraCell = `<td>${this._esc(row.finish_reason || '—')}</td>`;
            } else if (extraCol === 'ratio') {
                const inp = row.input_tokens || 0;
                const out = row.output_tokens || 0;
                const ratio = inp > 0 ? (out / inp).toFixed(2) : '—';
                extraCell = `<td>${ratio}</td>`;
            } else if (extraCol === 'cache_rate') {
                const inp = (row.input_tokens || 0) + (row.cache_read_tokens || 0);
                const pct = inp > 0 ? ((row.cache_read_tokens || 0) / inp * 100).toFixed(1) : '—';
                extraCell = `<td>${pct}%</td>`;
            }

            return `<tr>
                <td>${this._esc(timeStr)}</td>
                <td>${this._esc(row.model || '—')}</td>
                ${anySession ? `<td>${sessionCell}</td>` : ''}
                <td class="num">${fmt(row.input_tokens ?? 0)}</td>
                <td class="num">${fmt(row.output_tokens ?? 0)}</td>
                ${anyCacheData ? cacheBadge : ''}
                ${extraCell}
                <td>${traceCell}</td>
            </tr>`;
        }).join('');

        return `<table class="data-table">
            <thead><tr>
                <th>Time</th><th>Model</th>
                ${anySession ? '<th>Session</th>' : ''}
                <th>Input</th><th>Output</th>
                ${anyCacheData ? '<th title="COLD = no cache reads, full context rebuild. WARMING = partial hit. HOT = >80% from cache.">Cache</th>' : ''}
                ${extraHeader}
                <th>Trace</th>
            </tr></thead>
            <tbody>${rows}</tbody>
        </table>`;
    }

    _renderGroupTable(rows, idField, idLabel) {
        const fmt = n => Number(n).toLocaleString();
        if (!rows.length) return `<div class="empty-state-hint">No data in this window.</div>`;
        const tableRows = rows.map(r => {
            const cost = r.cost ?? null;
            const costStr = cost === null ? '—' : `$${cost.toFixed(4)}`;
            const id = String(r[idField] || '—');
            const navFn = idField === 'session_id'
                ? `window.app.navigateToLogsBySession('${this._esc(id)}')`
                : `window.app.navigateToLogsBySession('${this._esc(id)}')`;
            const idCell = id === '—' ? id
                : `<a href="#" onclick="${navFn}; return false;" title="${this._esc(id)}">${this._esc(id.slice(0, 24))}${id.length > 24 ? '…' : ''}</a>`;
            return `<tr>
                <td>${idCell}</td>
                <td>${fmt(r.request_count ?? 0)}</td>
                <td>${fmt(r.input_tokens ?? 0)}</td>
                <td>${fmt(r.output_tokens ?? 0)}</td>
                <td>${costStr}</td>
            </tr>`;
        }).join('');
        return `<table class="data-table">
            <thead><tr>
                <th>${idLabel}</th><th>Requests</th><th>Input</th><th>Output</th><th>Cost (est.)</th>
            </tr></thead>
            <tbody>${tableRows}</tbody>
        </table>`;
    }

    _renderErrorRunsTable(errorRate) {
        const fmt = n => Number(n).toLocaleString();
        if (!errorRate.length) return `<div class="empty-state-hint">No error data in this window.</div>`;
        const rows = [...errorRate]
            .sort((a, b) => (b.error_rate ?? 0) - (a.error_rate ?? 0))
            .map(r => {
                const pct = ((r.error_rate ?? 0) * 100).toFixed(1);
                const cls = (r.error_rate ?? 0) > 0.1 ? 'top-spans-cost-high' : '';
                return `<tr>
                    <td>${this._esc(r.model || '—')}</td>
                    <td>${fmt(r.total_calls ?? 0)}</td>
                    <td>${fmt(r.error_count ?? 0)}</td>
                    <td class="${cls}">${pct}%</td>
                </tr>`;
            }).join('');
        return `<table class="data-table">
            <thead><tr>
                <th>Model</th><th>Calls</th><th>Errors</th><th>Error rate</th>
            </tr></thead>
            <tbody>${rows}</tbody>
        </table>`;
    }

    _buildFinishReasons(reasons) {
        if (!reasons.length) {
            return `<h3>Stop reasons</h3><div class="empty-state-hint">No finish-reason data in this window.</div>`;
        }
        const total = reasons.reduce((acc, r) => acc + (r.count || 0), 0);
        const sorted = [...reasons].sort((a, b) => (b.count || 0) - (a.count || 0));

        const LABELS = {
            end_turn:   'end_turn — completed normally',
            max_tokens: 'max_tokens — truncated (hit token limit)',
            length:     'length — truncated (hit token limit)',
            stop_sequence: 'stop_sequence — stopped by stop token',
            tool_use:   'tool_use — paused for tool call',
        };

        const truncatedCount = reasons
            .filter(r => ['max_tokens','length'].includes(String(r.reason).toLowerCase()))
            .reduce((acc, r) => acc + (r.count || 0), 0);
        const truncatedPct = total > 0 ? (truncatedCount / total * 100) : 0;
        const truncatedBanner = truncatedCount > 0
            ? `<div class="finish-reason-warning-banner">⚠ ${Number(truncatedCount).toLocaleString()} truncated responses (${truncatedPct.toFixed(1)}%) — context window hit limit</div>`
            : '';

        const rows = sorted.map(r => {
            const count = r.count || 0;
            const pct = total > 0 ? (count / total) * 100 : 0;
            const reason = String(r.reason || 'unknown');
            const warning = ['max_tokens','length'].includes(reason.toLowerCase());
            const label = LABELS[reason.toLowerCase()] || reason;
            return `
                <div class="finish-reason-row">
                    <div class="finish-reason-name${warning ? ' warning-text' : ''}">${this._esc(label)}</div>
                    <div class="finish-reason-bar"><div class="finish-reason-fill ${warning ? 'warning' : ''}" style="width:${pct.toFixed(2)}%"></div></div>
                    <div class="finish-reason-count">${Number(count).toLocaleString()} (${pct.toFixed(1)}%)</div>
                </div>`;
        }).join('');

        return `
            <h3>Stop reasons</h3>
            ${truncatedBanner}
            <div class="finish-reasons-list">${rows}</div>`;
    }

    _buildTruncationRate(rows) {
        const meaningful = rows.filter(r => (r.truncated || 0) > 0);
        if (!meaningful.length) return '';
        const fmt = n => Number(n).toLocaleString();
        const tableRows = rows.map(r => {
            const rate = (r.rate || 0) * 100;
            let colorClass = 'trunc-rate-green';
            if (rate >= 5) colorClass = 'trunc-rate-red';
            else if (rate >= 1) colorClass = 'trunc-rate-yellow';
            return `
                <tr>
                    <td>${this._esc(r.model || '—')}</td>
                    <td>${fmt(r.total || 0)}</td>
                    <td>${fmt(r.truncated || 0)}</td>
                    <td class="${colorClass}">${rate.toFixed(1)}%</td>
                </tr>`;
        }).join('');
        return `
            <h3>Truncation rate by model</h3>
            <table class="data-table">
                <thead><tr>
                    <th>Model</th><th>Total calls</th><th>Truncated</th><th>Rate</th>
                </tr></thead>
                <tbody>${tableRows}</tbody>
            </table>`;
    }

    _buildCacheHitRate(rows) {
        const meaningful = rows.filter(r => (r.total_cache_read_tokens || 0) > 0);
        if (!meaningful.length) return '';
        const fmt = n => Number(n).toLocaleString();
        const tableRows = rows.map(r => {
            const rate = (r.hit_rate || 0) * 100;
            let colorClass = 'cache-rate-grey';
            if (rate >= 20) colorClass = 'cache-rate-green';
            else if (rate >= 5) colorClass = 'cache-rate-yellow';
            return `
                <tr>
                    <td>${this._esc(r.model || '—')}</td>
                    <td>${fmt(r.total_input_tokens || 0)}</td>
                    <td>${fmt(r.total_cache_read_tokens || 0)}</td>
                    <td>${fmt(r.total_cache_creation_tokens || 0)}</td>
                    <td class="${colorClass}">${rate.toFixed(1)}%</td>
                </tr>`;
        }).join('');
        return `
            <h3>Cache hit rate by model</h3>
            <table class="data-table">
                <thead><tr>
                    <th>Model</th><th>Input tokens</th><th>Cache read</th><th>Cache created</th><th>Hit rate</th>
                </tr></thead>
                <tbody>${tableRows}</tbody>
            </table>`;
    }

    _buildRequestParamProfile(profile) {
        if (!profile) return '';
        const tempBuckets = Array.isArray(profile.temperature_buckets) ? profile.temperature_buckets : [];
        const maxTokBuckets = Array.isArray(profile.max_tokens_buckets) ? profile.max_tokens_buckets : [];
        const distinctTemps = new Set(tempBuckets.map(b => b.temperature)).size;
        const distinctMaxToks = new Set(maxTokBuckets.map(b => b.max_tokens)).size;
        if (distinctTemps <= 1 && distinctMaxToks <= 1) return '';

        const fmt = n => Number(n).toLocaleString();

        const tempRows = tempBuckets.map(b => `
            <tr>
                <td>${b.temperature == null ? '<em>not set</em>' : this._esc(String(b.temperature))}</td>
                <td>${fmt(b.count || 0)}</td>
            </tr>`).join('');

        const maxTokRows = maxTokBuckets.map(b => `
            <tr>
                <td>${b.max_tokens == null ? '<em>not set</em>' : this._esc(String(b.max_tokens))}</td>
                <td>${fmt(b.count || 0)}</td>
            </tr>`).join('');

        const tempTable = distinctTemps > 1 ? `
            <div class="param-profile-table">
                <h4>Temperature distribution</h4>
                <table class="data-table">
                    <thead><tr><th>Temperature</th><th>Count</th></tr></thead>
                    <tbody>${tempRows}</tbody>
                </table>
            </div>` : '';

        const maxTokTable = distinctMaxToks > 1 ? `
            <div class="param-profile-table">
                <h4>Max tokens distribution</h4>
                <table class="data-table">
                    <thead><tr><th>Max tokens</th><th>Count</th></tr></thead>
                    <tbody>${maxTokRows}</tbody>
                </table>
            </div>` : '';

        return `
            <h3>Request parameters</h3>
            <div class="param-profile-container">${tempTable}${maxTokTable}</div>`;
    }

    _buildConversationDepthCard(depth) {
        if (!depth || !depth.total_conversations) return '';
        const fmt = n => Number(n).toLocaleString();
        const avg = depth.avg_turns != null ? Number(depth.avg_turns).toFixed(1) : '—';
        return `
                <div class="usage-card">
                    <div class="usage-card-label">Conversations</div>
                    <div class="usage-card-value">${fmt(depth.total_conversations)}</div>
                    <div class="gauge-hint">avg ${avg} turns · p50 ${depth.p50_turns ?? '—'} · p95 ${depth.p95_turns ?? '—'}</div>
                </div>`;
    }

    _buildCallsChart(callsSeries) {
        if (!Array.isArray(callsSeries) || !callsSeries.length) {
            return `<h3>Request volume over time</h3><div class="empty-state-hint">No request data in this window.</div>`;
        }

        const bucketMap = new Map();
        for (const row of callsSeries) {
            const ts = row.timestamp;
            bucketMap.set(ts, (bucketMap.get(ts) || 0) + (row.requests || 0));
        }
        const buckets = Array.from(bucketMap.entries())
            .sort((a, b) => a[0] - b[0])
            .map(([timestamp, requests]) => ({ timestamp, requests }));

        const totalRequests = buckets.reduce((a, b) => a + b.requests, 0);
        const maxRequests = buckets.reduce((a, b) => Math.max(a, b.requests), 0);

        const width = 100;
        const barGap = 0.5;
        const barWidth = buckets.length > 0 ? Math.max((width - barGap * (buckets.length - 1)) / buckets.length, 0.1) : 0;
        const chartHeight = 100;

        const bars = buckets.map((b, i) => {
            const h = maxRequests > 0 ? (b.requests / maxRequests) * chartHeight : 0;
            const x = i * (barWidth + barGap);
            const y = chartHeight - h;
            const tsDate = new Date(b.timestamp / 1_000_000);
            const title = `${formatTs(tsDate)}\n${b.requests.toLocaleString()} requests`;
            return `<rect class="cost-chart-bar" x="${x.toFixed(3)}" y="${y.toFixed(3)}" width="${barWidth.toFixed(3)}" height="${h.toFixed(3)}"><title>${this._esc(title)}</title></rect>`;
        }).join('');

        const labelFor = i => {
            const d = new Date(buckets[i].timestamp / 1_000_000);
            const pad = n => String(n).padStart(2, '0');
            return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
        };

        let axisHtml = '';
        if (buckets.length > 0) {
            const left = this._esc(labelFor(0));
            const mid = buckets.length > 2 ? this._esc(labelFor(Math.floor(buckets.length / 2))) : '';
            const right = buckets.length > 1 ? this._esc(labelFor(buckets.length - 1)) : '';
            axisHtml = `
                <div class="cost-chart-axis-labels">
                    <span class="cost-chart-axis-left">${left}</span>
                    <span class="cost-chart-axis-mid">${mid}</span>
                    <span class="cost-chart-axis-right">${right}</span>
                </div>`;
        }

        return `
            <h3>Request volume over time — ${totalRequests.toLocaleString()} total across ${buckets.length} bucket${buckets.length === 1 ? '' : 's'}</h3>
            <div class="cost-chart">
                <svg class="cost-chart-svg" viewBox="0 0 ${width} ${chartHeight}" preserveAspectRatio="none">
                    ${bars}
                </svg>
                ${axisHtml}
            </div>`;
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

window.AnalyticsView = AnalyticsView;
