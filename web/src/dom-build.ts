/**
 * Safe DOM construction utilities — no innerHTML or hand-rolled escaping.
 *
 * All user-provided content is inserted via textContent, which is inherently
 * XSS-safe. The browser handles all encoding.
 */

/**
 * Create an element with attributes and children.
 * Text values are set via textContent (XSS-safe).
 */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  opts?: {
    class?: string
    text?: string
    html?: never // intentionally never — no innerHTML
    attrs?: Record<string, string>
    dataset?: Record<string, string>
    onclick?: () => void
  },
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag)
  if (opts?.class) node.className = opts.class
  if (opts?.text !== undefined) node.textContent = opts.text
  if (opts?.attrs) {
    for (const [k, v] of Object.entries(opts.attrs)) {
      node.setAttribute(k, v)
    }
  }
  if (opts?.dataset) {
    for (const [k, v] of Object.entries(opts.dataset)) {
      node.dataset[k] = v
    }
  }
  if (opts?.onclick) node.addEventListener("click", opts.onclick)
  return node
}

/**
 * Clear all children of an element.
 */
export function clearChildren(node: HTMLElement): void {
  while (node.firstChild) {
    node.removeChild(node.firstChild)
  }
}

/**
 * Render a list of children into a parent, replacing existing content.
 * Each child is either an HTMLElement or a string (rendered as text).
 */
export function renderChildren(
  parent: HTMLElement,
  children: (HTMLElement | string)[],
): void {
  clearChildren(parent)
  for (const child of children) {
    if (typeof child === "string") {
      parent.appendChild(document.createTextNode(child))
    } else {
      parent.appendChild(child)
    }
  }
}
