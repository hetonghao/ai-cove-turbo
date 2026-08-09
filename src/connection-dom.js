(() => {
  "use strict";

  const EXIT_TIMEOUT_MS = 180;
  const pendingRemovals = new WeakMap();

  function nodeKey(node) {
    return node?.nodeType === 1 ? node.getAttribute?.("data-connection-key") ?? "" : "";
  }

  function sameNode(current, next) {
    return current?.nodeType === next?.nodeType
      && (current.nodeType !== 1 || current.nodeName === next.nodeName);
  }

  function animateText(node) {
    const target = node?.parentNode;
    if (target?.nodeType !== 1 || !target.classList || target.classList.contains("is-text-updating")) return;
    target.classList.add("is-text-updating");
    target.addEventListener?.("animationend", () => target.classList.remove("is-text-updating"), { once: true });
  }

  function cancelRemoval(node) {
    const pending = pendingRemovals.get(node);
    if (!pending) return;
    node.removeEventListener("animationend", pending.onEnd);
    window.clearTimeout?.(pending.timer);
    pendingRemovals.delete(node);
    node.classList.remove("is-leaving");
    node.removeAttribute("aria-hidden");
    node.inert = false;
  }

  function removeNode(node) {
    if (!nodeKey(node) || !node.classList || !node.addEventListener) {
      node.remove();
      return;
    }
    if (pendingRemovals.has(node)) return;
    node.classList.add("is-leaving");
    node.setAttribute("aria-hidden", "true");
    node.inert = true;
    const finish = () => {
      const pending = pendingRemovals.get(node);
      if (pending) node.removeEventListener("animationend", pending.onEnd);
      pendingRemovals.delete(node);
      node.remove();
    };
    const onEnd = (event) => {
      if (event.target === node) finish();
    };
    const timer = window.setTimeout?.(finish, EXIT_TIMEOUT_MS);
    pendingRemovals.set(node, { onEnd, timer });
    node.addEventListener("animationend", onEnd);
  }

  function reconcileAttributes(current, next) {
    const nextAttributes = new Map(Array.from(next.attributes ?? [], (attribute) => [attribute.name, attribute.value]));
    Array.from(current.attributes ?? []).forEach((attribute) => {
      if (current.nodeName === "DETAILS" && attribute.name === "open") return;
      if (!nextAttributes.has(attribute.name)) current.removeAttribute(attribute.name);
    });
    nextAttributes.forEach((value, name) => {
      if (current.nodeName === "DETAILS" && name === "open") return;
      if (current.getAttribute(name) !== value) current.setAttribute(name, value);
    });
  }

  function reconcileNode(current, next) {
    if (current.nodeType === 3) {
      if (current.nodeValue !== next.nodeValue) {
        current.nodeValue = next.nodeValue;
        animateText(current);
      }
      return;
    }
    reconcileAttributes(current, next);
    reconcileChildren(current, next);
  }

  function reconcileChildren(currentParent, nextParent) {
    const currentChildren = Array.from(currentParent.childNodes ?? []);
    const keyedChildren = new Map(currentChildren.map((child) => [nodeKey(child), child]).filter(([key]) => key));
    const used = new Set();
    let cursor = currentParent.firstChild;

    Array.from(nextParent.childNodes ?? []).forEach((nextChild) => {
      const key = nodeKey(nextChild);
      let current = key ? keyedChildren.get(key) : null;
      if (!current || !sameNode(current, nextChild)) {
        current = currentChildren.find((child) => !used.has(child) && !nodeKey(child) && sameNode(child, nextChild));
      }
      if (current) {
        used.add(current);
        cancelRemoval(current);
        if (current !== cursor) currentParent.insertBefore(current, cursor);
        reconcileNode(current, nextChild);
      } else {
        current = nextChild.cloneNode(true);
        currentParent.insertBefore(current, cursor);
        if (key && current.classList) current.classList.add("is-entering");
      }
      cursor = current.nextSibling;
    });

    currentChildren.filter((child) => !used.has(child)).forEach(removeNode);
  }

  function reconcileList(list, markup) {
    const template = document.createElement?.("template");
    if (!template?.content) {
      if (list.innerHTML !== markup) list.innerHTML = markup;
      return;
    }
    template.innerHTML = markup;
    reconcileChildren(list, template.content);
  }

  window.TurboConnectionDOM = Object.freeze({ reconcileChildren, reconcileList });
})();
