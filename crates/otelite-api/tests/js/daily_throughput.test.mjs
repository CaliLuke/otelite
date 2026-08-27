// Parity tests for the web daily-throughput table (issue #119 slice #144).
// Runs under `node --test` (no dependencies): asserts the rendered values,
// the confidence/missing-data wording, and the no-data state of
// AnalyticsView._buildDailyThroughputTable, mirroring the TUI's
// daily_throughput_rows tests.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
// analytics.js is a plain browser script (no imports); evaluate it in a
// sandbox without `window`/`module` leakage concerns.
const src = readFileSync(join(here, '../../static/js/analytics.js'), 'utf8');
const moduleObj = { exports: {} };
new Function('module', 'exports', 'window', 'parseHashQuery', 'parseHashWindow', src)(
    moduleObj,
    moduleObj.exports,
    undefined,
    () => ({}),
    () => null,
);
const { AnalyticsView } = moduleObj.exports;

// Bind the builder to a stub that reuses the real _esc, so the HTML under
// test is produced by the shipping code path.
const view = Object.create(AnalyticsView.prototype);

const point = (day, count, nStar, tps) => {
    const ts = Date.parse(`${day}T00:00:00Z`) * 1_000_000;
    return {
        timestamp: ts,
        count,
        throughput_sample_count: nStar,
        throughput_p10_tok_s: tps ? tps[0] : null,
        throughput_p50_tok_s: tps ? tps[1] : null,
        throughput_p90_tok_s: tps ? tps[2] : null,
    };
};

const resp = models => ({
    metrics: { duration: { all: [], models } },
});

test('renders values, weak-sample and missing wording', () => {
    const html = view._buildDailyThroughputTable(
        resp({
            alpha: [
                point('2026-08-24', 12, 12, [10.2, 20.4, 30.8]),
                point('2026-08-25', 7, 7, [5.0, 6.0, 7.0]),
                point('2026-08-26', 0, 0, null), // empty day -> omitted
            ],
            beta: [point('2026-08-24', 3, 0, null)],
        }),
        'UTC',
    );
    // Values, rounded like the CLI table.
    assert.match(html, /10 \/ 20 \/ 31/);
    assert.match(html, /5 \/ 6 \/ 7/);
    // Weak sample (n < 10) carries the dagger.
    assert.match(html, /7†/);
    // Calls present but no throughput-eligible calls -> em-dash cells.
    assert.match(html, /—/);
    // Empty day is omitted entirely.
    assert.doesNotMatch(html, /2026-08-26/);
    // Heading names the timezone and the derived-metric caveat is present.
    assert.match(html, /Output throughput by day \(UTC\)/);
    assert.match(html, /derived end-to-end output throughput per call/);
    assert.match(html, /span duration includes provider, queue and network time/);
});

test('no-data state', () => {
    const html = view._buildDailyThroughputTable(resp({ alpha: [point('2026-08-24', 0, 0, null)] }), 'UTC');
    assert.match(html, /No throughput data in this window\./);
    const empty = view._buildDailyThroughputTable(null, null);
    assert.match(empty, /No throughput data in this window\./);
});

test('partial triple renders as missing, not a partial value', () => {
    const html = view._buildDailyThroughputTable(
        resp({
            alpha: [
                {
                    timestamp: Date.parse('2026-08-24T00:00:00Z') * 1_000_000,
                    count: 12,
                    throughput_sample_count: 12,
                    throughput_p10_tok_s: 1,
                    throughput_p50_tok_s: 2,
                    throughput_p90_tok_s: null,
                },
            ],
        }),
        'UTC',
    );
    assert.match(html, /—/);
    assert.doesNotMatch(html, /1 \/ 2/);
});
