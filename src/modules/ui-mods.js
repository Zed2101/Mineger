import { formatBytes } from './utils.js';

export function renderModsList(mods) {
  const modsListEl = document.getElementById("mods-list");
  const modsHeaderEl = document.querySelector(".mods-header h3");

  const count = mods ? mods.length : 0;
  modsHeaderEl.textContent = `Mod Installate (${count})`;
  modsListEl.innerHTML = "";

  if (!mods || mods.length === 0) {
    modsListEl.innerHTML = `<li style="padding:20px; text-align:center; color:gray; list-style:none;">Nessuna mod installata</li>`;
    return;
  }

  mods.forEach(mod => {
    const li = document.createElement("li");
    li.className = "mod-item";
    const sizeFormatted = formatBytes(mod.size);

    li.innerHTML = `
      <span class="mod-name">${mod.name}</span>
      <span class="size">${sizeFormatted}</span>
    `;
    modsListEl.appendChild(li);
  });
}