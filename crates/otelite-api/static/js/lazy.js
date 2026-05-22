// IntersectionObserver-based lazy loader for charts and widgets.
//
// Usage:
//   import { lazyLoad } from './lazy.js';
//   lazyLoad(element, async () => { ... fetch + render ... });
//
// The callback fires once when `element` enters the viewport (with 100px
// rootMargin so it loads slightly before being visible). The observer
// disconnects after firing, so re-rendering the same element requires a
// fresh lazyLoad() call.

export function lazyLoad(element, fetchAndRender) {
    if (!element) return;
    if (!('IntersectionObserver' in window)) {
        // Fallback: fire immediately on platforms without IO support.
        Promise.resolve().then(fetchAndRender);
        return;
    }
    const obs = new IntersectionObserver((entries) => {
        for (const entry of entries) {
            if (entry.isIntersecting) {
                obs.disconnect();
                fetchAndRender();
                return;
            }
        }
    }, { rootMargin: '100px' });
    obs.observe(element);
}

// Convenience wrapper: lazyLoad multiple (element, callback) pairs.
export function lazyLoadAll(pairs) {
    for (const [el, cb] of pairs) lazyLoad(el, cb);
}

// Make available on window for non-module callers.
window.lazyLoad = lazyLoad;
window.lazyLoadAll = lazyLoadAll;
