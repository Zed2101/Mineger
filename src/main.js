const { invoke } = window.__TAURI__.core;
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

  updateBannerUI(server);
  populatePropertiesPanel(server.properties);
  renderModsList(server.mods);

  // --- NEW: Attach Start Button Listener ---
  const btnStart = document.querySelector('.btn-start');
  
  // Clone the button to remove old event listeners (simple hack)
  const newBtn = btnStart.cloneNode(true);
  btnStart.parentNode.replaceChild(newBtn, btnStart);
  
  newBtn.addEventListener('click', () => {
    requestStartServer(server.id);
  });
}

async function requestStartServer(serverId) {
  const btn = document.querySelector('.btn-start');
  if(!btn) return;

  const originalText = btn.textContent;
  btn.textContent = "Avvio in corso...";
  btn.disabled = true;
  btn.style.opacity = "0.7";

  try {
    const msg = await invoke('start_server', { id: serverId });
    console.log(msg);
    
    btn.textContent = "Server Avviato!";
    btn.style.backgroundColor = "var(--success)";
    
    // Reset button after 3 seconds
    setTimeout(() => {
      btn.textContent = "Stop Server"; // Should eventually change state
      btn.style.backgroundColor = "var(--danger)"; // Change to stop button
      btn.disabled = false;
      btn.style.opacity = "1";
    }, 3000);

  } catch (error) {
    console.error(error);
    btn.textContent = "Errore Avvio";
    btn.style.backgroundColor = "var(--danger)";
    alert("Errore avvio: " + error);
    
    setTimeout(() => {
      btn.textContent = originalText;
      btn.style.backgroundColor = "var(--success)"; // Reset color
      btn.disabled = false;
      btn.style.opacity = "1";
    }, 3000);
  }
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