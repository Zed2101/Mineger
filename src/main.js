import { getServers } from './modules/api.js';
import { setupTabs } from './modules/ui-tabs.js';
import { populatePropertiesPanel } from './modules/ui-properties.js';
import { renderModsList } from './modules/ui-mods.js';
import { setupModals } from './modules/ui-modals.js';

// --- GLOBAL STATE ---
// We pass this object to modules so they can read/write shared state
const state = {
  serverList: [],
  activeServerId: null
};

// --- DOM REF ---
const serverListEl = document.getElementById("server-list");
const loadingEl = document.getElementById("sidebar-loading");
const emptyStateEl = document.getElementById("empty-state");
const serverDetailsEl = document.getElementById("server-details");
const detailNameEl = document.getElementById("detail-name");
const detailVersionEl = document.getElementById("detail-version");
const detailStatusEl = document.getElementById("detail-status");
const detailIconImg = document.getElementById("detail-icon");

// --- MAIN LOGIC ---

function renderSidebar() {
  serverListEl.innerHTML = "";
  
  if (state.serverList.length === 0) {
     serverListEl.innerHTML = "<p style='text-align:center; color:gray; margin-top:20px;'>Nessun server trovato.</p>";
     return;
  }

  state.serverList.forEach(server => {
    const li = document.createElement("li");
    li.className = "server-item";
    const statusClass = server.status === 'online' ? 'online' : '';
    
    li.innerHTML = `
      <div class="server-info">
        <img src="./assets/${server.icon}" alt="${server.name} Icon" class="server-icon" />
        <div class="server-text">
          <h4>${server.name}</h4>
          <p>${server.version} • ${server.last_played}</p>
        </div>
      </div>
      <div class="status-dot ${statusClass}"></div>
    `;
    
    li.addEventListener("click", () => selectServer(server));
    serverListEl.appendChild(li);
  });
}

function selectServer(server) {
  state.activeServerId = server.id;

  emptyStateEl.classList.add("hidden");
  serverDetailsEl.classList.remove("hidden");

  // Update Banner
  updateBannerUI(server);

  // Delegate specific tabs to modules
  populatePropertiesPanel(server.properties);
  renderModsList(server.mods);
}

function updateBannerUI(server) {
  detailNameEl.textContent = server.name;
  detailVersionEl.textContent = server.version;
  detailIconImg.src = `./assets/${server.icon}`;
  
  if(server.status === 'online') {
    detailStatusEl.textContent = "ONLINE";
    detailStatusEl.style.color = "var(--success)";
  } else {
    detailStatusEl.textContent = "OFFLINE";
    detailStatusEl.style.color = "var(--danger)";
  }
}

async function initApp() {
  setupTabs();
  
  // Setup Modals passing state and refresh callbacks
  setupModals(state, renderSidebar, updateBannerUI);

  // Load Data
  serverListEl.classList.add("hidden"); 
  loadingEl.classList.remove("hidden");

  try {
    state.serverList = await getServers();
    console.log("Servers loaded:", state.serverList);
    renderSidebar();
  } catch (e) {
    serverListEl.innerHTML = `<div style='color:var(--danger); text-align:center;'>Errore: ${e}</div>`;
  } finally {
    loadingEl.classList.add("hidden");
    serverListEl.classList.remove("hidden");
  }
}

// Start
window.addEventListener("DOMContentLoaded", initApp);