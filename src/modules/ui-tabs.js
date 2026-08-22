// src/modules/ui-tabs.js

export function setupTabs(onChange) {
  const tabs = document.querySelectorAll('.tab[data-target]');
  const contents = document.querySelectorAll('.tab-content');

  tabs.forEach((tab) => {
    tab.addEventListener('click', () => {
      tabs.forEach((t) => t.classList.remove('active'));
      contents.forEach((c) => c.classList.add('hidden'));

      tab.classList.add('active');
      const target = document.getElementById(tab.dataset.target);
      if (target) target.classList.remove('hidden');
      if (onChange) onChange(tab.dataset.target);
    });
  });
}
