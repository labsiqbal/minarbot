if (window !== window.top) {
  // Browser permissions for the configured page belong to its top-level document.
  Object.defineProperty(window, 'Notification', { value: undefined, configurable: false });
} else {
  window.addEventListener('click', (event) => {
    if (!event.isTrusted || event.defaultPrevented || event.button !== 0) return;
    const link = event.target instanceof Element && event.target.closest('a[href]');
    if (!link) return;
    const url = new URL(link.href, location.href);
    if (!['http:', 'https:'].includes(url.protocol) || url.origin === location.origin) return;
    event.preventDefault();
    window.open(url.href, '_blank', 'noopener,noreferrer');
  });
}
