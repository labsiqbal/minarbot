function (operation, stamp, value, submit, redacted) {
    const doc = document;
    const node = this;
    const validPage = location.protocol === "http:" || location.protocol === "https:" || location.href === "about:blank";
    const current = [location.href, globalThis.navigation?.currentEntry?.id ?? "", performance.timeOrigin];
    if (operation === "state") {
        return {visible: doc.visibilityState === "visible", focused: doc.hasFocus(), stamp: current, validPage};
    }
    if (!validPage || doc.visibilityState !== "visible" || stamp.some((part, index) => part !== current[index])) {
        return {stale: true};
    }
    if (node !== globalThis && (!node.isConnected || node.ownerDocument !== doc || !(node instanceof Element))) {
        return {stale: true};
    }

    const visible = () => node.getClientRects().length > 0 && getComputedStyle(node).visibility === "visible";
    const body = () => doc.body?.innerText ?? "";
    switch (operation) {
        case "verify": return {valid: true};
        case "visible": return {found: visible()};
        case "text": return {found: body().includes(value)};
        case "extract": {
            const text = node === globalThis ? body() : (node.innerText ?? "");
            return {text: text.slice(0, 8000).toWellFormed(), truncated: text.length > 8000};
        }
        case "clip": {
            if (!visible()) return {unavailable: true};
            const r = node.getBoundingClientRect();
            const x = Math.max(0, r.left);
            const y = Math.max(0, r.top);
            const width = Math.min(innerWidth, r.right) - x;
            const height = Math.min(innerHeight, r.bottom) - y;
            return width >= 1 && height >= 1 ? {x, y, width, height} : {unavailable: true};
        }
    }
    if (node.matches(":disabled") || !visible()) return {unavailable: true};

    if (operation === "click" && node instanceof HTMLElement) {
        node.scrollIntoView({block: "nearest", inline: "nearest"});
        node.click();
        return {done: true};
    }
    if (operation === "hover") {
        node.scrollIntoView({block: "nearest", inline: "nearest"});
        const r = node.getBoundingClientRect();
        const point = {clientX: r.left + r.width / 2, clientY: r.top + r.height / 2};
        node.dispatchEvent(new PointerEvent("pointerover", {...point, bubbles: true, pointerType: "mouse"}));
        node.dispatchEvent(new MouseEvent("mouseover", {...point, bubbles: true}));
        node.dispatchEvent(new MouseEvent("mouseenter", point));
        node.dispatchEvent(new MouseEvent("mousemove", {...point, bubbles: true}));
        return {done: true};
    }
    if (operation === "select" && node instanceof HTMLSelectElement) {
        const option = Array.from(node.options).find(o => o.value === value && !o.disabled && !o.parentElement?.matches("optgroup:disabled"));
        if (!option) return {unavailable: true};
        node.scrollIntoView({block: "nearest", inline: "nearest"});
        if (node.selectedIndex !== option.index) {
            node.selectedIndex = option.index;
            node.dispatchEvent(new Event("input", {bubbles: true, composed: true}));
            node.dispatchEvent(new Event("change", {bubbles: true}));
        }
        return {done: true};
    }
    if (operation === "type") {
        const input = node instanceof HTMLInputElement && ["text", "search", "url", "tel", "email", "password", "number"].includes(node.type);
        if ((!input && (redacted || !(node instanceof HTMLTextAreaElement || node.isContentEditable))) || node.readOnly) {
            return {unavailable: true};
        }
        node.focus();
        if (node.getRootNode().activeElement !== node || !node.isConnected) return {unavailable: true};
        if (!doc.execCommand("insertText", false, value)) return {unavailable: true};
        if (submit && node.isConnected) {
            if (node.form) HTMLFormElement.prototype.requestSubmit.call(node.form);
            else {
                const key = {key: "Enter", code: "Enter", bubbles: true, cancelable: true, composed: true};
                node.dispatchEvent(new KeyboardEvent("keydown", key));
                node.dispatchEvent(new KeyboardEvent("keyup", key));
            }
        }
        return {done: true};
    }
    return {unavailable: true};
}
