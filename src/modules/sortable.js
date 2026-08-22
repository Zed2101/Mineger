// src/modules/sortable.js
//
// Riordino con drag & drop basato su pointer events (niente HTML5 DnD: in Tauri
// è riservato ai file). Un "ghost" segue il puntatore, l'elemento originale
// resta come placeholder e gli altri si spostano con animazioni FLIP.
// Il click normale resta intatto: il drag parte solo dopo `threshold` px.

const EASE = 'cubic-bezier(.2,.8,.2,1)';

/**
 * @param listEl   contenitore (scrollabile) degli item
 * @param opts     { itemSelector, groupOf(el), canDrag(), onReorder(group, ids), threshold }
 */
export function makeSortable(listEl, opts = {}) {
  const {
    itemSelector = '.sidebar-item',
    groupOf = (el) => el.dataset.group || '',
    canDrag = () => true,
    onReorder = null,
    threshold = 6,
  } = opts;

  let drag = null;

  function groupItems(group) {
    return [...listEl.querySelectorAll(itemSelector)].filter((el) => groupOf(el) === group);
  }

  function onDown(e) {
    if (e.button !== 0 || drag || !canDrag()) return;
    if (e.target.closest('button, input, a, [data-no-drag]')) return;
    const item = e.target.closest(itemSelector);
    if (!item || !listEl.contains(item)) return;

    drag = { item, startX: e.clientX, startY: e.clientY, active: false, group: groupOf(item) };
    document.addEventListener('pointermove', onMove);
    document.addEventListener('pointerup', onUp);
    document.addEventListener('pointercancel', onUp);
  }

  function startDrag(e) {
    const { item } = drag;
    const rect = item.getBoundingClientRect();
    const ghost = item.cloneNode(true);
    ghost.classList.add('drag-ghost');
    ghost.classList.remove('active');
    Object.assign(ghost.style, {
      position: 'fixed',
      left: `${rect.left}px`,
      top: `${rect.top}px`,
      width: `${rect.width}px`,
      height: `${rect.height}px`,
      margin: '0',
      zIndex: '1000',
      pointerEvents: 'none',
    });
    document.body.appendChild(ghost);

    item.classList.add('drag-placeholder');
    listEl.dataset.dragging = '1';
    document.body.classList.add('is-dragging');

    Object.assign(drag, { active: true, ghost, rect, offsetX: e.clientX - rect.left, offsetY: e.clientY - rect.top });
  }

  function moveGhost(e) {
    const dx = e.clientX - drag.offsetX - drag.rect.left;
    const dy = e.clientY - drag.offsetY - drag.rect.top;
    drag.ghost.style.transform = `translate(${dx}px, ${dy}px) scale(1.02)`;
  }

  function flip(others, first) {
    for (const el of others) {
      const delta = first.get(el) - el.getBoundingClientRect().top;
      if (!delta) continue;
      el.style.transition = 'none';
      el.style.transform = `translateY(${delta}px)`;
      el.getBoundingClientRect(); // forza il reflow
      el.style.transition = `transform 180ms ${EASE}`;
      el.style.transform = '';
      el.addEventListener('transitionend', () => { el.style.transition = ''; }, { once: true });
    }
  }

  function reorder(y) {
    const ph = drag.item;
    const items = groupItems(drag.group);
    const others = items.filter((el) => el !== ph);
    if (others.length === 0) return;

    let target = null;
    let before = true;
    for (const el of others) {
      const r = el.getBoundingClientRect();
      if (y < r.top + r.height / 2) {
        target = el;
        break;
      }
    }
    if (!target) {
      target = others[others.length - 1];
      before = false;
    }
    if (before && ph.nextElementSibling === target) return;
    if (!before && ph.previousElementSibling === target) return;

    const first = new Map(others.map((el) => [el, el.getBoundingClientRect().top]));
    if (before) target.before(ph);
    else target.after(ph);
    flip(others, first);
  }

  function autoScroll(y) {
    const r = listEl.getBoundingClientRect();
    const zone = 40;
    if (y < r.top + zone) listEl.scrollTop -= 10;
    else if (y > r.bottom - zone) listEl.scrollTop += 10;
  }

  function onMove(e) {
    if (!drag) return;
    if (!drag.active) {
      if (Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY) < threshold) return;
      startDrag(e);
    }
    e.preventDefault();
    moveGhost(e);
    reorder(e.clientY);
    autoScroll(e.clientY);
  }

  function onUp() {
    document.removeEventListener('pointermove', onMove);
    document.removeEventListener('pointerup', onUp);
    document.removeEventListener('pointercancel', onUp);
    const d = drag;
    drag = null;
    if (!d || !d.active) return;

    document.body.classList.remove('is-dragging');
    // Il click che segue il pointerup non deve selezionare: il flag sparisce al tick dopo.
    setTimeout(() => { delete listEl.dataset.dragging; }, 0);

    // Il ghost "atterra" sul placeholder
    const rect = d.item.getBoundingClientRect();
    const g = d.ghost;
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      g.remove();
      d.item.classList.remove('drag-placeholder');
      d.item.classList.add('drag-dropped');
      setTimeout(() => d.item.classList.remove('drag-dropped'), 260);
    };
    g.style.transition = `transform 160ms ${EASE}, opacity 160ms`;
    g.style.transform = `translate(${rect.left - d.rect.left}px, ${rect.top - d.rect.top}px) scale(1)`;
    g.addEventListener('transitionend', finish, { once: true });
    setTimeout(finish, 220);

    const ids = groupItems(d.group).map((el) => el.dataset.id);
    onReorder?.(d.group, ids);
  }

  listEl.addEventListener('pointerdown', onDown);

  return {
    isDragging: () => !!listEl.dataset.dragging,
    destroy() {
      listEl.removeEventListener('pointerdown', onDown);
    },
  };
}
